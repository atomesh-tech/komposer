//! `DependencyResolver\Pool`: the package versions the solver may pick from.

use crate::request::addr;
use indexmap::IndexMap;
use komposer_package::repository::{FilterEntry, SecurityAdvisory};
use komposer_package::{Package, PackageRef};
use komposer_semver::Constraint;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// A package id (its 1-based position in the pool), negated to mean "do not install".
pub type Literal = i32;

/// Versions removed by a policy filter: package name => normalized version => data.
pub(crate) type Removed<T> = IndexMap<String, IndexMap<String, T>>;

/// name => (any-version providers, constraint => providers)
type ProviderCache = HashMap<String, (Option<Arc<[Literal]>>, HashMap<Constraint, Arc<[Literal]>>)>;

/// `Pool`.
#[derive(Default)]
pub struct Pool {
    packages: Vec<PackageRef>,
    ids: HashMap<usize, Literal>,
    by_name: HashMap<String, Vec<Literal>>,
    provider_cache: RefCell<ProviderCache>,
    pub(crate) unacceptable_fixed_or_locked: Vec<PackageRef>,
    /// Packages of the root package repository (`RootPackageInterface`).
    pub(crate) root: HashSet<usize>,
    pub(crate) security_removed: Removed<Vec<SecurityAdvisory>>,
    pub(crate) abandoned_removed: Removed<String>,
    pub(crate) filter_list_removed: Removed<Vec<Arc<FilterEntry>>>,
    /// Versions the pool optimizer removed: name => normalized version => pretty version.
    pub(crate) removed_versions: Removed<String>,
    /// Package (kept by the optimizer) => the versions it stands for.
    pub(crate) removed_versions_by_package: HashMap<usize, IndexMap<String, String>>,
}

impl Pool {
    /// `new Pool($packages)`: ids are assigned in order, starting at 1.
    pub fn new(packages: Vec<PackageRef>) -> Pool {
        let mut pool = Pool::default();
        for (i, p) in packages.iter().enumerate() {
            let id = i as Literal + 1;
            pool.ids.insert(addr(p), id);
            for name in p.names(true) {
                pool.by_name.entry(name).or_default().push(id);
            }
        }
        pool.packages = packages;
        pool
    }

    /// A pool of `packages` carrying over this pool's metadata (what the pool filters return).
    pub(crate) fn derive(&self, packages: Vec<PackageRef>) -> Pool {
        Pool {
            unacceptable_fixed_or_locked: self.unacceptable_fixed_or_locked.clone(),
            root: self.root.clone(),
            security_removed: self.security_removed.clone(),
            abandoned_removed: self.abandoned_removed.clone(),
            filter_list_removed: self.filter_list_removed.clone(),
            removed_versions: self.removed_versions.clone(),
            removed_versions_by_package: self.removed_versions_by_package.clone(),
            ..Pool::new(packages)
        }
    }

    pub fn packages(&self) -> &[PackageRef] {
        &self.packages
    }

    /// `count()`.
    pub fn len(&self) -> usize {
        self.packages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.packages.is_empty()
    }

    /// `packageById()` / `literalToPackage()`.
    pub fn package(&self, literal: Literal) -> &PackageRef {
        &self.packages[literal.unsigned_abs() as usize - 1]
    }

    /// `$package->id`, if the package is in this pool.
    pub fn id_of(&self, package: &Package) -> Option<Literal> {
        self.ids.get(&addr(package)).copied()
    }

    /// `whatProvides()`: ids of the packages named or providing/replacing `name` that match.
    pub fn what_provides(&self, name: &str, constraint: Option<&Constraint>) -> Arc<[Literal]> {
        let cached = {
            let cache = self.provider_cache.borrow();
            cache.get(name).and_then(|(any, by_constraint)| match constraint {
                None => any.clone(),
                Some(c) => by_constraint.get(c).cloned(),
            })
        };
        if let Some(hit) = cached {
            return hit;
        }
        let found: Arc<[Literal]> = match self.by_name.get(name) {
            Some(ids) => ids.iter().copied().filter(|&id| Pool::matches(self.package(id), name, constraint)).collect(),
            None => Arc::from([]),
        };
        let mut cache = self.provider_cache.borrow_mut();
        let entry = cache.entry(name.to_string()).or_default();
        match constraint {
            None => entry.0 = Some(found.clone()),
            Some(c) => {
                entry.1.insert(c.clone(), found.clone());
            }
        }
        found
    }

    /// `match()`: the candidate is `name` in a matching version, or provides/replaces it.
    pub fn matches(candidate: &Package, name: &str, constraint: Option<&Constraint>) -> bool {
        if candidate.name == name {
            return constraint.is_none_or(|c| c.matches_version(&candidate.version));
        }
        candidate.provides.iter().chain(&candidate.replaces).any(|link| link.target == name && constraint.is_none_or(|c| c.matches(&link.constraint)))
    }

    /// `literalToPrettyString()`; `installed` holds the ids of present packages.
    pub fn literal_to_pretty_string(&self, literal: Literal, installed: &HashSet<Literal>) -> String {
        let prefix = match (installed.contains(&literal.abs()), literal > 0) {
            (true, true) => "keep",
            (true, false) => "remove",
            (false, true) => "install",
            (false, false) => "don't install",
        };
        format!("{prefix} {}", self.package(literal).pretty_string())
    }

    pub fn is_unacceptable_fixed_or_locked_package(&self, package: &Package) -> bool {
        self.unacceptable_fixed_or_locked.iter().any(|p| addr(p) == addr(package))
    }

    /// `instanceof RootPackageInterface`.
    pub fn is_root_package(&self, package: &Package) -> bool {
        self.root.contains(&addr(package))
    }

    /// `getRemovedVersions()`: versions of `name` matching `constraint` that the optimizer removed.
    pub fn removed_versions(&self, name: &str, constraint: &Constraint) -> impl Iterator<Item = (&String, &String)> {
        let versions = self.removed_versions.get(name).into_iter().flatten();
        versions.filter(move |(version, _)| constraint.matches_version(version))
    }

    /// `getRemovedVersionsByPackage()`: the removed versions a kept package stands for.
    pub fn removed_versions_by_package(&self, package: &Package) -> impl Iterator<Item = (&String, &String)> {
        self.removed_versions_by_package.get(&addr(package)).into_iter().flatten()
    }

    /// `isSecurityRemovedPackageVersion()`.
    pub fn is_security_removed(&self, name: &str, constraint: &Constraint) -> bool {
        removed(&self.security_removed, name, constraint).next().is_some()
    }

    /// `getSecurityAdvisoryIdentifiersForPackageVersion()`.
    pub fn security_advisory_ids(&self, name: &str, constraint: &Constraint) -> Vec<String> {
        let first = removed(&self.security_removed, name, constraint).next();
        first.map(|advisories| advisories.iter().map(|a| a.advisory_id.clone()).collect()).unwrap_or_default()
    }

    /// `isAbandonedRemovedPackageVersion()`.
    pub fn is_abandoned_removed(&self, name: &str, constraint: &Constraint) -> bool {
        removed(&self.abandoned_removed, name, constraint).next().is_some()
    }

    /// `isFilterListRemovedPackageVersion()`.
    pub fn is_filter_list_removed(&self, name: &str, constraint: &Constraint) -> bool {
        removed(&self.filter_list_removed, name, constraint).next().is_some()
    }

    /// `getFilterListEntryForPackageVersion()`: list name => "flagged as malware reported by ... (see url) reason: ...".
    pub fn filter_list_entry_descriptions(&self, name: &str, constraint: &Constraint) -> IndexMap<String, String> {
        let mut lists: IndexMap<&str, Vec<String>> = IndexMap::new();
        let mut seen: HashSet<*const FilterEntry> = HashSet::new();
        for entries in removed(&self.filter_list_removed, name, constraint) {
            for entry in entries.iter().filter(|e| seen.insert(Arc::as_ptr(e))) {
                let source = entry.source.as_deref().filter(|s| !s.is_empty()).map(|s| format!(" reported by {s}")).unwrap_or_default();
                let url = entry.url.as_deref().filter(|u| !u.is_empty()).map(|u| format!(" (see {u})")).unwrap_or_default();
                let reason = entry.reason.as_deref().filter(|r| !r.is_empty()).map(|r| format!(" reason: {r}")).unwrap_or_default();
                lists.entry(&entry.list_name).or_default().push(source + &url + &reason);
            }
        }
        lists
            .into_iter()
            .map(|(list, entries)| {
                let action = if list == "malware" { "flagged as " } else { "filtered by " };
                (list.to_string(), format!("{action}{list}{}", entries.join(", ")))
            })
            .collect()
    }
}

/// Data of the removed versions of `name` matching `constraint`.
fn removed<'a, T>(map: &'a Removed<T>, name: &str, constraint: &'a Constraint) -> impl Iterator<Item = &'a T> {
    map.get(name).into_iter().flatten().filter(|(version, _)| constraint.matches_version(version)).map(|(_, data)| data)
}
