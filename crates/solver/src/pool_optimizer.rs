//! `DependencyResolver\PoolOptimizer`: drops package versions that cannot change the result, which
//! makes rule generation and solving cheaper and shortens version lists in problems.

use crate::policy::DefaultPolicy;
use crate::pool::{Literal, Pool};
use crate::request::{Request, addr};
use indexmap::IndexMap;
use komposer_package::Package;
use komposer_semver::{Constraint, DEFAULT_BRANCH_ALIAS, Op, compact_constraint};
use std::collections::{HashMap, HashSet};

/// name => constraint string => constraint
type ConstraintsPerPackage = HashMap<String, IndexMap<String, Constraint>>;

/// `expandDisjunctiveMultiConstraints()` results, as many links share their constraint.
type Expansions = HashMap<Constraint, Vec<(String, Constraint)>>;

struct Optimizer<'a> {
    policy: &'a DefaultPolicy,
    pool: &'a Pool,
    irremovable: HashSet<Literal>,
    require_constraints: ConstraintsPerPackage,
    conflict_constraints: ConstraintsPerPackage,
    to_remove: HashSet<Literal>,
    /// aliased package id => alias ids
    aliases: HashMap<Literal, Vec<Literal>>,
    removed_versions_by_package: HashMap<usize, IndexMap<String, String>>,
}

/// `PoolOptimizer::optimize()`.
pub fn optimize(policy: &DefaultPolicy, request: &Request, pool: Pool) -> Pool {
    let mut optimizer = Optimizer {
        policy,
        pool: &pool,
        irremovable: HashSet::new(),
        require_constraints: HashMap::new(),
        conflict_constraints: HashMap::new(),
        to_remove: HashSet::new(),
        aliases: HashMap::new(),
        removed_versions_by_package: HashMap::new(),
    };
    optimizer.prepare(request);
    optimizer.optimize_by_identical_dependencies();
    optimizer.optimize_impossible_packages_away(request);

    // applyRemovalsToPool()
    let mut packages = vec![];
    let mut removed_versions: IndexMap<String, IndexMap<String, String>> = IndexMap::new();
    for (i, p) in pool.packages().iter().enumerate() {
        if optimizer.to_remove.contains(&(i as Literal + 1)) {
            removed_versions.entry(p.name.clone()).or_default().insert(p.version.clone(), p.pretty_version.clone());
        } else {
            packages.push(p.clone());
        }
    }
    let removed_by_package = std::mem::take(&mut optimizer.removed_versions_by_package);
    let mut optimized = pool.derive(packages);
    optimized.removed_versions = removed_versions;
    optimized.removed_versions_by_package = removed_by_package;
    optimized
}

/// `extract*ConstraintsPerPackage()` with `expandDisjunctiveMultiConstraints()`: `^2.14 || ^3.3` makes two
/// groups, each keeping its best match.
fn extract(map: &mut ConstraintsPerPackage, expansions: &mut Expansions, name: &str, constraint: &Constraint) {
    let expanded = expansions.entry(constraint.clone()).or_insert_with(|| match compact_constraint(constraint) {
        Constraint::Multi { constraints, conjunctive: false } => constraints.into_iter().map(|c| (c.to_string(), c)).collect(),
        c => vec![(c.to_string(), c)],
    });
    let entry = map.entry(name.to_string()).or_default();
    for (key, c) in expanded.iter() {
        entry.entry(key.clone()).or_insert_with(|| c.clone());
    }
}

impl Optimizer<'_> {
    fn package(&self, id: Literal) -> &Package {
        self.pool.package(id)
    }

    fn prepare(&mut self, request: &Request) {
        // fixed or locked packages are irremovable
        let mut irremovable_versions: IndexMap<&str, Vec<Constraint>> = IndexMap::new();
        let fixed_or_locked = request.fixed_or_locked_packages();
        for p in &fixed_or_locked {
            irremovable_versions.entry(&p.name).or_default().push(Constraint::single(Op::Eq, p.version.clone()));
        }
        let mut expansions = Expansions::new();
        for (name, constraint) in request.requires() {
            extract(&mut self.require_constraints, &mut expansions, name, &constraint.constraint);
        }
        for (i, p) in self.pool.packages().iter().enumerate() {
            for link in &p.requires {
                extract(&mut self.require_constraints, &mut expansions, &link.target, &link.constraint);
            }
            for link in &p.conflicts {
                extract(&mut self.conflict_constraints, &mut expansions, &link.target, &link.constraint);
            }
            // aliases and aliased packages are kept together
            if let Some(of_id) = p.alias_of.as_ref().and_then(|of| self.pool.id_of(of)) {
                self.aliases.entry(of_id).or_default().push(i as Literal + 1);
            }
        }

        let constraints: HashMap<&str, Constraint> = irremovable_versions
            .into_iter()
            .map(|(name, mut versions)| {
                let c = if versions.len() == 1 { versions.remove(0) } else { Constraint::Multi { constraints: versions, conjunctive: false } };
                (name, c)
            })
            .collect();
        for i in 0..self.pool.len() {
            let id = i as Literal + 1;
            let p = self.package(id);
            if constraints.get(p.name.as_str()).is_some_and(|c| c.matches_version(&p.version)) {
                self.mark_irremovable(id);
            }
        }
    }

    fn mark_irremovable(&mut self, id: Literal) {
        self.irremovable.insert(id);
        if let Some(of_id) = self.package(id).alias_of.as_ref().and_then(|of| self.pool.id_of(of)) {
            // recursing so all aliases of the aliased package are marked too
            self.mark_irremovable(of_id);
        }
        if let Some(aliases) = self.aliases.get(&id) {
            self.irremovable.extend(aliases.iter().copied());
        }
    }

    /// `optimizeByIdenticalDependencies()`: per required constraint, only the preferred one of the versions
    /// with identical dependencies can matter.
    fn optimize_by_identical_dependencies(&mut self) {
        let mut identical: IndexMap<String, IndexMap<String, IndexMap<usize, Vec<Literal>>>> = IndexMap::new();
        // dependency hashes are long (laravel/framework's lists dozens of links), so groups use their index
        let mut hashes: HashMap<String, usize> = HashMap::new();
        let mut group = String::new();
        for i in 0..self.pool.len() {
            let id = i as Literal + 1;
            if self.irremovable.contains(&id) {
                continue;
            }
            self.to_remove.insert(id);
            let p = self.package(id);
            let next = hashes.len();
            let dependency_hash = *hashes.entry(dependency_hash(p)).or_insert(next);
            // a replacement counts like the require it replaces
            let replaced: String =
                p.replaces.iter().filter(|l| l.constraint.matches_version(&p.version)).map(|l| format!("require:{}", l.constraint)).collect();
            for name in p.names(false) {
                let Some(requires) = self.require_constraints.get(&name) else { continue };
                let conflicts: String = self
                    .conflict_constraints
                    .get(&name)
                    .into_iter()
                    .flatten()
                    .filter(|(_, c)| c.matches_version(&p.version))
                    .map(|(key, _)| format!("conflict:{key}"))
                    .collect();
                for (key, require) in requires {
                    let matches = require.matches_version(&p.version);
                    if !matches && replaced.is_empty() && conflicts.is_empty() {
                        continue;
                    }
                    group.clear();
                    if matches {
                        group.push_str("require:");
                        group.push_str(key);
                    }
                    group.push_str(&replaced);
                    group.push_str(&conflicts);
                    entry(entry(&mut identical, &name), &group).entry(dependency_hash).or_default().push(id);
                }
            }
        }

        for (name, groups) in &identical {
            for ids in groups.values().flat_map(|g| g.values()) {
                if let [only] = ids[..] {
                    self.keep(only, name, ids);
                    continue;
                }
                for preferred in self.policy.select_preferred_packages(self.pool, ids, None) {
                    self.keep(preferred.abs(), name, ids);
                }
            }
        }
    }

    /// `keepPackageInGroup()`.
    fn keep(&mut self, id: Literal, name: &str, group: &[Literal]) {
        // record versions even if already kept, the package may be in groups for several names
        self.record_removed_versions(id, name, group);
        if !self.to_remove.remove(&id) {
            return;
        }
        let of_id = self.package(id).alias_of.as_ref().and_then(|of| self.pool.id_of(of));
        if let Some(of_id) = of_id {
            self.to_remove.remove(&of_id);
            self.record_removed_versions(of_id, name, group);
        }
        for alias in self.aliases.get(&of_id.unwrap_or(id)).cloned().unwrap_or_default() {
            self.to_remove.remove(&alias);
            self.record_removed_versions(alias, name, group);
        }
    }

    /// The versions of `group` (default branch aliases as the branch), as the ones `id` stands in for.
    fn record_removed_versions(&mut self, id: Literal, name: &str, group: &[Literal]) {
        let pool = self.pool;
        let p = pool.package(id);
        // `getNames(false)`
        if p.name != name && !p.replaces.iter().any(|l| l.target == name) {
            return;
        }
        let recorded = self.removed_versions_by_package.entry(addr(p)).or_default();
        for &member in group {
            let mut m = pool.package(member);
            if let Some(of) = m.alias_of.as_ref().filter(|_| m.pretty_version == DEFAULT_BRANCH_ALIAS) {
                m = of;
            }
            match recorded.get_mut(&m.version) {
                Some(pretty) => pretty.clone_from(&m.pretty_version),
                None => {
                    recorded.insert(m.version.clone(), m.pretty_version.clone());
                }
            }
        }
    }

    /// `optimizeImpossiblePackagesAway()`: versions the locked packages' requirements rule out.
    fn optimize_impossible_packages_away(&mut self, request: &Request) {
        let locked = request.locked_packages();
        if locked.is_empty() {
            return;
        }
        let mut index: HashMap<&str, IndexMap<Literal, &Package>> = HashMap::new();
        for (i, p) in self.pool.packages().iter().enumerate() {
            let id = i as Literal + 1;
            // irremovable, aliased, alias, fixed and locked packages stay
            if self.irremovable.contains(&id)
                || self.aliases.contains_key(&id)
                || p.is_alias()
                || request.is_fixed_package(p)
                || request.is_locked_package(p)
            {
                continue;
            }
            index.entry(&p.name).or_default().insert(id, p);
        }

        for p in &locked {
            // a locked package that nothing requires may get uninstalled, so its requirements do not apply
            if !p.names(false).iter().any(|n| self.require_constraints.contains_key(n)) {
                continue;
            }
            for link in &p.requires {
                let Some(candidates) = index.get_mut(link.target.as_str()) else { continue };
                candidates.retain(|id, required| {
                    let keep = link.constraint.matches_version(&required.version);
                    if !keep {
                        self.to_remove.insert(*id);
                    }
                    keep
                });
            }
        }
    }
}

/// `map.entry(key.to_owned()).or_default()`, without allocating the key when it is there already.
fn entry<'m, V: Default>(map: &'m mut IndexMap<String, V>, key: &str) -> &'m mut V {
    let i = match map.get_index_of(key) {
        Some(i) => i,
        None => map.insert_full(key.to_owned(), V::default()).0,
    };
    &mut map[i]
}

/// `calculateDependencyHash()`.
fn dependency_hash(p: &Package) -> String {
    let mut hash = String::new();
    for (key, links) in [("requires", &p.requires), ("conflicts", &p.conflicts), ("replaces", &p.replaces), ("provides", &p.provides)] {
        if links.is_empty() {
            continue;
        }
        hash += key;
        hash.push(':');
        let mut by_target: Vec<(&str, String)> = vec![];
        for link in links {
            let constraint = link.constraint.to_string();
            match by_target.iter_mut().find(|(t, _)| *t == link.target) {
                Some(entry) => entry.1 = constraint,
                None => by_target.push((&link.target, constraint)),
            }
        }
        by_target.sort_by(|a, b| a.0.cmp(b.0));
        for (target, constraint) in by_target {
            hash += &format!("{target}@{constraint}");
        }
    }
    hash
}
