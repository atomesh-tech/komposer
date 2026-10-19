//! `RepositoryInterface`, `ArrayRepository` and `StabilityFilter`.

use crate::package::{Package, PackageRef};
use anyhow::Result;
use komposer_semver::{Constraint, Stability};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// The repository classes Composer special-cases with `instanceof`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RepoKind {
    /// `PlatformRepository`
    Platform,
    /// `RootPackageRepository`
    Root,
    /// `LockArrayRepository` (composer.lock contents)
    Lock,
    /// `InstalledRepository` / `InstalledFilesystemRepository` (vendor/composer/installed.json)
    Installed,
    /// composer, vcs, path, package, artifact repositories
    Remote,
}

/// Arguments of `RepositoryInterface::loadPackages()`.
pub struct LoadRequest<'a> {
    /// package name => constraint (`None` = any version), in request order
    pub names: &'a [(String, Option<Constraint>)],
    pub acceptable_stabilities: &'a HashSet<Stability>,
    pub stability_flags: &'a HashMap<String, Stability>,
    /// name => normalized versions already loaded from this repository
    pub already_loaded: &'a HashMap<String, HashSet<String>>,
}

/// Return value of `loadPackages()`.
#[derive(Default)]
pub struct LoadResult {
    /// Names this repository canonically provides; lower priority repositories are not asked for them.
    pub names_found: Vec<String>,
    pub packages: Vec<PackageRef>,
}

pub trait Repository: Send + Sync {
    /// `getRepoName()`, e.g. `composer repo (https://repo.packagist.org)`.
    fn repo_name(&self) -> String;

    fn kind(&self) -> RepoKind {
        RepoKind::Remote
    }

    /// `loadPackages()`.
    fn load_packages(&self, req: &LoadRequest) -> Result<LoadResult>;

    /// `getPackages()`. Lazy remote repositories may only return what they have loaded.
    fn packages(&self) -> Result<Vec<PackageRef>>;

    /// `findPackages($name, $constraint)`.
    fn find_packages(&self, name: &str, constraint: Option<&Constraint>) -> Result<Vec<PackageRef>> {
        let name = name.to_lowercase();
        let found = self.packages()?.into_iter().filter(|p| p.name == name && constraint.is_none_or(|c| c.matches_version(&p.version)));
        Ok(found.collect())
    }

    /// `findPackage($name, $constraint)`.
    fn find_package(&self, name: &str, constraint: &Constraint) -> Result<Option<PackageRef>> {
        Ok(self.find_packages(name, Some(constraint))?.into_iter().next())
    }

    /// Loads metadata of packages that are likely to be requested next in one concurrent batch
    /// (komposer only, e.g. the packages of composer.lock before a full update). `dev_names` also
    /// get their dev branches. Results only change how soon the data arrives, never what is loaded.
    fn prefetch(&self, names: &[String], dev_names: &[String]) {
        let _ = (names, dev_names);
    }

    /// The metadata files loaded so far (`vendor/name`, `vendor/name~dev`), as hints for [`Repository::prefetch`]
    /// on the next run.
    fn loaded_files(&self) -> Vec<String> {
        vec![]
    }

    /// `AdvisoryProviderInterface::hasSecurityAdvisories()`.
    fn has_security_advisories(&self) -> bool {
        false
    }

    /// `AdvisoryProviderInterface::getSecurityAdvisories()`. With `allow_partial`, advisories
    /// embedded in package metadata (id + affected versions only) are acceptable.
    fn security_advisories(&self, constraints: &[(String, Constraint)], allow_partial: bool) -> Result<AdvisoryResult> {
        let _ = (constraints, allow_partial);
        Ok(AdvisoryResult::default())
    }

    /// `FilterListProviderInterface::getFilter()`: entries of the named lists (e.g. `malware`)
    /// that may match the given packages.
    fn filter_entries(&self, constraints: &[(String, Constraint)], lists: &[String]) -> Result<Vec<FilterEntry>> {
        let _ = (constraints, lists);
        Ok(vec![])
    }

    /// `search($query, $mode, $type)`: `{"name", "description", "abandoned"?}` results.
    fn search(&self, query: &str, mode: SearchMode, kind: Option<&str>) -> Result<Vec<serde_json::Value>> {
        Ok(array_search(&self.packages()?, query, mode, kind))
    }

    /// `getProviders($name)`: names of packages that `provide` the given name.
    fn providers(&self, name: &str) -> Result<Vec<String>> {
        let mut out = vec![];
        for p in self.packages()? {
            if p.provides.iter().any(|l| l.target == name) && !out.contains(&p.pretty_name) {
                out.push(p.pretty_name.clone());
            }
        }
        Ok(out)
    }

    /// `getProviders($name)` with each provider's description, as shown in solver problems.
    /// Override when [`Repository::packages`] does not hold the providers.
    fn provider_descriptions(&self, name: &str) -> Result<Vec<(String, Option<String>)>> {
        let packages = self.packages()?;
        let mut out = vec![];
        for provider in self.providers(name)? {
            let package = packages.iter().find(|p| p.pretty_name == provider);
            let description = package.and_then(|p| p.meta.get("description")?.as_str().map(String::from));
            out.push((provider, description));
        }
        Ok(out)
    }

    /// `PlatformRepository::isPlatformPackageDisabled()`: disabled by `config.platform` (`"ext-foo": false`).
    fn is_platform_package_disabled(&self, name: &str) -> bool {
        let _ = name;
        false
    }
}

/// `RepositoryInterface::SEARCH_*`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchMode {
    Fulltext,
    Name,
    Vendor,
}

/// `ArrayRepository::search()` over a list of packages.
pub fn array_search(packages: &[PackageRef], query: &str, mode: SearchMode, kind: Option<&str>) -> Vec<serde_json::Value> {
    use serde_json::{Value, json};
    let parts: Vec<String> = query.split_whitespace().map(|p| if mode == SearchMode::Fulltext { regex::escape(p) } else { p.to_string() }).collect();
    let Ok(regex) = regex::Regex::new(&format!("(?i)(?:{})", parts.join("|"))) else { return vec![] };
    let mut matches: Vec<(String, Value)> = vec![];
    for p in packages {
        let name = if mode == SearchMode::Vendor { p.name.split('/').next().unwrap_or("").to_string() } else { p.name.clone() };
        if matches.iter().any(|(n, _)| *n == name) || kind.is_some_and(|k| p.package_type != k) {
            continue;
        }
        let description = p.meta.get("description").and_then(Value::as_str);
        let keywords: Vec<&str> = p.meta.get("keywords").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).collect();
        let text = format!("{} {}", keywords.join(" "), description.unwrap_or(""));
        if !regex.is_match(&name) && !(mode == SearchMode::Fulltext && regex.is_match(&text)) {
            continue;
        }
        let entry = match mode {
            SearchMode::Vendor => json!({"name": name, "description": null}),
            _ => {
                let mut e = json!({"name": p.pretty_name, "description": description});
                if p.is_abandoned() {
                    e["abandoned"] = p.replacement_package().map_or(Value::Bool(true), Value::from);
                }
                e
            }
        };
        matches.push((name, entry));
    }
    matches.into_iter().map(|(_, e)| e).collect()
}

/// `SecurityAdvisory` / `PartialSecurityAdvisory` (partial ones only have id and versions).
#[derive(Clone, Debug)]
pub struct SecurityAdvisory {
    pub package_name: String,
    pub advisory_id: String,
    pub affected_versions: Constraint,
    pub affected_versions_pretty: String,
    pub title: Option<String>,
    pub cve: Option<String>,
    pub link: Option<String>,
    pub severity: Option<String>,
    pub reported_at: Option<String>,
    /// `sources` as given by the repository
    pub sources: Vec<serde_json::Value>,
}

impl SecurityAdvisory {
    pub fn is_partial(&self) -> bool {
        self.title.is_none()
    }
}

#[derive(Clone, Debug, Default)]
pub struct AdvisoryResult {
    pub names_found: Vec<String>,
    /// package name => advisories
    pub advisories: HashMap<String, Vec<SecurityAdvisory>>,
}

/// `FilterListEntry`: a package/version range on a block list such as `malware`.
#[derive(Clone, Debug)]
pub struct FilterEntry {
    pub package_name: String,
    pub list_name: String,
    pub constraint: Constraint,
    pub url: Option<String>,
    pub reason: Option<String>,
    pub id: Option<String>,
    /// who reported it (`reported by ...`), matched by `policy.malware.ignore-source`
    pub source: Option<String>,
}

/// `InstalledRepository::findPackagesWithReplacersAndProviders($name, $constraint)`.
pub fn find_with_replacers_and_providers(packages: &[PackageRef], name: &str, constraint: Option<&Constraint>) -> Vec<PackageRef> {
    let name = name.to_lowercase();
    let matching = |p: &&PackageRef| {
        if p.name == name {
            return constraint.is_none_or(|c| c.matches(&Constraint::single(komposer_semver::Op::Eq, p.version.clone())));
        }
        p.provides.iter().chain(&p.replaces).any(|l| l.target == name && constraint.is_none_or(|c| c.matches(&l.constraint)))
    };
    packages.iter().filter(matching).cloned().collect()
}

/// `StabilityFilter::isPackageAcceptable()`.
pub fn is_package_acceptable(acceptable: &HashSet<Stability>, flags: &HashMap<String, Stability>, names: &[String], stability: Stability) -> bool {
    for name in names {
        if let Some(flag) = flags.get(name) {
            if stability <= *flag {
                return true;
            }
        } else if acceptable.contains(&stability) {
            return true;
        }
    }
    false
}

/// What `FilesystemRepository::initialize()` reads from `vendor/composer/installed.json`.
pub struct InstalledJson {
    pub packages: ArrayRepository,
    /// `getDevMode()`: `None` for files written before Composer 2
    pub dev_mode: Option<bool>,
    pub dev_package_names: Vec<String>,
}

/// Reads installed.json: `{"packages": [...], "dev": .., "dev-package-names": [...]}` or the Composer 1
/// plain list. A missing file is an empty repository.
pub fn read_installed_json(path: &std::path::Path) -> Result<InstalledJson> {
    let mut installed =
        InstalledJson { packages: ArrayRepository::new("installed repo", RepoKind::Installed), dev_mode: None, dev_package_names: vec![] };
    if !path.exists() {
        return Ok(installed);
    }
    let invalid =
        |e: String| anyhow::anyhow!("Invalid repository data in {}, packages could not be loaded: [UnexpectedValueException] {e}", path.display());
    let data = crate::json::read(path).map_err(|e| invalid(format!("{e:#}")))?;
    let packages = data.get("packages").unwrap_or(&data);
    let serde_json::Value::Array(packages) = packages else { return Err(invalid("Could not parse package list from the repository".into())) };
    if let Some(serde_json::Value::Array(names)) = data.get("dev-package-names") {
        installed.dev_package_names = names.iter().filter_map(|n| n.as_str().map(String::from)).collect();
    }
    installed.dev_mode = data.get("dev").and_then(serde_json::Value::as_bool);
    let opts = crate::loader::LoadOptions { transport_options: true, ..Default::default() };
    for package in packages {
        let config = package.as_object().ok_or_else(|| invalid("Could not parse package list from the repository".into()))?;
        for p in crate::loader::load(config, opts).map_err(|e| invalid(format!("{e:#}")))? {
            installed.packages.add(p);
        }
    }
    Ok(installed)
}

/// `ArrayRepository`: an in-memory list of packages.
pub struct ArrayRepository {
    name: String,
    kind: RepoKind,
    packages: Vec<PackageRef>,
}

impl ArrayRepository {
    pub fn new(name: impl Into<String>, kind: RepoKind) -> ArrayRepository {
        ArrayRepository { name: name.into(), kind, packages: vec![] }
    }

    /// `addPackage()`: aliases also register the package they alias (after themselves), if not added yet.
    pub fn add(&mut self, package: PackageRef) {
        if self.packages.iter().any(|p| Arc::ptr_eq(p, &package)) {
            return;
        }
        let alias_of = package.alias_of.clone();
        self.packages.push(package);
        if let Some(of) = alias_of {
            self.add(of);
        }
    }

    pub fn add_package(&mut self, package: Package) -> PackageRef {
        let p = Arc::new(package);
        self.add(p.clone());
        p
    }

    pub fn remove(&mut self, package: &PackageRef) {
        self.packages.retain(|p| !Arc::ptr_eq(p, package));
    }

    pub fn all(&self) -> &[PackageRef] {
        &self.packages
    }

    pub fn len(&self) -> usize {
        self.packages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.packages.is_empty()
    }

    /// Packages without aliases (`getCanonicalPackages()` keeps the first per name).
    pub fn canonical(&self) -> Vec<PackageRef> {
        let mut seen = HashSet::new();
        self.packages.iter().filter(|p| !p.is_alias() && seen.insert(p.name.clone())).cloned().collect()
    }
}

impl Repository for ArrayRepository {
    fn repo_name(&self) -> String {
        self.name.clone()
    }

    fn kind(&self) -> RepoKind {
        self.kind
    }

    fn load_packages(&self, req: &LoadRequest) -> Result<LoadResult> {
        let constraint_for = |name: &str| req.names.iter().find(|(n, _)| n == name).map(|(_, c)| c);
        let mut result: Vec<PackageRef> = vec![];
        let mut names_found: Vec<String> = vec![];
        let push = |result: &mut Vec<PackageRef>, p: &PackageRef| {
            if !result.iter().any(|r| Arc::ptr_eq(r, p)) {
                result.push(p.clone());
            }
        };
        for p in &self.packages {
            let Some(constraint) = constraint_for(&p.name) else { continue };
            let already = req.already_loaded.get(&p.name).is_some_and(|v| v.contains(&p.version));
            if constraint.as_ref().is_none_or(|c| c.matches_version(&p.version))
                && is_package_acceptable(req.acceptable_stabilities, req.stability_flags, &p.names(true), p.stability)
                && !already
            {
                push(&mut result, p);
                // add the aliased package for packages where the alias matches
                if let Some(of) = &p.alias_of {
                    push(&mut result, of);
                }
            }
            if !names_found.contains(&p.name) {
                names_found.push(p.name.clone());
            }
        }
        // add aliases of selected packages, even if the aliases did not match
        for p in &self.packages {
            if let Some(of) = &p.alias_of
                && result.iter().any(|r| Arc::ptr_eq(r, of))
            {
                push(&mut result, p);
            }
        }
        Ok(LoadResult { names_found, packages: result })
    }

    fn packages(&self) -> Result<Vec<PackageRef>> {
        Ok(self.packages.clone())
    }
}
