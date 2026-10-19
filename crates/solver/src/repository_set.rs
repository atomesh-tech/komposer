//! `Repository\RepositorySet`: the repositories plus the root package's stability settings,
//! aliases and references, from which pools are created.

use crate::pool::Pool;
use crate::pool_builder::{PoolBuilder, PoolOptions};
use crate::request::{PrettyConstraint, Request, addr};
use anyhow::{Result, bail};
use indexmap::IndexMap;
use komposer_package::io::Io;
use komposer_package::repository::{LoadRequest, RepoKind, Repository, SecurityAdvisory, is_package_acceptable};
use komposer_package::{Package, PackageRef, RootAlias, is_platform_package};
use komposer_semver::{Constraint, Op, Stability};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

/// `BasePackage::STABILITIES`.
const STABILITIES: [Stability; 5] = [Stability::Stable, Stability::RC, Stability::Beta, Stability::Alpha, Stability::Dev];

/// A root alias: (pretty alias, normalized alias).
pub(crate) type AliasTarget = (String, String);

/// `RepositorySet`.
#[derive(Clone)]
pub struct RepositorySet {
    /// name => version => alias
    pub(crate) root_aliases: HashMap<String, HashMap<String, AliasTarget>>,
    pub(crate) root_references: HashMap<String, String>,
    repositories: Vec<Arc<dyn Repository>>,
    pub(crate) acceptable_stabilities: HashSet<Stability>,
    pub(crate) stability_flags: HashMap<String, Stability>,
    root_requires: IndexMap<String, PrettyConstraint>,
    pub(crate) temporary_constraints: IndexMap<String, PrettyConstraint>,
    allow_installed_repositories: bool,
}

/// Advisories by package name (sorted), plus the errors of repositories that could not be reached.
#[derive(Clone, Debug, Default)]
pub struct Advisories {
    pub advisories: BTreeMap<String, Vec<SecurityAdvisory>>,
    pub unreachable_repos: Vec<String>,
}

impl RepositorySet {
    /// `findPackages()` flag: include packages whose stability is not acceptable.
    pub const ALLOW_UNACCEPTABLE_STABILITIES: u8 = 1;
    /// `findPackages()` flag: look in all repositories, even after a higher priority one had the name.
    pub const ALLOW_SHADOWED_REPOSITORIES: u8 = 2;

    /// `new RepositorySet($minimumStability)`; the root settings are added with the `with_*` methods.
    pub fn new(minimum_stability: Stability) -> RepositorySet {
        RepositorySet {
            root_aliases: HashMap::new(),
            root_references: HashMap::new(),
            repositories: vec![],
            acceptable_stabilities: STABILITIES.into_iter().filter(|s| *s <= minimum_stability).collect(),
            stability_flags: HashMap::new(),
            root_requires: IndexMap::new(),
            temporary_constraints: IndexMap::new(),
            allow_installed_repositories: false,
        }
    }

    /// Package name => stability flag, from the root requirements.
    pub fn with_stability_flags(mut self, flags: HashMap<String, Stability>) -> RepositorySet {
        self.stability_flags = flags;
        self
    }

    /// Inline aliases of the root requirements.
    pub fn with_root_aliases(mut self, aliases: &[RootAlias]) -> RepositorySet {
        for a in aliases {
            let target = (a.alias.clone(), a.alias_normalized.clone());
            self.root_aliases.entry(a.package.clone()).or_default().insert(a.version.clone(), target);
        }
        self
    }

    /// Package name => source reference, from `dev-main#abc123` root requirements.
    pub fn with_root_references(mut self, references: HashMap<String, String>) -> RepositorySet {
        self.root_references = references;
        self
    }

    /// The root package's requirements (platform packages are left out).
    pub fn with_root_requires(mut self, requires: IndexMap<String, PrettyConstraint>) -> RepositorySet {
        self.root_requires = requires.into_iter().filter(|(name, _)| !is_platform_package(name)).collect();
        self
    }

    /// `--with` constraints that filter the pool during an update.
    pub fn with_temporary_constraints(mut self, constraints: IndexMap<String, PrettyConstraint>) -> RepositorySet {
        self.temporary_constraints = constraints;
        self
    }

    /// `allowInstalledRepositories()`.
    pub fn allow_installed_repositories(&mut self, allow: bool) {
        self.allow_installed_repositories = allow;
    }

    /// `addRepository()`: earlier repositories have a higher priority.
    pub fn add_repository(&mut self, repository: Arc<dyn Repository>) {
        self.repositories.push(repository);
    }

    pub fn repositories(&self) -> &[Arc<dyn Repository>] {
        &self.repositories
    }

    pub fn root_requires(&self) -> &IndexMap<String, PrettyConstraint> {
        &self.root_requires
    }

    pub fn temporary_constraints(&self) -> &IndexMap<String, PrettyConstraint> {
        &self.temporary_constraints
    }

    /// `findPackages()`: packages named `name` (or providing it) in repository priority order.
    pub fn find_packages(&self, name: &str, constraint: Option<&Constraint>, flags: u8) -> Result<Vec<PackageRef>> {
        Ok(self.find_packages_in(name, constraint, flags)?.into_iter().map(|(_, p)| p).collect())
    }

    /// `findPackages()` with the index of the repository each package comes from.
    pub(crate) fn find_packages_in(&self, name: &str, constraint: Option<&Constraint>, flags: u8) -> Result<Vec<(usize, PackageRef)>> {
        let ignore_stability = flags & Self::ALLOW_UNACCEPTABLE_STABILITIES != 0;
        let mut found = vec![];
        if flags & Self::ALLOW_SHADOWED_REPOSITORIES != 0 {
            for (i, repo) in self.repositories.iter().enumerate() {
                for p in repo.find_packages(name, constraint)? {
                    if ignore_stability || self.is_package_acceptable(&p.names(true), p.stability) {
                        found.push((i, p));
                    }
                }
            }
            return Ok(found);
        }

        let all: HashSet<Stability> = STABILITIES.into();
        let no_flags = HashMap::new();
        let names = [(name.to_string(), constraint.cloned())];
        let already_loaded = HashMap::new();
        let req = LoadRequest {
            names: &names,
            acceptable_stabilities: if ignore_stability { &all } else { &self.acceptable_stabilities },
            stability_flags: if ignore_stability { &no_flags } else { &self.stability_flags },
            already_loaded: &already_loaded,
        };
        for (i, repo) in self.repositories.iter().enumerate() {
            let result = repo.load_packages(&req)?;
            found.extend(result.packages.into_iter().map(|p| (i, p)));
            // avoid loading the same package again from other repositories once it has been found
            if result.names_found.iter().any(|n| n == name) {
                break;
            }
        }
        Ok(found)
    }

    /// `getMatchingSecurityAdvisories()`: advisories affecting these package versions.
    pub fn matching_security_advisories(&self, packages: &[PackageRef], allow_partial: bool, ignore_unreachable: bool) -> Result<Advisories> {
        matching_security_advisories(&self.repositories, packages, allow_partial, ignore_unreachable)
    }

    /// `getProviders()`: (pretty name, description) of the packages providing `name`.
    pub fn providers(&self, name: &str) -> Result<Vec<(String, Option<String>)>> {
        let mut providers: IndexMap<String, (String, Option<String>)> = IndexMap::new();
        for repo in &self.repositories {
            for (provider, description) in repo.provider_descriptions(name)? {
                providers.insert(provider.to_lowercase(), (provider, description));
            }
        }
        Ok(providers.into_values().collect())
    }

    /// `isPackageAcceptable()`.
    pub fn is_package_acceptable(&self, names: &[String], stability: Stability) -> bool {
        is_package_acceptable(&self.acceptable_stabilities, &self.stability_flags, names, stability)
    }

    fn check_installed_repositories(&self) -> Result<()> {
        if !self.allow_installed_repositories && self.repositories.iter().any(|r| r.kind() == RepoKind::Installed) {
            bail!("The pool can not accept packages from an installed repository");
        }
        Ok(())
    }

    /// Packages of the root package repository, which Composer recognizes by class.
    pub(crate) fn root_package_addresses(&self) -> HashSet<usize> {
        let roots = self.repositories.iter().filter(|r| r.kind() == RepoKind::Root);
        roots.flat_map(|r| r.packages().unwrap_or_default()).map(|p| addr(&p)).collect()
    }

    /// `createPool()`. The pool builder may lock and unlock packages of the request.
    pub fn create_pool(&self, request: &mut Request, io: &Io, options: &PoolOptions) -> Result<Pool> {
        self.check_installed_repositories()?;
        PoolBuilder::new(self, io, options).build(request)
    }

    /// `createPoolWithAllPackages()`.
    pub fn create_pool_with_all_packages(&self) -> Result<Pool> {
        self.check_installed_repositories()?;
        let mut packages = vec![];
        for repo in &self.repositories {
            for p in repo.packages()? {
                let alias = self.root_aliases.get(&p.name).and_then(|a| a.get(&p.version)).map(|a| root_alias(base_ref(&p), a));
                packages.push(p);
                packages.extend(alias);
            }
        }
        let mut pool = Pool::new(packages);
        pool.root = self.root_package_addresses();
        Ok(pool)
    }

    /// `createPoolForPackage()`.
    pub fn create_pool_for_package(&self, name: &str, locked: Option<Arc<dyn Repository>>) -> Result<Pool> {
        self.create_pool_for_packages(&[name], locked)
    }

    /// `createPoolForPackages()`: a pool with only these packages, without their dependencies.
    pub fn create_pool_for_packages(&self, names: &[&str], locked: Option<Arc<dyn Repository>>) -> Result<Pool> {
        let mut request = match locked {
            Some(repo) => Request::with_locked_repository(repo)?,
            None => Request::new(),
        };
        for name in names {
            if is_platform_package(name) {
                bail!(
                    "createPoolForPackage(s) can not be used for platform packages, as they are never loaded by the PoolBuilder which expects them to be fixed. Use createPoolWithAllPackages or pass in a proper request with the platform packages you need fixed in it."
                );
            }
            request.require_name(name, None)?;
        }
        if !names.is_empty() {
            request.restrict_packages(names.iter().map(|n| n.to_lowercase()).collect());
        }
        self.create_pool(&mut request, &Io::null(), &PoolOptions::default())
    }
}

/// `new AliasPackage($base, $aliasNormalized, $alias)` marked as root package alias.
pub(crate) fn root_alias(base: &PackageRef, (pretty, normalized): &AliasTarget) -> PackageRef {
    let mut alias = Package::alias(base, normalized, pretty);
    alias.root_package_alias = true;
    Arc::new(alias)
}

/// The non-alias package behind an alias.
fn base_ref(p: &PackageRef) -> &PackageRef {
    match &p.alias_of {
        Some(of) => base_ref(of),
        None => p,
    }
}

/// `RepositorySet::getMatchingSecurityAdvisories()` over these repositories.
pub(crate) fn matching_security_advisories(
    repositories: &[Arc<dyn Repository>],
    packages: &[PackageRef],
    allow_partial: bool,
    ignore_unreachable: bool,
) -> Result<Advisories> {
    let constraints = version_constraints(packages);
    let mut result = Advisories::default();
    for repo in repositories.iter().filter(|r| r.has_security_advisories()) {
        match repo.security_advisories(&constraints, allow_partial) {
            Ok(found) => {
                for (name, advisories) in found.advisories {
                    result.advisories.entry(name).or_default().extend(advisories);
                }
            }
            Err(e) if ignore_unreachable => result.unreachable_repos.push(e.to_string()),
            Err(e) => return Err(e),
        }
    }
    Ok(result)
}

/// Name => `== version || ...` of the packages' versions, as advisory and filter list queries take them.
pub(crate) fn version_constraints<'a>(packages: impl IntoIterator<Item = &'a PackageRef>) -> Vec<(String, Constraint)> {
    let mut by_name: IndexMap<&str, IndexMap<&str, Constraint>> = IndexMap::new();
    // root alias versions are not actual package versions
    for p in packages.into_iter().filter(|p| !(p.is_alias() && p.root_package_alias)) {
        by_name.entry(&p.name).or_default().insert(&p.version, Constraint::single(Op::Eq, p.version.clone()));
    }
    by_name.into_iter().map(|(name, versions)| (name.to_string(), Constraint::multi(versions.into_values().collect(), false))).collect()
}
