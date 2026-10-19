//! `DependencyResolver\Request`: what the solver has to achieve.

use anyhow::{Result, bail};
use indexmap::IndexMap;
use komposer_package::repository::Repository;
use komposer_package::{Package, PackageRef};
use komposer_semver::Constraint;
use std::collections::HashSet;
use std::sync::Arc;

/// A constraint plus `ConstraintInterface::getPrettyString()`, which Composer prints in problems.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrettyConstraint {
    pub constraint: Constraint,
    pub pretty: String,
}

impl PrettyConstraint {
    pub fn new(constraint: Constraint, pretty: impl Into<String>) -> PrettyConstraint {
        PrettyConstraint { constraint, pretty: pretty.into() }
    }

    /// A constraint without pretty string, which then prints as `__toString()`.
    pub fn plain(constraint: Constraint) -> PrettyConstraint {
        let pretty = constraint.to_string();
        PrettyConstraint { constraint, pretty }
    }

    /// `VersionParser::parseConstraints()`, which keeps the input as pretty string.
    pub fn parse(constraint: &str) -> Result<PrettyConstraint> {
        Ok(PrettyConstraint::new(komposer_semver::parse_constraints(constraint)?, constraint))
    }
}

/// `Request::UPDATE_*`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateAllowTransitive {
    /// `UPDATE_ONLY_LISTED`
    OnlyListed,
    /// `UPDATE_LISTED_WITH_TRANSITIVE_DEPS_NO_ROOT_REQUIRE`
    WithDepsNoRootRequire,
    /// `UPDATE_LISTED_WITH_TRANSITIVE_DEPS`
    WithDeps,
}

/// Composer's `spl_object_id()`: packages are compared by identity.
pub(crate) fn addr(p: &Package) -> usize {
    p as *const Package as usize
}

/// An insertion-ordered set of packages keyed by identity.
#[derive(Clone, Default)]
pub(crate) struct PackageSet {
    list: Vec<PackageRef>,
    ptrs: HashSet<usize>,
}

impl PackageSet {
    pub(crate) fn insert(&mut self, p: PackageRef) {
        if self.ptrs.insert(addr(&p)) {
            self.list.push(p);
        }
    }

    pub(crate) fn remove(&mut self, p: &Package) {
        if self.ptrs.remove(&addr(p)) {
            self.list.retain(|x| addr(x) != addr(p));
        }
    }

    pub(crate) fn contains(&self, p: &Package) -> bool {
        self.ptrs.contains(&addr(p))
    }

    pub(crate) fn list(&self) -> &[PackageRef] {
        &self.list
    }
}

/// `Request`.
#[derive(Clone)]
pub struct Request {
    locked_repository: Option<Arc<dyn Repository>>,
    locked_repository_packages: Vec<PackageRef>,
    requires: IndexMap<String, PrettyConstraint>,
    fixed: PackageSet,
    locked: PackageSet,
    fixed_locked: PackageSet,
    update_allow_list: Vec<String>,
    // Composer's default of `false` behaves like this mode in both getters
    update_allow_transitive: UpdateAllowTransitive,
    restricted_packages: Option<Vec<String>>,
}

impl Default for Request {
    fn default() -> Request {
        Request {
            locked_repository: None,
            locked_repository_packages: vec![],
            requires: IndexMap::new(),
            fixed: PackageSet::default(),
            locked: PackageSet::default(),
            fixed_locked: PackageSet::default(),
            update_allow_list: vec![],
            update_allow_transitive: UpdateAllowTransitive::WithDepsNoRootRequire,
            restricted_packages: None,
        }
    }
}

impl Request {
    /// `new Request()`.
    pub fn new() -> Request {
        Request::default()
    }

    /// `new Request($lockedRepository)`. The repository's packages are read once, here.
    pub fn with_locked_repository(repo: Arc<dyn Repository>) -> Result<Request> {
        let packages = repo.packages()?;
        Ok(Request { locked_repository: Some(repo), locked_repository_packages: packages, ..Request::default() })
    }

    /// `requireName()`; `None` is `MatchAllConstraint`.
    pub fn require_name(&mut self, name: &str, constraint: Option<PrettyConstraint>) -> Result<()> {
        let name = name.to_lowercase();
        let constraint = constraint.unwrap_or_else(|| PrettyConstraint::plain(Constraint::Any));
        if let Some(existing) = self.requires.get(&name) {
            bail!(
                "Overwriting requires seems like a bug ({name} {} => {}, check why it is happening, might be a root alias",
                existing.pretty,
                constraint.pretty
            );
        }
        self.requires.insert(name, constraint);
        Ok(())
    }

    /// `fixPackage()`: present and has to remain installed (platform and root packages).
    pub fn fix_package(&mut self, package: PackageRef) {
        self.fixed.insert(package);
    }

    /// `lockPackage()`: locked to its version but removable.
    pub fn lock_package(&mut self, package: PackageRef) {
        self.locked.insert(package);
    }

    /// `fixLockedPackage()`: a lock file package that may not be removed (`composer install`).
    pub fn fix_locked_package(&mut self, package: PackageRef) {
        self.fixed.insert(package.clone());
        self.fixed_locked.insert(package);
    }

    /// `unlockPackage()`.
    pub fn unlock_package(&mut self, package: &Package) {
        self.locked.remove(package);
    }

    /// `setUpdateAllowList()`.
    pub fn set_update_allow_list(&mut self, list: Vec<String>, transitive: UpdateAllowTransitive) {
        self.update_allow_list = list;
        self.update_allow_transitive = transitive;
    }

    pub fn update_allow_list(&self) -> &[String] {
        &self.update_allow_list
    }

    /// `getUpdateAllowTransitiveDependencies()`.
    pub fn update_allow_transitive_dependencies(&self) -> bool {
        self.update_allow_transitive != UpdateAllowTransitive::OnlyListed
    }

    /// `getUpdateAllowTransitiveRootDependencies()`.
    pub fn update_allow_transitive_root_dependencies(&self) -> bool {
        self.update_allow_transitive == UpdateAllowTransitive::WithDeps
    }

    pub fn requires(&self) -> &IndexMap<String, PrettyConstraint> {
        &self.requires
    }

    pub fn fixed_packages(&self) -> &[PackageRef] {
        self.fixed.list()
    }

    pub fn is_fixed_package(&self, package: &Package) -> bool {
        self.fixed.contains(package)
    }

    /// `getLockedPackages()`.
    pub fn locked_packages(&self) -> Vec<PackageRef> {
        [self.locked.list(), self.fixed_locked.list()].concat()
    }

    pub fn is_locked_package(&self, package: &Package) -> bool {
        self.locked.contains(package) || self.fixed_locked.contains(package)
    }

    /// `getFixedOrLockedPackages()`.
    pub fn fixed_or_locked_packages(&self) -> Vec<PackageRef> {
        [self.fixed.list(), self.locked.list()].concat()
    }

    /// `getPresentMap()`: the locked repository's packages, then the fixed ones.
    pub fn present_packages(&self) -> Vec<PackageRef> {
        let mut present = PackageSet::default();
        for p in self.locked_repository_packages.iter().chain(self.fixed.list()) {
            present.insert(p.clone());
        }
        present.list
    }

    pub fn locked_repository(&self) -> Option<&Arc<dyn Repository>> {
        self.locked_repository.as_ref()
    }

    /// `getLockedRepository()->getPackages()`.
    pub fn locked_repository_packages(&self) -> &[PackageRef] {
        &self.locked_repository_packages
    }

    /// `restrictPackages()`: the pool builder only loads these names.
    pub fn restrict_packages(&mut self, names: Vec<String>) {
        self.restricted_packages = Some(names);
    }

    pub fn restricted_packages(&self) -> Option<&[String]> {
        self.restricted_packages.as_deref()
    }
}
