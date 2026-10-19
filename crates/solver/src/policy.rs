//! `DependencyResolver\DefaultPolicy`: which of several candidates the solver tries first.

use crate::pool::{Literal, Pool};
use indexmap::IndexMap;
use komposer_package::Package;
use komposer_semver::{Op, Stability};
use std::cmp::Ordering;
use std::collections::HashMap;

/// `DefaultPolicy`.
#[derive(Clone, Debug, Default)]
pub struct DefaultPolicy {
    pub prefer_stable: bool,
    pub prefer_lowest: bool,
    /// `COMPOSER_PREFER_DEV_OVER_PRERELEASE`
    pub prefer_dev_over_prerelease: bool,
    /// package name => normalized version, preferred if available (`--minimal-changes`)
    pub preferred_versions: Option<HashMap<String, String>>,
}

impl DefaultPolicy {
    /// `new DefaultPolicy($preferStable, $preferLowest, $preferredVersions)`.
    pub fn new(prefer_stable: bool, prefer_lowest: bool, preferred_versions: Option<HashMap<String, String>>) -> DefaultPolicy {
        let prefer_dev_over_prerelease = std::env::var("COMPOSER_PREFER_DEV_OVER_PRERELEASE").is_ok_and(|v| !v.is_empty() && v != "0");
        DefaultPolicy { prefer_stable, prefer_lowest, prefer_dev_over_prerelease, preferred_versions }
    }

    /// `versionCompare()`: `a op b`, where `op` is `<`, `>` or `==`.
    pub fn version_compare(&self, a: &Package, b: &Package, op: Op) -> bool {
        if self.prefer_stable && a.stability != b.stability {
            let (mut sa, mut sb) = (a.stability, b.stability);
            if self.prefer_lowest && self.prefer_dev_over_prerelease && sa != Stability::Stable && sb != Stability::Stable {
                // dev counts as more stable than alpha/beta/RC so lowest versions can be tested with fixes applied
                let dev_as_stable = |s| if s == Stability::Dev { Stability::Stable } else { s };
                (sa, sb) = (dev_as_stable(sa), dev_as_stable(sb));
            }
            return sa < sb;
        }

        // dev branches are compared like Constraint::matchSpecific(.., true) does
        let (a_branch, b_branch) = (a.version.starts_with("dev-"), b.version.starts_with("dev-"));
        if (a.is_dev() && a_branch) || (b.is_dev() && b_branch) {
            if a_branch && b_branch {
                return op == Op::Eq && a.version == b.version;
            }
            let ord = komposer_semver::version_compare(&a.version, &b.version);
            return match op {
                Op::Lt => ord == Ordering::Less,
                Op::Gt => ord == Ordering::Greater,
                _ => ord == Ordering::Equal,
            };
        }
        komposer_semver::compare(&a.version, op, &b.version)
    }

    /// `selectPreferredPackages()`: the best candidates, best first.
    pub fn select_preferred_packages(&self, pool: &Pool, literals: &[Literal], required_package: Option<&str>) -> Vec<Literal> {
        let mut literals = literals.to_vec();
        literals.sort_unstable();

        let mut by_name: IndexMap<&str, Vec<Literal>> = IndexMap::new();
        for &l in &literals {
            by_name.entry(pool.package(l).name.as_str()).or_default().push(l);
        }

        let mut selected = vec![];
        for (_, mut group) in by_name {
            group.sort_by(|&a, &b| self.compare_by_priority(pool, a, b, required_package, true));
            selected.extend(self.prune_remote_aliases(pool, self.prune_to_best_version(pool, &group)));
        }

        // now sort across all packages to respect replaces across packages
        // ponytail: Rust's stable sort, which matches PHP's zend_sort unless replace relations make the comparison
        // non-transitive (a replacer cycle across 3+ names); port zend_sort if such a case ever shows up
        selected.sort_by(|&a, &b| self.compare_by_priority(pool, a, b, required_package, false));
        selected
    }

    /// `compareByPriority()` on two literals.
    pub fn compare_by_priority(&self, pool: &Pool, a: Literal, b: Literal, required_package: Option<&str>, ignore_replace: bool) -> Ordering {
        let (pa, pb) = (pool.package(a), pool.package(b));
        // prefer aliases to the original package
        if pa.name == pb.name && pa.is_alias() != pb.is_alias() {
            return if pa.is_alias() { Ordering::Less } else { Ordering::Greater };
        }

        if !ignore_replace {
            // return original, not replaced
            if replaces(pa, pb) {
                return Ordering::Greater;
            }
            if replaces(pb, pa) {
                return Ordering::Less;
            }

            // for replacers not replacing each other, prefer the vendor of the required package
            if let Some((vendor, _)) = required_package.and_then(|r| r.split_once('/')) {
                let (a_same, b_same) = (pa.name.starts_with(vendor), pb.name.starts_with(vendor));
                if a_same != b_same {
                    return if a_same { Ordering::Less } else { Ordering::Greater };
                }
            }
        }

        // priority equal, sort by package id to make reproducible
        a.abs().cmp(&b.abs())
    }

    /// `pruneToBestVersion()`.
    fn prune_to_best_version(&self, pool: &Pool, literals: &[Literal]) -> Vec<Literal> {
        if let Some(preferred) = &self.preferred_versions {
            let name = &pool.package(literals[0]).name;
            if let Some(version) = preferred.get(name) {
                let best: Vec<Literal> = literals.iter().copied().filter(|&l| pool.package(l).version == *version).collect();
                if !best.is_empty() {
                    return best;
                }
            }
        }

        let op = if self.prefer_lowest { Op::Lt } else { Op::Gt };
        let mut best = vec![literals[0]];
        let mut best_package = pool.package(literals[0]);
        for &l in &literals[1..] {
            let package = pool.package(l);
            if self.version_compare(package, best_package, op) {
                best_package = package;
                best = vec![l];
            } else if self.version_compare(package, best_package, Op::Eq) {
                best.push(l);
            }
        }
        best
    }

    /// `pruneRemoteAliases()`: root aliases win over branch aliases.
    fn prune_remote_aliases(&self, pool: &Pool, literals: Vec<Literal>) -> Vec<Literal> {
        let is_root_alias = |&l: &Literal| {
            let p = pool.package(l);
            p.is_alias() && p.root_package_alias
        };
        if literals.iter().any(is_root_alias) { literals.into_iter().filter(is_root_alias).collect() } else { literals }
    }
}

/// `replaces()`: source replaces the target's name (constraints ignored, this is for priorities only).
fn replaces(source: &Package, target: &Package) -> bool {
    source.replaces.iter().any(|l| l.target == target.name)
}
