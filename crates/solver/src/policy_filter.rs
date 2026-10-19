//! Dependency policies applied to the pool during updates: `SecurityAdvisoryPoolFilter` (advisories and
//! abandoned packages) and `FilterListPoolFilter` (the `malware` list and custom `policy.<name>` lists),
//! configured by a simplified `Policy\PolicyConfig`.

use crate::filter::{name_to_regex, names_to_regex};
use crate::pool::{Pool, Removed};
use crate::repository_set::{Advisories, matching_security_advisories, version_constraints};
use crate::request::Request;
use anyhow::{Result, bail};
use indexmap::IndexMap;
use komposer_package::config::Config;
use komposer_package::io::Io;
use komposer_package::loader::php_truthy;
use komposer_package::repository::{FilterEntry, Repository, SecurityAdvisory};
use komposer_package::{Package, PackageRef, is_platform_package};
use komposer_semver::{Constraint, parse_constraints};
use regex::Regex;
use serde_json::{Map, Value};
use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

/// `ListPolicyConfig::BLOCK_SCOPE_*`: which command a block applies to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockScope {
    /// update and require
    Update,
    Install,
}

/// The parts of `PolicyConfig` the pool filters use.
#[derive(Clone, Debug)]
pub struct PolicyOptions {
    /// `false` when `config.policy` is `false`.
    pub enabled: bool,
    /// Block versions affected by security advisories.
    pub advisories_block: bool,
    /// Advisory ids, CVE ids, remote ids or package names whose advisories do not block.
    pub advisories_ignore: Vec<String>,
    /// Block abandoned packages.
    pub abandoned_block: bool,
    /// Package name patterns (`*` wildcards) exempt from abandoned blocking.
    pub abandoned_ignore: Vec<String>,
    /// Block versions on the `malware` filter list.
    pub malware_block: bool,
    /// `None` blocks for all commands.
    pub malware_block_scope: Option<BlockScope>,
    /// Package name patterns with the constraint of versions exempt from malware blocking.
    pub malware_ignore: Vec<(String, Constraint)>,
    /// Sources whose malware entries are ignored (`ignore-source`).
    pub malware_ignore_source: Vec<String>,
    /// Custom `policy.<name>` filter lists (`CustomListPolicyConfig`), provided by repositories.
    pub custom_lists: Vec<CustomList>,
    /// Ignore advisory/filter sources that can not be reached, during updates and installs.
    pub ignore_unreachable_update: bool,
    pub ignore_unreachable_install: bool,
}

/// `CustomListPolicyConfig`: a named block list. Custom lists never block installs.
// ponytail: list `sources` (UrlSourceFilterListProvider) are not fetched; only repository-provided lists apply
#[derive(Clone, Debug)]
pub struct CustomList {
    pub name: String,
    pub block: bool,
    /// Package name patterns with the constraint of versions exempt from blocking.
    pub ignore: Vec<(String, Constraint)>,
}

/// `PolicyConfig::RESERVED_NAMES`, `FUTURE_RESERVED_NAMES` and the built-in and non-list keys.
const BUILTIN_LISTS: [&str; 3] = ["advisories", "abandoned", "malware"];
const NON_LIST_KEYS: [&str; 1] = ["ignore-unreachable"];
const FUTURE_RESERVED_NAMES: [&str; 10] =
    ["package", "packages", "license", "licence", "licenses", "licences", "support", "maintenance", "security", "minimum-release-age"];

impl Default for PolicyOptions {
    /// Composer's defaults: advisories and malware block, abandoned packages do not.
    fn default() -> PolicyOptions {
        PolicyOptions {
            enabled: true,
            advisories_block: true,
            advisories_ignore: vec![],
            abandoned_block: false,
            abandoned_ignore: vec![],
            malware_block: true,
            malware_block_scope: None,
            malware_ignore: vec![],
            malware_ignore_source: vec![],
            custom_lists: vec![],
            ignore_unreachable_update: true,
            ignore_unreachable_install: true,
        }
    }
}

impl PolicyOptions {
    /// `PolicyConfig::fromConfig()`: `policy.*` with the `audit.*` fallbacks, then the `COMPOSER_POLICY_*` env overrides.
    pub fn from_config(config: &Config) -> Result<PolicyOptions> {
        let policy_raw = config.get("policy");
        if policy_raw == Some(Value::Bool(false)) {
            return Ok(PolicyOptions { enabled: false, advisories_block: false, malware_block: false, ..PolicyOptions::default() });
        }
        let empty = Map::new();
        let policy = policy_raw.as_ref().and_then(Value::as_object).unwrap_or(&empty);
        let audit_raw = config.get("audit");
        let audit = audit_raw.as_ref().and_then(Value::as_object).unwrap_or(&empty);
        let mut options = PolicyOptions::default();
        let isset = |m: &Map<String, Value>, k: &str| m.get(k).filter(|v| !v.is_null()).cloned();

        // advisories
        match isset(policy, "advisories") {
            None if !audit.is_empty() => {
                options.advisories_block = isset(audit, "block-insecure").is_none_or(|v| php_truthy(&v));
                options.advisories_ignore = legacy_ignores(audit.get("ignore"))?;
            }
            Some(Value::Bool(false)) => options.advisories_block = false,
            advisories => {
                let advisories = advisories.and_then(|v| v.as_object().cloned()).unwrap_or_default();
                options.advisories_block = isset(&advisories, "block").is_none_or(|v| php_truthy(&v));
                options.advisories_ignore = ignore_id_map(advisories.get("ignore-id"))?;
                options.advisories_ignore.extend(ignore_map(advisories.get("ignore"))?.into_iter().map(|(name, _)| name));
            }
        }

        // abandoned
        match isset(policy, "abandoned") {
            None if !audit.is_empty() => {
                options.abandoned_block = isset(audit, "block-abandoned").is_some_and(|v| php_truthy(&v));
                options.abandoned_ignore = legacy_ignores(audit.get("ignore-abandoned"))?;
            }
            Some(Value::Bool(false)) => {}
            abandoned => {
                let abandoned = abandoned.and_then(|v| v.as_object().cloned()).unwrap_or_default();
                options.abandoned_block = isset(&abandoned, "block").is_some_and(|v| php_truthy(&v));
                options.abandoned_ignore = ignore_map(abandoned.get("ignore"))?.into_iter().map(|(name, _)| name).collect();
            }
        }

        // malware
        match isset(policy, "malware") {
            Some(Value::Bool(false)) => options.malware_block = false,
            malware => {
                let malware = malware.and_then(|v| v.as_object().cloned()).unwrap_or_default();
                options.malware_block = isset(&malware, "block").is_none_or(|v| php_truthy(&v));
                options.malware_block_scope = match isset(&malware, "block-scope").as_ref().and_then(Value::as_str) {
                    Some("update") => Some(BlockScope::Update),
                    Some("install") => Some(BlockScope::Install),
                    _ => None,
                };
                options.malware_ignore = ignore_map(malware.get("ignore"))?;
                let sources = malware.get("ignore-source").and_then(Value::as_array).into_iter().flatten();
                options.malware_ignore_source = sources.filter_map(|s| s.as_str().map(String::from)).collect();
            }
        }

        // custom lists
        for (name, list) in policy {
            if BUILTIN_LISTS.contains(&name.as_str()) || NON_LIST_KEYS.contains(&name.as_str()) {
                continue;
            }
            if name.starts_with("ignore") {
                bail!("Invalid custom dependency policy name: \"{name}\" starts with reserved prefix \"ignore\".");
            }
            if FUTURE_RESERVED_NAMES.contains(&name.as_str()) {
                bail!("Invalid custom dependency policy name: \"{name}\" is reserved for future use.");
            }
            let (block, ignore) = match list {
                Value::Bool(true) => (true, vec![]),
                Value::Object(m) => (m.get("block").filter(|v| !v.is_null()).is_none_or(php_truthy), ignore_map(m.get("ignore"))?),
                _ => (false, vec![]),
            };
            options.custom_lists.push(CustomList { name: name.clone(), block, ignore });
        }

        // ignore-unreachable
        if let Some(v) = isset(policy, "ignore-unreachable") {
            let scopes: Vec<&str> = v.as_array().map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
            (options.ignore_unreachable_install, options.ignore_unreachable_update) = match &v {
                Value::Array(_) => (scopes.contains(&"install"), scopes.contains(&"update")),
                v => (php_truthy(v), php_truthy(v)),
            };
        } else if isset(audit, "ignore-unreachable").is_some_and(|v| php_truthy(&v)) {
            (options.ignore_unreachable_install, options.ignore_unreachable_update) = (false, false);
        }

        if let Some(block) = bool_env("COMPOSER_POLICY_ADVISORIES_BLOCK")? {
            options.advisories_block = block;
        }
        if let Some(block) = bool_env("COMPOSER_POLICY_MALWARE_BLOCK")? {
            options.malware_block = block;
        }
        let abandoned = bool_env("COMPOSER_POLICY_ABANDONED_BLOCK")?;
        if let Some(block) = abandoned.map_or_else(|| bool_env("COMPOSER_SECURITY_BLOCKING_ABANDONED"), |b| Ok(Some(b)))? {
            options.abandoned_block = block;
        }
        Ok(options)
    }

    /// `withBlockingDisabled()`: `--no-blocking`, `COMPOSER_NO_BLOCKING`.
    pub fn without_blocking(mut self) -> PolicyOptions {
        self.advisories_block = false;
        self.abandoned_block = false;
        self.malware_block = false;
        for list in &mut self.custom_lists {
            list.block = false;
        }
        self
    }

    /// `getActiveBlockFilterLists($blockScope)`: list name => ignore rules.
    pub fn active_block_lists(&self, scope: BlockScope) -> Vec<(&str, &[(String, Constraint)])> {
        let mut lists: Vec<(&str, &[(String, Constraint)])> = vec![];
        if self.blocks_malware(scope) {
            lists.push(("malware", &self.malware_ignore));
        }
        if scope == BlockScope::Update {
            lists.extend(self.custom_lists.iter().filter(|l| l.block).map(|l| (l.name.as_str(), l.ignore.as_slice())));
        }
        lists
    }

    /// `MalwarePolicyConfig::shouldBlock()`.
    pub fn blocks_malware(&self, scope: BlockScope) -> bool {
        self.malware_block && self.malware_block_scope.is_none_or(|s| s == scope)
    }

    fn ignore_unreachable(&self, scope: BlockScope) -> bool {
        match scope {
            BlockScope::Update => self.ignore_unreachable_update,
            BlockScope::Install => self.ignore_unreachable_install,
        }
    }
}

/// `Platform::getBoolEnv()`.
fn bool_env(name: &str) -> Result<Option<bool>> {
    match std::env::var(name).ok().filter(|v| !v.is_empty()).as_deref() {
        None => Ok(None),
        Some("1" | "true" | "on") => Ok(Some(true)),
        Some("0" | "false" | "off") => Ok(Some(false)),
        Some(v) => bail!("Invalid value for {name}: {v}. Expected 0, 1, false, true, off, or on."),
    }
}

/// `parseLegacyAuditIgnore()` / `parseLegacyIgnoreWithApply()`: the ids or package names ignored on block.
fn legacy_ignores(config: Option<&Value>) -> Result<Vec<String>> {
    let entries: Vec<(Option<&String>, &Value)> = match config {
        Some(Value::Array(list)) => list.iter().map(|v| (None, v)).collect(),
        Some(Value::Object(map)) => map.iter().map(|(k, v)| (Some(k), v)).collect(),
        _ => vec![],
    };
    let mut out = vec![];
    for (key, value) in entries {
        let id = match (key, value) {
            (Some(k), _) => k.clone(),
            (None, Value::String(s)) => s.clone(),
            (None, v) => v.to_string(),
        };
        let on_block = match value {
            Value::Object(m) if key.is_some() => match m.get("apply").map(|a| a.as_str()).unwrap_or(Some("all")) {
                Some("all" | "block") => true,
                Some("audit") => false,
                other => bail!("Invalid 'apply' value for '{id}': {}. Expected 'audit', 'block', or 'all'.", other.unwrap_or("non-string")),
            },
            _ => true,
        };
        if on_block {
            out.push(id);
        }
    }
    Ok(out)
}

/// `IgnorePackageRule::parseIgnoreMap()`, keeping the rules that apply on block: (name pattern, constraint).
fn ignore_map(config: Option<&Value>) -> Result<Vec<(String, Constraint)>> {
    let mut rules = vec![];
    let rule = |name: &str, rule: &Map<String, Value>| -> Result<Option<(String, Constraint)>> {
        if rule.get("on-block").is_some_and(|v| !php_truthy(v)) {
            return Ok(None);
        }
        let constraint = match rule.get("constraint").filter(|c| !c.is_null()) {
            Some(c) => parse_constraints(c.as_str().unwrap_or_default())?,
            None => Constraint::Any,
        };
        Ok(Some((name.to_string(), constraint)))
    };
    match config {
        Some(Value::Array(list)) => {
            for name in list.iter().filter_map(Value::as_str) {
                rules.push((name.to_string(), Constraint::Any));
            }
        }
        Some(Value::Object(map)) => {
            for (name, value) in map {
                match value {
                    Value::Null | Value::String(_) => rules.push((name.clone(), Constraint::Any)),
                    Value::Array(list) => {
                        for r in list {
                            let Some(r) = r.as_object() else {
                                bail!("Invalid ignore rule for \"{name}\": expected an object.");
                            };
                            rules.extend(rule(name, r)?);
                        }
                    }
                    Value::Object(r) => rules.extend(rule(name, r)?),
                    _ => bail!(
                        "Invalid ignore entry at key \"{name}\": value is not a supported shape. Expected null, a reason string, a rule object, or a list of rule objects."
                    ),
                }
            }
        }
        _ => {}
    }
    Ok(rules)
}

/// `IgnoreIdRule::parseIgnoreIdMap()`, keeping the ids that apply on block.
fn ignore_id_map(config: Option<&Value>) -> Result<Vec<String>> {
    let mut ids = vec![];
    match config {
        Some(Value::Array(list)) => {
            for (i, v) in list.iter().enumerate() {
                let Some(id) = v.as_str() else {
                    bail!("Invalid ignore-id entry at index {i}: expected an advisory ID string.");
                };
                ids.push(id.to_string());
            }
        }
        Some(Value::Object(map)) => {
            for (id, v) in map {
                match v {
                    Value::Object(r) if r.get("on-block").is_some_and(|b| !php_truthy(b)) => {}
                    Value::Null | Value::String(_) | Value::Object(_) => ids.push(id.clone()),
                    _ => bail!(
                        "Invalid ignore-id entry for \"{id}\": value is not a supported shape. Expected null, a reason string, or a rule object."
                    ),
                }
            }
        }
        _ => {}
    }
    Ok(ids)
}

/// Root and platform packages are never filtered.
fn is_filterable(pool: &Pool, p: &Package) -> bool {
    !pool.is_root_package(p) && !is_platform_package(&p.name)
}

fn warn_unreachable(io: &Io, warning: &str, unreachable: &[String]) {
    if unreachable.is_empty() {
        return;
    }
    io.warning(warning);
    for repo in unreachable {
        io.write_error(&format!("  - {repo}"));
    }
}

/// `SecurityAdvisoryPoolFilter::filter()`.
pub fn security_advisory_filter(
    pool: Pool,
    repositories: &[Arc<dyn Repository>],
    request: &Request,
    policy: &PolicyOptions,
    io: &Io,
) -> Result<Pool> {
    if !policy.advisories_block {
        return Ok(pool);
    }
    let candidates: Vec<PackageRef> = pool.packages().iter().filter(|p| is_filterable(&pool, p) && !request.is_locked_package(p)).cloned().collect();

    let ignore = &policy.advisories_ignore;
    let ignore_unreachable = policy.ignore_unreachable_update;
    let mut all = matching_security_advisories(repositories, &candidates, true, ignore_unreachable)?;
    if needs_complete_advisory_load(&all, ignore) {
        all = matching_security_advisories(repositories, &candidates, false, ignore_unreachable)?;
    }
    if ignore_unreachable {
        let warning =
            "Security advisory data could not be fetched from some repositories (ignored per policy.ignore-unreachable); matches may be incomplete:";
        warn_unreachable(io, warning, &all.unreachable_repos);
    }
    let advisories = process_advisories(all.advisories, ignore);

    let ignore_abandoned = (!policy.abandoned_ignore.is_empty()).then(|| names_to_regex(&policy.abandoned_ignore));
    let mut packages = vec![];
    let mut security_removed: Removed<Vec<SecurityAdvisory>> = IndexMap::new();
    let mut abandoned_removed: Removed<String> = IndexMap::new();
    for p in pool.packages() {
        if policy.abandoned_block && p.is_abandoned() && ignore_abandoned.as_ref().is_none_or(|re| !re.is_match(&p.name)) {
            for name in p.names(false) {
                abandoned_removed.entry(name).or_default().insert(p.version.clone(), p.pretty_version.clone());
            }
            continue;
        }
        let matching = matching_advisories(p, &advisories);
        if !matching.is_empty() {
            for name in p.names(false) {
                security_removed.entry(name).or_default().insert(p.version.clone(), matching.clone());
            }
            continue;
        }
        packages.push(p.clone());
    }
    let mut filtered = pool.derive(packages);
    filtered.security_removed = security_removed;
    filtered.abandoned_removed = abandoned_removed;
    filtered.filter_list_removed = IndexMap::new();
    Ok(filtered)
}

/// `Auditor::needsCompleteAdvisoryLoad()`: partial advisories lack the CVE and remote ids that may be ignored.
fn needs_complete_advisory_load(all: &Advisories, ignore: &[String]) -> bool {
    !all.advisories.is_empty()
        && all.advisories.values().flatten().any(SecurityAdvisory::is_partial)
        && ignore.iter().any(|id| !id.starts_with("PKSA-"))
}

/// `Auditor::processAdvisories()`: the advisories that are not ignored.
fn process_advisories(all: BTreeMap<String, Vec<SecurityAdvisory>>, ignore: &[String]) -> BTreeMap<String, Vec<SecurityAdvisory>> {
    if ignore.is_empty() {
        return all;
    }
    let ignored = |id: &str| ignore.iter().any(|i| i == id);
    let mut active: BTreeMap<String, Vec<SecurityAdvisory>> = BTreeMap::new();
    for (package, advisories) in all {
        for a in advisories {
            let full_ignored = !a.is_partial()
                && (a.cve.as_deref().is_some_and(ignored)
                    || a.sources.iter().any(|s| s.get("remoteId").and_then(Value::as_str).is_some_and(ignored)));
            if !ignored(&package) && !ignored(&a.advisory_id) && !full_ignored {
                active.entry(package.clone()).or_default().push(a);
            }
        }
    }
    active
}

/// `getMatchingAdvisories()`: dev versions are never matched.
fn matching_advisories(p: &Package, advisories: &BTreeMap<String, Vec<SecurityAdvisory>>) -> Vec<SecurityAdvisory> {
    if p.is_dev() {
        return vec![];
    }
    let names = p.names(false);
    let candidates = names.iter().filter_map(|n| advisories.get(n)).flatten();
    candidates.filter(|a| a.affected_versions.matches_version(&p.version)).cloned().collect()
}

/// Filter list entries by package name.
type FilterMap = BTreeMap<String, Vec<Arc<FilterEntry>>>;

/// `FilterListPoolFilter::filter()`.
pub fn filter_list_filter(
    pool: Pool,
    repositories: &[Arc<dyn Repository>],
    request: &Request,
    policy: &PolicyOptions,
    scope: BlockScope,
    io: &Io,
) -> Result<Pool> {
    // during updates, locked packages are about to be installed so install-scope lists apply to them too
    let check_locked = scope == BlockScope::Update;
    let scope_lists = policy.active_block_lists(scope);
    let install_lists = if check_locked { policy.active_block_lists(BlockScope::Install) } else { vec![] };
    if scope_lists.is_empty() && install_lists.is_empty() {
        return Ok(pool);
    }
    let mut ignore_unreachable = policy.ignore_unreachable(scope);
    if check_locked {
        ignore_unreachable = ignore_unreachable && policy.ignore_unreachable(BlockScope::Install);
    }

    let mut list_names: Vec<String> = scope_lists.iter().chain(&install_lists).map(|(n, _)| n.to_string()).collect();
    list_names.dedup();
    let candidates: Vec<&PackageRef> = pool.packages().iter().filter(|p| is_filterable(&pool, p)).collect();
    let (map, unreachable) = collect_filter_lists(repositories, &candidates, &list_names, ignore_unreachable)?;
    let warning = "Filter list data could not be fetched from some sources (ignored per policy.ignore-unreachable); matches may be incomplete:";
    warn_unreachable(io, warning, &unreachable);
    if map.is_empty() {
        return Ok(pool);
    }

    let locked: HashSet<(&str, &str)> = if check_locked {
        request.locked_repository_packages().iter().map(|p| (p.name.as_str(), p.version.as_str())).collect()
    } else {
        HashSet::new()
    };
    let compile = |lists: &[(&str, &[(String, Constraint)])]| -> Vec<(String, Vec<(Regex, Constraint)>)> {
        lists.iter().map(|(n, ignore)| (n.to_string(), ignore.iter().map(|(p, c)| (name_to_regex(p), c.clone())).collect())).collect()
    };
    let (scope_active, install_active) = (compile(&scope_lists), compile(&install_lists));

    let mut packages = vec![];
    let mut removed: Removed<Vec<Arc<FilterEntry>>> = IndexMap::new();
    for p in pool.packages() {
        if !is_filterable(&pool, p) {
            packages.push(p.clone());
            continue;
        }
        let locked_equivalent = check_locked && (request.is_locked_package(p) || locked.contains(&(p.name.as_str(), p.version.as_str())));
        let active = if locked_equivalent { &install_active } else { &scope_active };
        let entries = matching_block_entries(p, &map, active, &policy.malware_ignore_source);
        if entries.is_empty() {
            packages.push(p.clone());
            continue;
        }
        for name in p.names(false) {
            removed.entry(name).or_default().insert(p.version.clone(), entries.clone());
        }
    }
    let mut filtered = pool.derive(packages);
    filtered.filter_list_removed = removed;
    Ok(filtered)
}

/// `FilterListProviderSet::getMatchingFilterLists()` + `FilterListAuditor::collectFilterLists()`.
fn collect_filter_lists(
    repositories: &[Arc<dyn Repository>],
    packages: &[&PackageRef],
    lists: &[String],
    ignore_unreachable: bool,
) -> Result<(FilterMap, Vec<String>)> {
    let constraints = version_constraints(packages.iter().copied());
    let mut map = FilterMap::new();
    let mut unreachable = vec![];
    for repo in repositories {
        let entries = match repo.filter_entries(&constraints, lists) {
            Ok(entries) => entries,
            Err(e) if ignore_unreachable => {
                unreachable.push(e.to_string());
                continue;
            }
            Err(e) => return Err(e),
        };
        for entry in entries {
            let matches = constraints.iter().any(|(n, c)| *n == entry.package_name && entry.constraint.matches(c));
            if lists.contains(&entry.list_name) && matches {
                map.entry(entry.package_name.clone()).or_default().push(Arc::new(entry));
            }
        }
    }
    Ok((map, unreachable))
}

/// `FilterListAuditor::getMatchingBlockEntries()`: entries of the active lists matching the package, minus
/// each list's ignore rules and, for malware, `ignore-source`.
fn matching_block_entries(
    p: &Package,
    map: &FilterMap,
    active: &[(String, Vec<(Regex, Constraint)>)],
    ignore_source: &[String],
) -> Vec<Arc<FilterEntry>> {
    let mut out = vec![];
    for name in p.names(false) {
        let Some(entries) = map.get(&name) else { continue };
        for (list, ignore) in active {
            if ignore.iter().any(|(re, c)| re.is_match(&name) && c.matches_version(&p.version)) {
                continue;
            }
            let ignored = |e: &FilterEntry| list == "malware" && e.source.as_ref().is_some_and(|s| ignore_source.contains(s));
            out.extend(entries.iter().filter(|e| e.list_name == *list && !ignored(e) && e.constraint.matches_version(&p.version)).cloned());
        }
    }
    out
}
