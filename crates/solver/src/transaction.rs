//! `Transaction`, `LockTransaction` and `LocalRepoTransaction`: from a package set to another,
//! as ordered operations.

use crate::pool::{Literal, Pool};
use crate::request::{PackageSet, addr};
use crate::rule::RuleHandle;
use anyhow::{Result, bail};
use indexmap::IndexMap;
use komposer_package::loader::package_naming_error;
use komposer_package::operation::Operation;
use komposer_package::repository::Repository;
use komposer_package::{Package, PackageRef, RootAlias, is_platform_package};
use regex::Regex;
use serde_json::Value;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock};

/// `Transaction::__construct()` + `getOperations()`: what to do to turn `present` into `result`.
pub fn transaction_operations(present: &[PackageRef], result: &[PackageRef]) -> Vec<Operation> {
    // sort alias packages by the same name behind their non alias version
    let package_sort = |a: &PackageRef, b: &PackageRef| -> Ordering {
        if a.name == b.name {
            if a.is_alias() != b.is_alias() {
                return if a.is_alias() { Ordering::Less } else { Ordering::Greater };
            }
            return b.version.cmp(&a.version);
        }
        b.name.cmp(&a.name)
    };

    let mut result_packages = PackageSet::default();
    let mut by_name: HashMap<String, Vec<PackageRef>> = HashMap::new();
    for p in result {
        result_packages.insert(p.clone());
        for name in p.names(true) {
            by_name.entry(name).or_default().push(p.clone());
        }
    }
    let mut sorted: Vec<PackageRef> = result_packages.list().to_vec();
    sorted.sort_by(package_sort);
    for list in by_name.values_mut() {
        list.sort_by(package_sort);
    }
    let providers = |target: &str| by_name.get(target).map(Vec::as_slice).unwrap_or(&[]);

    let mut present_map: IndexMap<&str, &PackageRef> = IndexMap::new();
    let mut remove_map: IndexMap<&str, &PackageRef> = IndexMap::new();
    let mut present_aliases: IndexMap<String, &PackageRef> = IndexMap::new();
    let mut remove_aliases: IndexMap<String, &PackageRef> = IndexMap::new();
    for p in present {
        if p.is_alias() {
            let key = format!("{}::{}", p.name, p.version);
            present_aliases.insert(key.clone(), p);
            remove_aliases.insert(key, p);
        } else {
            present_map.insert(&p.name, p);
            remove_map.insert(&p.name, p);
        }
    }

    // roots: packages in the result that are not required by any other package in it
    let mut roots: IndexMap<usize, PackageRef> = sorted.iter().map(|p| (addr(p), p.clone())).collect();
    for p in &sorted {
        if !roots.contains_key(&addr(p)) {
            continue;
        }
        for link in &p.requires {
            for require in providers(&link.target) {
                if addr(require) != addr(p) {
                    roots.shift_remove(&addr(require));
                }
            }
        }
    }

    let mut operations = vec![];
    let mut stack: Vec<PackageRef> = roots.into_values().collect();
    let mut visited = HashSet::new();
    let mut processed = HashSet::new();
    while let Some(package) = stack.pop() {
        if processed.contains(&addr(&package)) {
            continue;
        }
        if visited.insert(addr(&package)) {
            stack.push(package.clone());
            match &package.alias_of {
                Some(of) => stack.push(of.clone()),
                None => {
                    for link in &package.requires {
                        stack.extend(providers(&link.target).iter().cloned());
                    }
                }
            }
            continue;
        }
        processed.insert(addr(&package));
        if package.is_alias() {
            let key = format!("{}::{}", package.name, package.version);
            if present_aliases.contains_key(&key) {
                remove_aliases.shift_remove(&key);
            } else {
                operations.push(Operation::MarkAliasInstalled(package));
            }
        } else if let Some(&source) = present_map.get(package.name.as_str()) {
            let needs_update = package.version != source.version
                || package.dist_reference() != source.dist_reference()
                || package.source_reference() != source.source_reference()
                || package.is_abandoned() != source.is_abandoned()
                || package.replacement_package() != source.replacement_package();
            if needs_update {
                operations.push(Operation::Update { initial: source.clone(), target: package.clone() });
            }
            remove_map.shift_remove(package.name.as_str());
        } else {
            remove_map.shift_remove(package.name.as_str());
            operations.push(Operation::Install(package));
        }
    }

    let mut all: Vec<Operation> = remove_map.values().rev().map(|&p| Operation::Uninstall(p.clone())).collect();
    all.extend(operations);
    all.extend(remove_aliases.values().map(|&p| Operation::MarkAliasUninstalled(p.clone())));

    // moving plugins moves them before uninstalls, so uninstalls are moved to the front again after
    move_uninstalls_to_front(move_plugins_to_front(all))
}

/// `LocalRepoTransaction`: from the installed repository to the locked one, for `install`.
pub fn local_repo_transaction(locked: &dyn Repository, installed: &dyn Repository) -> Result<Vec<Operation>> {
    Ok(transaction_operations(&installed.packages()?, &locked.packages()?))
}

/// `movePluginsToFront()`: plugins and their dependencies are installed first.
fn move_plugins_to_front(operations: Vec<Operation>) -> Vec<Operation> {
    let (mut dl_no_deps, mut dl_with_deps, mut dl_requires) = (vec![], vec![], vec![]);
    let (mut plugins_no_deps, mut plugins_with_deps, mut plugin_requires) = (vec![], vec![], vec![]);
    let mut rest: Vec<Option<Operation>> = operations.into_iter().map(Some).collect();

    for slot in rest.iter_mut().rev() {
        let package = match slot.as_ref() {
            Some(Operation::Install(p)) | Some(Operation::Update { target: p, .. }) => p.clone(),
            _ => continue,
        };
        let names = package.names(true);
        let requires: Vec<String> = package.requires.iter().map(|l| l.target.clone()).filter(|r| !is_platform_package(r)).collect();

        let modifies_downloads =
            package.package_type == "composer-plugin" && package.extra.get("plugin-modifies-downloads") == Some(&Value::Bool(true));
        if modifies_downloads || names.iter().any(|n| dl_requires.contains(n)) {
            let op = slot.take();
            if modifies_downloads && requires.is_empty() {
                dl_no_deps.insert(0, op);
            } else {
                dl_requires.extend(requires);
                dl_with_deps.insert(0, op);
            }
            continue;
        }

        let is_plugin = package.package_type == "composer-plugin" || package.package_type == "composer-installer";
        if is_plugin || names.iter().any(|n| plugin_requires.contains(n)) {
            let op = slot.take();
            if is_plugin && requires.is_empty() {
                plugins_no_deps.insert(0, op);
            } else {
                plugin_requires.extend(requires);
                plugins_with_deps.insert(0, op);
            }
        }
    }

    [dl_no_deps, dl_with_deps, plugins_no_deps, plugins_with_deps, rest].into_iter().flatten().flatten().collect()
}

/// `moveUninstallsToFront()`.
fn move_uninstalls_to_front(operations: Vec<Operation>) -> Vec<Operation> {
    let (mut uninstalls, rest): (Vec<Operation>, Vec<Operation>) =
        operations.into_iter().partition(|op| matches!(op, Operation::Uninstall(_) | Operation::MarkAliasUninstalled(_)));
    uninstalls.extend(rest);
    uninstalls
}

/// `LockTransaction`: the solver's result compared to the lock file.
#[derive(Clone, Debug)]
pub struct LockTransaction {
    operations: Vec<Operation>,
    present: Vec<PackageRef>,
    all: Vec<PackageRef>,
    non_dev: Vec<PackageRef>,
    dev: Vec<PackageRef>,
}

impl LockTransaction {
    /// `fixed` tells by pool id which packages are fixed (`unlockableMap`).
    pub(crate) fn new(pool: &Pool, present: Vec<PackageRef>, fixed: &[bool], decisions: &[(Literal, RuleHandle)]) -> Result<LockTransaction> {
        let (mut all, mut non_dev) = (vec![], vec![]);
        // Composer iterates the decisions from the last one to the first
        for &(literal, _) in decisions.iter().rev() {
            if literal <= 0 {
                continue;
            }
            let package = pool.package(literal);
            if !pool.is_root_package(package) {
                validate_package(package)?;
            }
            all.push(package.clone());
            if !fixed[literal as usize] {
                non_dev.push(package.clone());
            }
        }
        let operations = transaction_operations(&present, &all);
        Ok(LockTransaction { operations, present, all, non_dev, dev: vec![] })
    }

    pub fn operations(&self) -> &[Operation] {
        &self.operations
    }

    /// `setNonDevPackages()`: moves the packages found by the no-dev extraction solve to non-dev.
    pub fn set_non_dev_packages(&mut self, extraction: &LockTransaction) {
        self.dev = std::mem::take(&mut self.non_dev);
        for package in extraction.new_lock_packages(false, false) {
            let (matching, rest): (Vec<PackageRef>, Vec<PackageRef>) =
                std::mem::take(&mut self.dev).into_iter().partition(|p| p.name == package.name);
            self.non_dev.extend(matching);
            self.dev = rest;
        }
    }

    /// `getNewLockPackages()`: the packages for the lock file's `packages` (or `packages-dev`) section.
    pub fn new_lock_packages(&self, dev_mode: bool, update_mirrors: bool) -> Vec<PackageRef> {
        let packages = if dev_mode { &self.dev } else { &self.non_dev };
        packages
            .iter()
            .filter(|p| !p.is_alias())
            .map(|p| {
                // when only updating mirrors, keep the present packages' references so the lock file stays as-is
                if update_mirrors && !self.present.iter().any(|q| addr(q) == addr(p)) { self.update_mirror_and_urls(p) } else { p.clone() }
            })
            .collect()
    }

    /// `updateMirrorAndUrls()`: the present package with the new package's urls and mirrors, if the types match.
    fn update_mirror_and_urls(&self, package: &PackageRef) -> PackageRef {
        let Some(present) = self.present.iter().find(|p| {
            p.name == package.name
                && p.version == package.version
                && p.source_reference().is_some()
                && p.source.as_ref().map(|s| &s.kind) == package.source.as_ref().map(|s| &s.kind)
        }) else {
            return package.clone();
        };

        let mut updated = (**present).clone();
        if !present.is_alias()
            && let (Some(source), Some(new)) = (&mut updated.source, &package.source)
        {
            source.url = new.url.clone();
            source.mirrors = new.mirrors.clone();
        }
        // if the dist type changed, only the source url/mirrors are updated
        if updated.dist.as_ref().map(|d| &d.kind) != package.dist.as_ref().map(|d| &d.kind) {
            return Arc::new(updated);
        }
        if let (Some(dist), Some(new)) = (&mut updated.dist, &package.dist) {
            // update the dist url if it is in a known format
            if let Some(reference) = dist.reference.clone()
                && KNOWN_DIST_HOSTS.is_match(&new.url)
            {
                dist.url = replace_sha_in_url(&new.url, &reference);
            }
            dist.mirrors = new.mirrors.clone();
        }
        Arc::new(updated)
    }

    /// `getAliases()`: the root aliases in use, for the lock file.
    pub fn aliases(&self, aliases: &[RootAlias]) -> Vec<RootAlias> {
        let mut remaining: Vec<Option<&RootAlias>> = aliases.iter().map(Some).collect();
        let mut used = vec![];
        for package in self.all.iter().filter(|p| p.is_alias()) {
            for slot in remaining.iter_mut() {
                if let Some(alias) = slot.filter(|a| a.package == package.name) {
                    used.push(alias.clone());
                    *slot = None;
                }
            }
        }
        used.sort_by(|a, b| a.package.cmp(&b.package));
        used
    }
}

/// bitbucket, github and gitlab dist urls embed the reference.
pub(crate) static KNOWN_DIST_HOSTS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^https?://(?:(?:www\.)?bitbucket\.org|(api\.)?github\.com|(?:www\.)?gitlab\.com)/").unwrap());

/// `preg_replace('{(?<=/|sha=)[a-f0-9]{40}(?=/|$)}i', $reference, $url)`.
pub(crate) fn replace_sha_in_url(url: &str, reference: &str) -> String {
    let b = url.as_bytes();
    let mut out = String::with_capacity(url.len());
    let mut i = 0;
    while i < b.len() {
        let preceded = (i > 0 && b[i - 1] == b'/') || (i >= 4 && &b[i - 4..i] == b"sha=");
        let end = i + 40;
        if preceded && end <= b.len() && b[i..end].iter().all(u8::is_ascii_hexdigit) && (end == b.len() || b[end] == b'/') {
            out.push_str(reference);
            i = end;
        } else {
            let ch = url[i..].chars().next().expect("char boundary");
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

/// `ValidatingArrayLoader::validatePackage()`: rejects names and urls that could inject commands.
fn validate_package(p: &Package) -> Result<()> {
    if let Some(err) = package_naming_error(&p.name, false) {
        bail!("Invalid package found during dependency resolution, aborting: {err}");
    }
    let fields = [
        ("source.url", p.source.as_ref().map(|s| s.url.as_str())),
        ("source.reference", p.source_reference()),
        ("dist.url", p.dist.as_ref().map(|d| d.url.as_str())),
        ("dist.reference", p.dist_reference()),
    ];
    for (field, value) in fields {
        if let Some(value) = value
            && value.trim_start().starts_with('-')
        {
            bail!("{} has an invalid {field}, it must not start with a \"-\": {value}", p.name);
        }
    }
    static RSH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^\s*(?:rsh|jsh)\s*:").unwrap());
    static P4PORT: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)^(?:(?:tcp|ssl)(?:4|6|46|64)?:)?(?:\[[0-9a-f:.]+\]|[a-z0-9._][a-z0-9._-]*)(?::[a-z0-9._][a-z0-9._-]*)?$").unwrap()
    });
    if let Some(source) = p.source.as_ref().filter(|s| s.kind == "perforce")
        && (RSH.is_match(&source.url) || !P4PORT.is_match(&source.url))
    {
        bail!("{} has an invalid source.url, it must be a Perforce port of the form [tcp|ssl:][host:]port: {}", p.name, source.url);
    }
    static DOTDOT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?:^|[\\/])\.\.(?:[\\/]|$)").unwrap());
    if let Some(bin) = p.bin.iter().find(|b| DOTDOT.is_match(b)) {
        bail!("{} has an invalid bin {bin}, it must not contain \"..\" path segments", p.name);
    }
    Ok(())
}
