//! `ArrayLoader` and `RootPackageLoader`: JSON package data to [`Package`]s.

use crate::package::{Link, LinkType, Location, Package, PackageRef, ROOT_DEFAULT_PRETTY_VERSION, ROOT_NAME, RootAlias, RootPackage};
use anyhow::{Context, Result, bail};
use chrono::{DateTime, NaiveDate, NaiveDateTime};
use komposer_semver::{self as semver, DEFAULT_BRANCH_ALIAS, Stability};
use regex::Regex;
use serde_json::{Map, Value};
use std::sync::{Arc, LazyLock};

/// Options of `ArrayLoader`.
#[derive(Clone, Copy, Debug, Default)]
pub struct LoadOptions {
    /// `$loadOptions`: keep `transport-options`.
    pub transport_options: bool,
    /// `loadPackages()` drops links of a package to itself; `load()` keeps them.
    pub skip_self_links: bool,
}

/// `ArrayLoader::load()`. Returns the package, or its branch alias followed by the package
/// (the order `ArrayRepository::addPackage()` registers them in).
pub fn load(config: &Map<String, Value>, opts: LoadOptions) -> Result<Vec<PackageRef>> {
    let package = load_package(config, opts)?;
    let package = Arc::new(package);
    match branch_alias(config)? {
        Some(alias) => {
            let pretty = pretty_alias(&alias);
            let alias = Arc::new(Package::alias(&package, &alias, &pretty));
            Ok(vec![alias, package])
        }
        None => Ok(vec![package]),
    }
}

/// `ArrayLoader::loadPackages()` for repository metadata.
pub fn load_packages(versions: &[Value]) -> Result<Vec<PackageRef>> {
    let opts = LoadOptions { skip_self_links: true, ..Default::default() };
    let mut out = Vec::with_capacity(versions.len());
    for v in versions {
        let Some(config) = v.as_object() else { continue };
        out.extend(load(config, opts)?);
    }
    Ok(out)
}

/// `createObject()` + links + `configureObject()`, without the branch alias.
pub fn load_package(config: &Map<String, Value>, opts: LoadOptions) -> Result<Package> {
    let name = config.get("name").and_then(Value::as_str).with_context(|| {
        format!("Unknown package has no name defined ({}).", crate::json::encode(&Value::Object(config.clone()), crate::json::Flags::PLAIN))
    })?;
    let pretty_version = scalar_string(config.get("version")).with_context(|| format!("Package {name} has no version defined."))?;

    let version = match config.get("version_normalized").and_then(Value::as_str) {
        // repositories that need to stay Composer 1 compatible send this; renormalize it
        Some(v) if v != DEFAULT_BRANCH_ALIAS => v.to_string(),
        _ => semver::normalize(&pretty_version).map_err(|e| anyhow::anyhow!("Failed to normalize version for package \"{name}\": {e}"))?,
    };

    let mut p = Package::new(name, &version, &pretty_version);
    for kind in LinkType::ALL {
        if let Some(Value::Object(links)) = config.get(kind.key()) {
            *p.links_mut(kind) = parse_links(&p.name, &p.pretty_version, kind, links, opts.skip_self_links)?;
        }
    }
    configure(&mut p, config, opts);
    Ok(p)
}

fn configure(p: &mut Package, config: &Map<String, Value>, opts: LoadOptions) {
    let get = |k: &str| config.get(k).filter(|v| !v.is_null());
    let non_empty = |k: &str| get(k).filter(|v| !php_empty(v));

    p.package_type = get("type").and_then(Value::as_str).map(str::to_lowercase).unwrap_or_else(|| "library".into());
    p.target_dir = get("target-dir").and_then(Value::as_str).map(normalize_target_dir);
    if let Some(extra) = get("extra").filter(|v| v.is_object() || v.is_array()) {
        p.extra = extra.clone();
    }
    match get("bin") {
        Some(Value::Array(bins)) => p.bin = bins.iter().filter_map(scalar_string_ref).map(|b| b.trim_start_matches('/').to_string()).collect(),
        Some(Value::Object(bins)) => p.bin = bins.values().filter_map(scalar_string_ref).map(|b| b.trim_start_matches('/').to_string()).collect(),
        Some(v) => p.bin = scalar_string_ref(v).map(|b| vec![b.trim_start_matches('/').to_string()]).unwrap_or_default(),
        None => {}
    }
    p.installation_source = get("installation-source").and_then(Value::as_str).map(String::from);
    p.default_branch = get("default-branch") == Some(&Value::Bool(true));

    if let Some(Value::Object(src)) = get("source")
        && ["type", "url", "reference"].iter().all(|k| src.get(*k).is_some_and(|v| !v.is_null()))
    {
        p.source = Some(location(src));
    }
    if let Some(Value::Object(dist)) = get("dist")
        && ["type", "url"].iter().all(|k| dist.get(*k).is_some_and(|v| !v.is_null()))
    {
        p.dist = Some(location(dist));
    }

    if let Some(Value::Object(suggest)) = get("suggest") {
        p.suggests = suggest
            .iter()
            .map(|(k, v)| match v.as_str() {
                Some(s) if s.trim() == "self.version" => (k.clone(), Value::String(p.pretty_version.clone())),
                _ => (k.clone(), v.clone()),
            })
            .collect();
    }
    if let Some(v) = get("autoload") {
        p.autoload = v.clone();
    }
    if let Some(v) = get("autoload-dev") {
        p.autoload_dev = v.clone();
    }
    if let Some(v) = get("include-path") {
        p.include_path = v.clone();
    }
    if let Some(v) = get("php-ext") {
        p.php_ext = v.clone();
    }
    p.time = non_empty("time").and_then(scalar_string_ref).and_then(|t| format_date(&t));
    p.notification_url = non_empty("notification-url").and_then(Value::as_str).map(String::from);

    let mut meta = Map::new();
    if let Some(Value::Object(archive)) = get("archive") {
        let mut a = Map::new();
        if let Some(n) = archive.get("name").filter(|v| !php_empty(v)) {
            a.insert("name".into(), n.clone());
        }
        if let Some(e) = archive.get("exclude").filter(|v| !php_empty(v)) {
            a.insert("exclude".into(), e.clone());
        }
        if !a.is_empty() {
            meta.insert("archive".into(), Value::Object(a));
        }
    }
    if let Some(Value::Object(scripts)) = get("scripts") {
        let scripts = scripts.iter().map(|(event, listeners)| {
            let list = match listeners {
                Value::Array(_) | Value::Object(_) => listeners.clone(),
                Value::Null => Value::Array(vec![]),
                v => Value::Array(vec![v.clone()]),
            };
            (event.clone(), list)
        });
        meta.insert("scripts".into(), Value::Object(scripts.collect()));
    }
    if let Some(Value::String(d)) = non_empty("description") {
        meta.insert("description".into(), Value::String(d.clone()));
    }
    if let Some(Value::String(h)) = non_empty("homepage") {
        meta.insert("homepage".into(), Value::String(h.clone()));
    }
    if let Some(Value::Array(k)) = non_empty("keywords") {
        let k = k.iter().map(|v| Value::String(scalar_string_ref(v).unwrap_or_default()));
        meta.insert("keywords".into(), Value::Array(k.collect()));
    }
    if let Some(l) = non_empty("license") {
        let l = match l {
            Value::Array(_) | Value::Object(_) => l.clone(),
            v => Value::Array(vec![v.clone()]),
        };
        meta.insert("license".into(), l);
    }
    if let Some(a) = non_empty("authors").filter(|v| v.is_array() || v.is_object()) {
        meta.insert("authors".into(), a.clone());
    }
    if let Some(s) = get("support").filter(|v| v.is_array() || v.is_object()) {
        meta.insert("support".into(), s.clone());
    }
    if let Some(f) = non_empty("funding").filter(|v| v.is_array() || v.is_object()) {
        meta.insert("funding".into(), f.clone());
    }
    if let Some(a) = get("abandoned") {
        meta.insert("abandoned".into(), a.clone());
    }
    p.meta = meta;

    if opts.transport_options
        && let Some(t) = get("transport-options")
    {
        p.transport_options = t.clone();
    }
}

fn location(m: &Map<String, Value>) -> Location {
    let s = |k: &str| m.get(k).and_then(scalar_string_ref);
    Location {
        kind: s("type").unwrap_or_default(),
        url: s("url").unwrap_or_default(),
        reference: s("reference"),
        shasum: s("shasum"),
        mirrors: m.get("mirrors").filter(|v| !v.is_null()).cloned(),
    }
}

/// `ArrayLoader::parseLinks()`.
pub fn parse_links(source: &str, source_version: &str, kind: LinkType, links: &Map<String, Value>, skip_self: bool) -> Result<Vec<Link>> {
    let mut out: Vec<Link> = Vec::with_capacity(links.len());
    for (target, constraint) in links {
        let Some(pretty) = constraint.as_str() else { continue };
        let target = target.to_lowercase();
        if skip_self && target == source {
            continue;
        }
        let link = create_link(source, source_version, kind, &target, pretty)?;
        match out.iter_mut().find(|l| l.target == target) {
            Some(existing) => *existing = link,
            None => out.push(link),
        }
    }
    Ok(out)
}

/// `ArrayLoader::createLink()`.
pub fn create_link(source: &str, source_version: &str, kind: LinkType, target: &str, pretty: &str) -> Result<Link> {
    let constraint = if pretty == "self.version" { source_version } else { pretty };
    let parsed = semver::parse_constraints(constraint).map_err(|_| {
        anyhow::anyhow!("Link constraint in {source} {} > {target} should be a valid version constraint, got \"{constraint}\"", kind.description())
    })?;
    Ok(Link { source: source.to_lowercase(), target: target.to_lowercase(), constraint: parsed, kind, pretty_constraint: pretty.to_string() })
}

/// `ArrayLoader::getBranchAlias()`: normalized alias version, if any.
pub fn branch_alias(config: &Map<String, Value>) -> Result<Option<String>> {
    let version = scalar_string(config.get("version")).context("no/invalid version defined")?;
    if !version.starts_with("dev-") && !version.ends_with("-dev") {
        return Ok(None);
    }

    if let Some(Value::Object(aliases)) = config.get("extra").and_then(|e| e.get("branch-alias")) {
        for (source_branch, target) in aliases {
            let Some(target) = target.as_str() else { continue };
            // ensure it is an alias to a -dev package
            let Some(target_base) = target.strip_suffix("-dev") else { continue };
            // normalize without -dev and ensure it's a numeric branch that is parseable
            let validated = if target == DEFAULT_BRANCH_ALIAS { target.to_string() } else { semver::normalize_branch(target_base) };
            if !validated.ends_with("-dev") {
                continue;
            }
            // ensure that it is the current branch aliasing itself
            if version.to_lowercase() != source_branch.to_lowercase() {
                continue;
            }
            // if using numeric aliases ensure the alias is a valid subversion
            if let (Some(src_prefix), Some(target_prefix)) =
                (semver::parse_numeric_alias_prefix(source_branch), semver::parse_numeric_alias_prefix(target))
                && !target_prefix.to_lowercase().starts_with(&src_prefix.to_lowercase())
            {
                continue;
            }
            return Ok(Some(validated));
        }
    }

    if config.get("default-branch") == Some(&Value::Bool(true))
        && semver::parse_numeric_alias_prefix(version.strip_prefix('v').unwrap_or(&version)).is_none()
    {
        return Ok(Some(DEFAULT_BRANCH_ALIAS.to_string()));
    }
    Ok(None)
}

/// `{(\.9{7})+}` → `.x`, the pretty form of a normalized branch alias.
pub fn pretty_alias(normalized: &str) -> String {
    static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(\.9{7})+").unwrap());
    RE.replace_all(normalized, ".x").into_owned()
}

/// Root version detected from the environment or VCS (`VersionGuesser::guessVersion()`).
#[derive(Clone, Debug, Default)]
pub struct GuessedVersion {
    pub pretty_version: String,
    pub version: String,
    pub commit: Option<String>,
}

/// `RootPackageLoader::load()`, minus repository registration (the caller builds
/// repositories from [`RootPackage::repositories`]). `guessed` is only consulted when
/// composer.json has no `version` and `COMPOSER_ROOT_VERSION` is unset.
pub fn load_root(raw: &Map<String, Value>, guess: impl FnOnce() -> Option<GuessedVersion>) -> Result<RootPackage> {
    let mut config = raw.clone();
    match config.get("name").and_then(Value::as_str) {
        None => {
            config.insert("name".into(), ROOT_NAME.into());
        }
        Some(name) => {
            if let Some(err) = package_naming_error(name, false) {
                bail!("Your package name {err}");
            }
        }
    }

    let mut auto_versioned = false;
    if !config.contains_key("version") {
        let mut commit = None;
        if let Some(v) = std::env::var("COMPOSER_ROOT_VERSION").ok().filter(|v| !v.is_empty()) {
            config.insert("version".into(), root_version_from_env(&v).into());
        } else if let Some(g) = guess() {
            config.insert("version".into(), g.pretty_version.into());
            config.insert("version_normalized".into(), g.version.into());
            commit = g.commit;
        }
        if !config.contains_key("version") {
            config.insert("version".into(), "1.0.0".into());
            auto_versioned = true;
        }
        if let Some(commit) = commit {
            let loc = serde_json::json!({"type": "", "url": "", "reference": commit});
            config.insert("source".into(), loc.clone());
            config.insert("dist".into(), loc);
        }
    }

    let mut package = load_package(&config, LoadOptions::default())?;
    // source/dist with empty type and url are still recorded for the root package
    if let Some(Value::Object(src)) = config.get("source") {
        package.source.get_or_insert_with(|| location(src));
    }
    if let Some(Value::Object(dist)) = config.get("dist") {
        package.dist.get_or_insert_with(|| location(dist));
    }
    if auto_versioned {
        package.pretty_version = ROOT_DEFAULT_PRETTY_VERSION.into();
    }

    let minimum_stability = match config.get("minimum-stability").and_then(Value::as_str) {
        Some(s) => semver::normalize_stability(s).map_err(|e| anyhow::anyhow!("{e}"))?,
        None => Stability::Stable,
    };

    let mut aliases = vec![];
    let mut stability_flags = vec![];
    let mut references = vec![];
    for kind in [LinkType::Require, LinkType::DevRequire] {
        if !config.contains_key(kind.key()) {
            continue;
        }
        let links: Vec<(String, String)> = package.links(kind).iter().map(|l| (l.target.clone(), l.pretty_constraint.clone())).collect();
        extract_aliases(&links, &mut aliases)?;
        extract_stability_flags(&links, minimum_stability, &mut stability_flags);
        extract_references(&links, &mut references);
        if links.iter().any(|(target, _)| *target == package.name) {
            bail!(
                "Root package '{}' cannot require itself in its composer.json\nDid you accidentally name your root package after an external package?",
                package.name
            );
        }
    }
    for kind in LinkType::ALL {
        if let Some(Value::Object(links)) = config.get(kind.key()) {
            for name in links.keys() {
                if let Some(err) = package_naming_error(name, true) {
                    bail!("{}.{err}", kind.key());
                }
            }
        }
    }

    let branch_alias = branch_alias(&config)?.map(|a| {
        let pretty = pretty_alias(&a);
        (a, pretty)
    });

    Ok(RootPackage {
        package,
        minimum_stability,
        prefer_stable: config.get("prefer-stable").is_some_and(php_truthy),
        stability_flags,
        references,
        aliases,
        config: config.get("config").and_then(Value::as_object).cloned().unwrap_or_default(),
        repositories: match config.get("repositories") {
            Some(Value::Array(r)) => r.clone(),
            Some(Value::Object(r)) => r.values().cloned().collect(),
            _ => vec![],
        },
        branch_alias,
        raw: raw.clone(),
    })
}

/// `VersionGuesser::getRootVersionFromEnv()`.
pub fn root_version_from_env(version: &str) -> String {
    static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^(\d+(?:\.\d+)*)-dev$").unwrap());
    match RE.captures(version) {
        Some(c) => format!("{}.x-dev", &c[1]),
        None => version.to_string(),
    }
}

fn extract_aliases(requires: &[(String, String)], aliases: &mut Vec<RootAlias>) -> Result<()> {
    static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?:^|\| *|, *)([^,\s#|]+)(?:#[^ ]+)? +as +([^,\s|]+)(?:$| *\|| *,)").unwrap());
    for (name, version) in requires {
        if let Some(c) = RE.captures(version) {
            let norm = |v: &str| semver::normalize_full(v, Some(version)).map_err(|e| anyhow::anyhow!("{e}"));
            aliases.push(RootAlias { package: name.to_lowercase(), version: norm(&c[1])?, alias: c[2].to_string(), alias_normalized: norm(&c[2])? });
        } else if version.contains(" as ") {
            bail!("Invalid alias definition in \"{name}\": \"{version}\". Aliases should be in the form \"exact-version as other-exact-version\".");
        }
    }
    Ok(())
}

/// `RootPackageLoader::extractStabilityFlags()`.
pub fn extract_stability_flags(requires: &[(String, String)], minimum: Stability, flags: &mut Vec<(String, Stability)>) {
    static OR_SPLIT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s*\|\|?\s*").unwrap());
    static FLAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^[^@]*?@(stable|RC|beta|alpha|dev)$").unwrap());
    static ALIAS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^([^,\s@]+) as .+$").unwrap());
    static PLAIN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[^,\s@]+$").unwrap());

    fn set(flags: &mut Vec<(String, Stability)>, name: String, s: Stability) {
        match flags.iter_mut().find(|(n, _)| *n == name) {
            Some(entry) => entry.1 = s,
            None => flags.push((name, s)),
        }
    }
    let current = |flags: &Vec<(String, Stability)>, name: &str| flags.iter().find(|(n, _)| n == name).map(|(_, s)| *s);

    for (req_name, req_version) in requires {
        let name = req_name.to_lowercase();
        let constraints: Vec<String> = OR_SPLIT.split(req_version.trim()).flat_map(split_and_constraints).collect();

        // explicit stability flags win, the most unstable one is kept
        let mut matched = false;
        for constraint in &constraints {
            if let Some(c) = FLAG.captures(constraint) {
                let stability = Stability::parse(&c[1]).unwrap_or(Stability::Stable);
                if current(flags, &name).is_some_and(|s| s > stability) {
                    continue;
                }
                set(flags, name.clone(), stability);
                matched = true;
            }
        }
        if matched {
            continue;
        }

        // infer flags for explicit -dev or -beta versions that are less stable than allowed
        for constraint in &constraints {
            let version = ALIAS.replace(constraint, "$1");
            if !PLAIN.is_match(&version) {
                continue;
            }
            let stability = semver::parse_stability(&version);
            if stability == Stability::Stable {
                continue;
            }
            if current(flags, &name).is_some_and(|s| s > stability) || minimum > stability {
                continue;
            }
            set(flags, name.clone(), stability);
        }
    }
}

/// `preg_split('{(?<!^|as|[=>< ,]) *(?<!-)[, ](?!-) *(?!,|as|$)}', $s)`, simulated with the
/// same backtracking because the `regex` crate has no lookaround.
pub fn split_and_constraints(s: &str) -> Vec<String> {
    let b = s.as_bytes();
    let n = b.len();
    let match_at = |p: usize| -> Option<usize> {
        if p == 0 || p > n || b[..p].ends_with(b"as") || matches!(b[p - 1], b'=' | b'>' | b'<' | b' ' | b',') {
            return None;
        }
        let spaces = |from: usize| b[from..].iter().take_while(|&&c| c == b' ').count();
        for q in (p..=p + spaces(p)).rev() {
            if q >= n || b[q - 1] == b'-' || !matches!(b[q], b',' | b' ') || b.get(q + 1) == Some(&b'-') {
                continue;
            }
            for r in (q + 1..=q + 1 + spaces(q + 1)).rev() {
                if r < n && !b[r..].starts_with(b",") && !b[r..].starts_with(b"as") {
                    return Some(r);
                }
            }
        }
        None
    };
    let mut parts = vec![];
    let (mut last, mut p) = (0, 0);
    while p < n {
        match match_at(p) {
            Some(end) => {
                parts.push(s[last..p].to_string());
                (last, p) = (end, end);
            }
            None => p += 1,
        }
    }
    parts.push(s[last..].to_string());
    parts
}

/// `RootPackageLoader::extractReferences()`.
pub fn extract_references(requires: &[(String, String)], references: &mut Vec<(String, String)>) {
    static ALIAS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^([^,\s@]+) as .+$").unwrap());
    static REF: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[^,\s@]+?#([a-f0-9]+)$").unwrap());
    for (name, version) in requires {
        let version = ALIAS.replace(version, "$1");
        if let Some(c) = REF.captures(&version)
            && semver::parse_stability(&version) == Stability::Dev
        {
            let name = name.to_lowercase();
            references.retain(|(n, _)| *n != name);
            references.push((name, c[1].to_string()));
        }
    }
}

/// `ValidatingArrayLoader::hasPackageNamingError()`.
pub fn package_naming_error(name: &str, is_link: bool) -> Option<String> {
    static VALID: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)^[a-z0-9](?:[_.-]?[a-z0-9]+)*/[a-z0-9](?:(?:[_.]|-{1,2})?[a-z0-9]+)*$").unwrap());
    static CAMEL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"([a-z])([A-Z])|([A-Z])([A-Z][a-z])").unwrap());
    const RESERVED: [&str; 22] = [
        "nul", "con", "prn", "aux", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8", "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5",
        "lpt6", "lpt7", "lpt8", "lpt9",
    ];
    if crate::is_platform_package(name) {
        return None;
    }
    if !VALID.is_match(name) {
        return Some(format!(
            "{name} is invalid, it should have a vendor name, a forward slash, and a package name. The vendor and package name can be words separated by -, . or _. The complete name should match \"^[a-z0-9]([_.-]?[a-z0-9]+)*/[a-z0-9](([_.]?|-{{0,2}})[a-z0-9]+)*$\"."
        ));
    }
    let lower = name.to_lowercase();
    let (vendor, pkg) = lower.split_once('/').unwrap_or((&lower, ""));
    if RESERVED.contains(&vendor) || RESERVED.contains(&pkg) {
        return Some(format!("{name} is reserved, package and vendor names can not match any of: {}.", RESERVED.join(", ")));
    }
    if name.ends_with(".json") {
        return Some(format!("{name} is invalid, package names can not end in .json, consider renaming it or perhaps using a -json suffix instead."));
    }
    if name.chars().any(|c| c.is_ascii_uppercase()) {
        if is_link {
            return Some(format!("{name} is invalid, it should not contain uppercase characters. Please use {lower} instead."));
        }
        let suggest = CAMEL.replace_all(name, "$1$3-$2$4").to_lowercase();
        return Some(format!("{name} is invalid, it should not contain uppercase characters. We suggest using {suggest} instead."));
    }
    None
}

/// `Package::getTargetDir()` path cleanup: drops `.`/`..` segments.
fn normalize_target_dir(dir: &str) -> String {
    static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?:^|[\\/]+)\.\.?(?:[\\/]+|$)(?:\.\.?(?:[\\/]+|$))*").unwrap());
    RE.replace_all(dir, "/").trim_start_matches('/').to_string()
}

/// `new \DateTime($date, UTC)` formatted as `DATE_RFC3339`; `None` where PHP would throw.
pub fn format_date(date: &str) -> Option<String> {
    let date = date.trim();
    if !date.is_empty() && date.bytes().all(|b| b.is_ascii_digit()) {
        let ts: i64 = date.parse().ok()?;
        return DateTime::from_timestamp(ts, 0).map(|d| d.format("%Y-%m-%dT%H:%M:%S+00:00").to_string());
    }
    if let Ok(d) = DateTime::parse_from_rfc3339(date) {
        return Some(d.format("%Y-%m-%dT%H:%M:%S%:z").to_string());
    }
    for fmt in ["%Y-%m-%d %H:%M:%S%.f %z", "%Y-%m-%d %H:%M:%S%.f%:z", "%Y-%m-%dT%H:%M:%S%.f%z", "%Y-%m-%d %H:%M:%S%.f%z"] {
        if let Ok(d) = DateTime::parse_from_str(date, fmt) {
            return Some(d.format("%Y-%m-%dT%H:%M:%S%:z").to_string());
        }
    }
    for fmt in ["%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M", "%Y-%m-%dT%H:%M"] {
        if let Ok(d) = NaiveDateTime::parse_from_str(date, fmt) {
            return Some(d.format("%Y-%m-%dT%H:%M:%S+00:00").to_string());
        }
    }
    if let Some(utc) = date.strip_suffix(" UTC").or_else(|| date.strip_suffix('Z')) {
        return format_date(utc);
    }
    NaiveDate::parse_from_str(date, "%Y-%m-%d").ok().map(|d| d.format("%Y-%m-%dT00:00:00+00:00").to_string())
}

/// PHP `empty()`.
pub fn php_empty(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::Bool(b) => !b,
        Value::Number(n) => n.as_f64() == Some(0.0),
        Value::String(s) => s.is_empty() || s == "0",
        Value::Array(a) => a.is_empty(),
        Value::Object(o) => o.is_empty(),
    }
}

/// PHP `(bool)` cast.
pub fn php_truthy(v: &Value) -> bool {
    !php_empty(v)
}

/// PHP `(string)` of a scalar.
fn scalar_string(v: Option<&Value>) -> Option<String> {
    v.and_then(scalar_string_ref)
}

pub(crate) fn scalar_string_ref(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(true) => Some("1".into()),
        Value::Bool(false) => Some(String::new()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn and_split_matches_composer() {
        assert_eq!(split_and_constraints(">=1.0 <2.0"), vec![">=1.0", "<2.0"]);
        assert_eq!(split_and_constraints(">=1.0,<2.0"), vec![">=1.0", "<2.0"]);
        assert_eq!(split_and_constraints(">= 1.0"), vec![">= 1.0"]);
        assert_eq!(split_and_constraints("1.0 - 2.0"), vec!["1.0 - 2.0"]);
        assert_eq!(split_and_constraints("dev-main as 1.0.x-dev"), vec!["dev-main as 1.0.x-dev"]);
        assert_eq!(split_and_constraints("^1.0@dev"), vec!["^1.0@dev"]);
    }

    #[test]
    fn stability_flags() {
        let reqs = vec![
            ("a/a".to_string(), "^1.0@dev".to_string()),
            ("b/b".to_string(), "1.0.0-beta2".to_string()),
            ("c/c".to_string(), "dev-main as 1.0.x-dev".to_string()),
            ("d/d".to_string(), "^1.0".to_string()),
        ];
        let mut flags = vec![];
        extract_stability_flags(&reqs, Stability::Stable, &mut flags);
        assert_eq!(flags, vec![("a/a".to_string(), Stability::Dev), ("b/b".to_string(), Stability::Beta), ("c/c".to_string(), Stability::Dev),]);
    }

    #[test]
    fn dates() {
        assert_eq!(format_date("2024-05-01T12:00:00+00:00").as_deref(), Some("2024-05-01T12:00:00+00:00"));
        assert_eq!(format_date("2024-05-01 12:00:00").as_deref(), Some("2024-05-01T12:00:00+00:00"));
        assert_eq!(format_date("2024-05-01T12:00:00Z").as_deref(), Some("2024-05-01T12:00:00+00:00"));
        assert_eq!(format_date("1700000000").as_deref(), Some("2023-11-14T22:13:20+00:00"));
        assert_eq!(format_date("nope"), None);
    }

    #[test]
    fn naming() {
        assert!(package_naming_error("laravel/framework", false).is_none());
        assert!(package_naming_error("ext-mbstring", true).is_none());
        assert!(package_naming_error("Foo/Bar", true).unwrap().contains("Please use foo/bar"));
        assert!(package_naming_error("nope", false).is_some());
    }

    #[test]
    fn loads_branch_alias() {
        let cfg = json!({"name": "a/b", "version": "dev-master", "extra": {"branch-alias": {"dev-master": "2.x-dev"}}});
        let pkgs = load(cfg.as_object().unwrap(), LoadOptions::default()).unwrap();
        assert_eq!(pkgs.len(), 2);
        assert_eq!(pkgs[0].version, "2.9999999.9999999.9999999-dev");
        assert_eq!(pkgs[0].pretty_version, "2.x-dev");
        assert!(Arc::ptr_eq(pkgs[0].alias_of.as_ref().unwrap(), &pkgs[1]));
    }
}
