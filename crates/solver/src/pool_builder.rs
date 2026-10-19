//! `DependencyResolver\PoolBuilder`: loads the package versions that can matter for a request,
//! following requirements lazily through the repositories.

use crate::filter::name_to_regex;
use crate::policy::DefaultPolicy;
use crate::policy_filter::{BlockScope, PolicyOptions, filter_list_filter, security_advisory_filter};
use crate::pool::Pool;
use crate::pool_optimizer::optimize;
use crate::problem::is_symlinked_path;
use crate::repository_set::{RepositorySet, root_alias};
use crate::request::{Request, addr};
use crate::transaction::{KNOWN_DIST_HOSTS, replace_sha_in_url};
use anyhow::{Result, bail};
use indexmap::IndexMap;
use komposer_package::io::{Io, Verbosity};
use komposer_package::repository::{LoadRequest, RepoKind, Repository};
use komposer_package::{Package, PackageRef, is_platform_package};
use komposer_semver::{Constraint, compact_constraint, is_subset_of};
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

const LOAD_BATCH_SIZE: usize = 50;

/// What `createPool()` filters.
#[derive(Clone, Default)]
pub struct PoolOptions {
    /// Packages of these types are ignored.
    pub ignored_types: Vec<String>,
    /// If set, only packages of these types are allowed.
    pub allowed_types: Option<Vec<String>>,
    /// Runs `SecurityAdvisoryPoolFilter` (which also blocks abandoned packages).
    pub security_advisory_filter: Option<PolicyOptions>,
    /// Runs `FilterListPoolFilter`.
    pub filter_list_filter: Option<FilterListFilter>,
    /// Runs `PoolOptimizer` with the policy the solver will use.
    pub optimizer: Option<DefaultPolicy>,
}

/// `FilterListPoolFilter` settings.
#[derive(Clone)]
pub struct FilterListFilter {
    pub policy: PolicyOptions,
    pub scope: BlockScope,
    /// The configured repositories, which provide the filter lists.
    pub repositories: Vec<Arc<dyn Repository>>,
}

impl PoolOptions {
    /// What `Installer::doUpdate()` uses; `repositories` are the configured repositories.
    pub fn for_update(
        solver_policy: &DefaultPolicy,
        policy: &PolicyOptions,
        update_mirrors: bool,
        repositories: &[Arc<dyn Repository>],
    ) -> PoolOptions {
        PoolOptions {
            // COMPOSER_POOL_OPTIMIZER=0 is a debugging switch
            optimizer: std::env::var("COMPOSER_POOL_OPTIMIZER").map_or(true, |v| v != "0").then(|| solver_policy.clone()),
            security_advisory_filter: (policy.enabled && policy.advisories_block && !update_mirrors).then(|| policy.clone()),
            // locked packages are also checked against install-scope lists during updates
            filter_list_filter: (policy.enabled
                && !(policy.active_block_lists(BlockScope::Update).is_empty() && policy.active_block_lists(BlockScope::Install).is_empty()))
            .then(|| FilterListFilter { policy: policy.clone(), scope: BlockScope::Update, repositories: repositories.to_vec() }),
            ..PoolOptions::default()
        }
    }

    /// The filters `Installer::doInstall()` uses; `repositories` are the configured repositories.
    pub fn for_install(policy: &PolicyOptions, repositories: &[Arc<dyn Repository>]) -> PoolOptions {
        PoolOptions {
            filter_list_filter: (policy.enabled && !policy.active_block_lists(BlockScope::Install).is_empty()).then(|| FilterListFilter {
                policy: policy.clone(),
                scope: BlockScope::Install,
                repositories: repositories.to_vec(),
            }),
            ..PoolOptions::default()
        }
    }
}

/// A loaded package and the index of the repository it was loaded from.
type Loaded = Option<(PackageRef, Option<usize>)>;

pub(crate) struct PoolBuilder<'a> {
    set: &'a RepositorySet,
    io: &'a Io,
    options: &'a PoolOptions,
    root: HashSet<usize>,
    /// aliased package => (index, alias)
    alias_map: HashMap<usize, Vec<(usize, PackageRef)>>,
    packages_to_load: IndexMap<String, Constraint>,
    loaded_packages: HashMap<String, Constraint>,
    loaded_per_repo: Vec<HashMap<String, HashSet<String>>>,
    packages: Vec<Loaded>,
    unacceptable: Vec<PackageRef>,
    update_allow_list: Vec<Regex>,
    /// names of locked packages whose remote versions were not loaded (by name and replaced names)
    skipped_load: HashMap<String, Vec<PackageRef>>,
    /// locked symlinked path repo packages, unlocked without update propagation
    path_repo_unlocked: HashSet<String>,
    /// root requirements which already have their whole range loaded
    max_extended_reqs: HashSet<String>,
    update_allow_warned: HashSet<String>,
    restricted: Option<HashSet<String>>,
    /// original => copy with the root reference applied (Composer mutates the package instead)
    referenced: HashMap<usize, (PackageRef, PackageRef)>,
}

impl<'a> PoolBuilder<'a> {
    pub(crate) fn new(set: &'a RepositorySet, io: &'a Io, options: &'a PoolOptions) -> PoolBuilder<'a> {
        PoolBuilder {
            set,
            io,
            options,
            root: set.root_package_addresses(),
            alias_map: HashMap::new(),
            packages_to_load: IndexMap::new(),
            loaded_packages: HashMap::new(),
            loaded_per_repo: vec![HashMap::new(); set.repositories().len()],
            packages: vec![],
            unacceptable: vec![],
            update_allow_list: vec![],
            skipped_load: HashMap::new(),
            path_repo_unlocked: HashSet::new(),
            max_extended_reqs: HashSet::new(),
            update_allow_warned: HashSet::new(),
            restricted: None,
            referenced: HashMap::new(),
        }
    }

    /// `buildPool()`.
    pub(crate) fn build(mut self, request: &mut Request) -> Result<Pool> {
        self.restricted = request.restricted_packages().map(|names| names.iter().cloned().collect());

        if !request.update_allow_list().is_empty() {
            self.update_allow_list = request.update_allow_list().iter().map(|p| name_to_regex(p)).collect();
            self.warn_about_non_matching_update_allow_list(request)?;
            for locked in request.locked_repository_packages().to_vec() {
                if self.update_allow_list.iter().any(|re| re.is_match(&locked.name)) {
                    continue;
                }
                // remember which packages we skipped loading remote content for in this partial update
                self.skipped_load.entry(locked.name.clone()).or_default().push(locked.clone());
                for link in &locked.replaces {
                    self.skipped_load.entry(link.target.clone()).or_default().push(locked.clone());
                }
                // symlinked path repo packages are never loaded from the lock file to keep them in sync; they are
                // unlocked, but without update propagation
                if is_symlinked_path(&locked) {
                    self.path_repo_unlocked.insert(locked.name.clone());
                    continue;
                }
                request.lock_package(locked);
            }
        }

        for p in request.fixed_or_locked_packages() {
            // fixed packages do not need to trigger loading any packages
            self.loaded_packages.insert(p.name.clone(), Constraint::Any);
            // replace means conflict, so a name replaced by a fixed package does not need loading either
            for link in &p.replaces {
                self.loaded_packages.insert(link.target.clone(), Constraint::Any);
            }
            if self.root.contains(&addr(&p)) || self.is_platform_repo_package(&p)? || self.set.is_package_acceptable(&p.names(true), p.stability) {
                self.load_package(request, p, false, None);
            } else {
                self.unacceptable.push(p);
            }
        }

        for (name, constraint) in request.requires() {
            // fixed and locked packages have already been added
            if self.loaded_packages.contains_key(name) {
                continue;
            }
            self.packages_to_load.insert(name.clone(), constraint.constraint.clone());
            self.max_extended_reqs.insert(name.clone());
        }

        while !self.packages_to_load.is_empty() {
            self.load_packages_marked_for_loading(request)?;
        }

        if !self.set.temporary_constraints.is_empty() {
            self.apply_temporary_constraints();
        }

        let mut pool = Pool::new(self.packages.into_iter().flatten().map(|(p, _)| p).collect());
        pool.unacceptable_fixed_or_locked = self.unacceptable;
        pool.root = self.root;
        self.io.write_error_at("Built pool.", Verbosity::Debug);

        // filter vulnerable packages before optimizing the pool, the optimizer could otherwise remove versions needed
        if let Some(policy) = &self.options.security_advisory_filter {
            pool = run_filter(self.io, "Security advisory pool filter", "were filtered away", pool, |pool| {
                security_advisory_filter(pool, self.set.repositories(), request, policy, self.io)
            })?;
        }
        if let Some(f) = &self.options.filter_list_filter {
            pool = run_filter(self.io, "Filter list pool filter", "were filtered away by dependency policies", pool, |pool| {
                filter_list_filter(pool, &f.repositories, request, &f.policy, f.scope, self.io)
            })?;
        }
        if let Some(policy) = &self.options.optimizer {
            pool = run_filter(self.io, "Pool optimizer", "were optimized away", pool, |pool| Ok(optimize(policy, request, pool)))?;
        }
        Ok(pool)
    }

    /// `instanceof PlatformRepository` for a fixed package.
    fn is_platform_repo_package(&self, p: &Package) -> Result<bool> {
        for repo in self.set.repositories().iter().filter(|r| r.kind() == RepoKind::Platform) {
            if repo.packages()?.iter().any(|q| addr(q) == addr(p)) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Removes the packages (with their aliases) not matching the `--with` temporary constraints.
    fn apply_temporary_constraints(&mut self) {
        for i in 0..self.packages.len() {
            let Some((package, _)) = self.packages[i].clone() else { continue };
            // aliases are checked along with the package they alias
            if package.is_alias() {
                continue;
            }
            for name in package.names(true) {
                let Some(constraint) = self.set.temporary_constraints.get(&name) else { continue };
                let mut indices = vec![(i, package.clone())];
                indices.extend(self.alias_map.get(&addr(&package)).into_iter().flatten().cloned());
                if !indices.iter().any(|(_, p)| constraint.constraint.matches_version(&p.version)) {
                    for (index, _) in indices {
                        self.packages[index] = None;
                    }
                }
            }
        }
    }

    /// `markPackageNameForLoading()`.
    fn mark_package_name_for_loading(&mut self, request: &Request, name: &str, constraint: &Constraint) {
        // platform requirements are skipped at this stage, and a root require (which was not unlocked)
        // already loaded the maximum range
        if is_platform_package(name) || self.max_extended_reqs.contains(name) {
            return;
        }

        // root requires can not be overruled by dependencies, so do not extend beyond them (this is
        // triggered by a root require which was locked but got unlocked)
        let mut constraint = constraint.clone();
        if let Some(root) = request.requires().get(name)
            && !is_subset_of(&constraint, &root.constraint)
        {
            constraint = root.constraint.clone();
        }

        let Some(loaded) = self.loaded_packages.get(name) else {
            // maybe it was already marked but not loaded yet, then the constraint has to be extended
            if let Some(existing) = self.packages_to_load.get(name) {
                if is_subset_of(&constraint, existing) {
                    return;
                }
                constraint = compact_constraint(&Constraint::multi(vec![existing.clone(), constraint], false));
            }
            self.packages_to_load.insert(name.to_string(), constraint);
            return;
        };

        // no need to load packages for a subset of the constraint they were already loaded with
        if is_subset_of(&constraint, loaded) {
            return;
        }
        // extend the constraint and mark the package as not loaded so the missing versions are loaded
        let extended = compact_constraint(&Constraint::multi(vec![loaded.clone(), constraint], false));
        self.packages_to_load.shift_remove(name);
        self.packages_to_load.insert(name.to_string(), extended);
        self.loaded_packages.remove(name);
    }

    /// `loadPackagesMarkedForLoading()`.
    fn load_packages_marked_for_loading(&mut self, request: &mut Request) -> Result<()> {
        let to_load = std::mem::take(&mut self.packages_to_load);
        let mut names: Vec<(String, Option<Constraint>)> = vec![];
        for (name, constraint) in to_load {
            if self.restricted.as_ref().is_some_and(|r| !r.contains(&name)) {
                continue;
            }
            self.loaded_packages.insert(name.clone(), constraint.clone());
            names.push((name, Some(constraint)));
        }

        // load in chunks of 50 to prevent memory usage build-up due to caches of all sorts
        let mut batches: Vec<Vec<(String, Option<Constraint>)>> = names.chunks(LOAD_BATCH_SIZE).map(<[_]>::to_vec).collect();
        let locked_repo = request.locked_repository().cloned();
        for (repo_index, repo) in self.set.repositories().iter().enumerate() {
            // these repos have their packages fixed or locked if they need to be loaded
            if repo.kind() == RepoKind::Platform || locked_repo.as_ref().is_some_and(|l| same_repo(l, repo)) {
                continue;
            }
            if batches.is_empty() {
                break;
            }
            for batch in &mut batches {
                let result = repo.load_packages(&LoadRequest {
                    names: batch,
                    acceptable_stabilities: &self.set.acceptable_stabilities,
                    stability_flags: &self.set.stability_flags,
                    already_loaded: &self.loaded_per_repo[repo_index],
                })?;
                // avoid loading the same package again from other repositories once it has been found
                batch.retain(|(name, _)| !result.names_found.contains(name));
                for p in result.packages {
                    self.loaded_per_repo[repo_index].entry(p.name.clone()).or_default().insert(p.version.clone());
                    let ignored = self.options.ignored_types.contains(&p.package_type)
                        || self.options.allowed_types.as_ref().is_some_and(|t| !t.contains(&p.package_type));
                    if !ignored {
                        let propagate = !self.path_repo_unlocked.contains(&p.name);
                        self.load_package(request, p, propagate, Some(repo_index));
                    }
                }
            }
            let remaining: Vec<(String, Option<Constraint>)> = batches.into_iter().flatten().collect();
            batches = remaining.chunks(LOAD_BATCH_SIZE).map(<[_]>::to_vec).collect();
        }
        Ok(())
    }

    /// `loadPackage()`.
    fn load_package(&mut self, request: &mut Request, package: PackageRef, propagate_update: bool, repo: Option<usize>) {
        let name = package.name.clone();
        // root references apply to all versions of the name, the solver picks the right one
        let package = match self.set.root_references.get(&name) {
            Some(reference) if !request.is_locked_package(&package) && !request.is_fixed_package(&package) => {
                self.with_root_reference(&package, reference)
            }
            _ => package,
        };
        let index = self.packages.len();
        self.packages.push(Some((package.clone(), repo)));
        if let Some(of) = &package.alias_of {
            self.alias_map.entry(addr(of)).or_default().push((index, package.clone()));
        }

        // fixed or locked packages (no propagation) get their root aliases loaded as separate packages, except
        // path-repo-unlocked ones
        if (propagate_update || self.path_repo_unlocked.contains(&name))
            && let Some(alias) = self.set.root_aliases.get(&name).and_then(|a| a.get(&package.version))
        {
            let base = package.alias_of.as_ref().unwrap_or(&package);
            let alias = root_alias(base, alias);
            self.alias_map.entry(addr(base)).or_default().push((self.packages.len(), alias.clone()));
            self.packages.push(Some((alias, repo)));
        }

        for link in &package.requires {
            let require = link.target.as_str();
            if !self.skipped_load.contains_key(require) {
                self.mark_package_name_for_loading(request, require, &link.constraint);
                continue;
            }
            // the required package is loaded as a locked package only, unlock it if it needs to be updated along
            if propagate_update && request.update_allow_transitive_dependencies() {
                self.unlock_if_allowed(request, require, |builder, request| {
                    builder.mark_package_name_for_loading(request, require, &link.constraint)
                });
            } else if self.path_repo_unlocked.contains(require) && !self.loaded_packages.contains_key(require) {
                // a path-repo-unlocked package not referenced by the root needs to be loaded here
                self.mark_package_name_for_loading(request, require, &link.constraint);
            }
        }

        // in a partial update with deps, replaced packages that are locked also need unlocking, as they
        // could prevent this package from being installable
        if propagate_update && request.update_allow_transitive_dependencies() {
            for link in &package.replaces {
                let replace = link.target.as_str();
                if self.loaded_packages.contains_key(replace) && self.skipped_load.contains_key(replace) {
                    // the replaced package only needs to be loaded if something else requires it
                    self.unlock_if_allowed(request, replace, |builder, request| builder.mark_package_name_for_loading_if_required(request, replace));
                }
            }
        }
    }

    /// Unlocks `name` and runs `then`, unless it is (replaced by) a root requirement and those stay locked.
    fn unlock_if_allowed(&mut self, request: &mut Request, name: &str, then: impl FnOnce(&mut Self, &Request)) {
        let skipped_root_requires = self.skipped_root_requires(request, name);
        if request.update_allow_transitive_root_dependencies() || skipped_root_requires.is_empty() {
            self.unlock_package(request, name);
            then(self, request);
            return;
        }
        for root in skipped_root_requires {
            if self.update_allow_warned.insert(root.clone()) {
                self.io.warning(&format!("Dependency {root} is also a root requirement. Package has not been listed as an update argument, so keeping locked at old version. Use --with-all-dependencies (-W) to include root dependencies."));
            }
        }
    }

    /// `getSkippedRootRequires()`.
    fn skipped_root_requires(&self, request: &Request, name: &str) -> Vec<String> {
        let Some(skipped) = self.skipped_load.get(name) else { return vec![] };
        let requires = request.requires();
        let label = |p: &Package| if p.name != name { format!("{} (via replace of {name})", p.name) } else { p.name.clone() };
        if requires.contains_key(name) {
            return skipped.iter().map(|p| label(p)).collect();
        }
        let mut matches = vec![];
        for p in skipped {
            if requires.contains_key(&p.name) {
                matches.push(p.name.clone());
            }
            if p.replaces.iter().any(|l| requires.contains_key(&l.target)) {
                matches.push(label(p));
            }
        }
        matches
    }

    /// `warnAboutNonMatchingUpdateAllowList()`.
    fn warn_about_non_matching_update_allow_list(&self, request: &Request) -> Result<()> {
        if request.locked_repository().is_none() {
            bail!("No lock repo present and yet a partial update was requested.");
        }
        'patterns: for (pattern, re) in request.update_allow_list().iter().zip(&self.update_allow_list) {
            // a locked package matches, all good
            if request.locked_repository_packages().iter().any(|p| re.is_match(&p.name)) {
                continue;
            }
            // a root require matches, probably a new package
            let mut matched_platform_package = false;
            for name in request.requires().keys().filter(|n| re.is_match(n)) {
                if !is_platform_package(name) {
                    continue 'patterns;
                }
                matched_platform_package = true;
            }
            self.io.warning(&if matched_platform_package {
                format!("Pattern \"{pattern}\" listed for update matches platform packages, but these cannot be updated by Composer.")
            } else if pattern.contains('*') {
                format!("Pattern \"{pattern}\" listed for update does not match any locked packages.")
            } else {
                format!("Package \"{pattern}\" listed for update is not locked.")
            });
        }
        Ok(())
    }

    /// `unlockPackage()`: a partial update with transitive deps found that this locked package needs to be updated.
    fn unlock_package(&mut self, request: &mut Request, name: &str) {
        for replacer in self.skipped_load.get(name).cloned().unwrap_or_default() {
            // unfixing a replaced name also unfixes the replacer, unless that was unfixed already
            let replacer_name = replacer.name.as_str();
            if replacer_name == name || !self.skipped_load.contains_key(replacer_name) {
                continue;
            }
            let (name_is_root, replacer_is_root) = (request.requires().contains_key(name), request.requires().contains_key(replacer_name));
            if request.update_allow_transitive_root_dependencies() || (!name_is_root && !replacer_is_root) {
                self.unlock_package(request, replacer_name);
                if replacer_is_root {
                    self.mark_package_name_for_loading(request, replacer_name, &Constraint::Any);
                } else {
                    for constraint in self.loaded_requires_of(replacer_name) {
                        self.mark_package_name_for_loading(request, replacer_name, &constraint);
                    }
                }
            }
        }

        if self.path_repo_unlocked.contains(name) {
            let indices: Vec<usize> = (0..self.packages.len()).filter(|&i| self.packages[i].as_ref().is_some_and(|(p, _)| p.name == name)).collect();
            for index in indices {
                self.remove_loaded_package(index);
            }
        }

        self.skipped_load.remove(name);
        self.loaded_packages.remove(name);
        self.max_extended_reqs.remove(name);
        self.path_repo_unlocked.remove(name);

        // remove the locked package by this name which was already initialized
        for locked in request.locked_packages() {
            if locked.is_alias() || locked.name != name {
                continue;
            }
            let Some(index) = self.packages.iter().position(|e| e.as_ref().is_some_and(|(p, _)| Arc::ptr_eq(p, &locked))) else {
                continue;
            };
            request.unlock_package(&locked);
            self.remove_loaded_package(index);

            // requirements on this package by other locked or fixed packages need loading now, as the locked package
            // satisfied them before, and so do packages it replaced in case an update removes the replacement
            for other in request.fixed_or_locked_packages() {
                if Arc::ptr_eq(&other, &locked) || !self.skipped_load.contains_key(&other.name) {
                    continue;
                }
                if let Some(link) = other.requires.iter().find(|l| l.target == locked.name) {
                    self.mark_package_name_for_loading(request, &locked.name, &link.constraint);
                }
                for replace in &locked.replaces {
                    if other.requires.iter().any(|l| l.target == replace.target) && self.skipped_load.contains_key(&replace.target) {
                        self.unlock_package(request, &replace.target);
                        // this package is in the requires, so no need to check whether it is required
                        self.mark_package_name_for_loading(request, &replace.target, &replace.constraint);
                    }
                }
            }
        }
    }

    /// The constraints the loaded packages require `name` with.
    fn loaded_requires_of(&self, name: &str) -> Vec<Constraint> {
        let loaded = self.packages.iter().flatten().map(|(p, _)| p);
        loaded.filter_map(|p| p.requires.iter().find(|l| l.target == name)).map(|l| l.constraint.clone()).collect()
    }

    /// `markPackageNameForLoadingIfRequired()`.
    fn mark_package_name_for_loading_if_required(&mut self, request: &Request, name: &str) {
        if let Some(root) = request.requires().get(name) {
            let constraint = root.constraint.clone();
            self.mark_package_name_for_loading(request, name, &constraint);
        }
        for constraint in self.loaded_requires_of(name) {
            self.mark_package_name_for_loading(request, name, &constraint);
        }
    }

    /// `removeLoadedPackage()`: drops the package and its aliases, so they can be loaded again.
    fn remove_loaded_package(&mut self, index: usize) {
        let Some((package, repo)) = self.packages[index].take() else { return };
        let mut forget = |p: &Package| {
            if let Some(versions) = repo.and_then(|r| self.loaded_per_repo[r].get_mut(&p.name)) {
                versions.remove(&p.version);
            }
        };
        forget(&package);
        for (alias_index, alias) in self.alias_map.remove(&addr(&package)).unwrap_or_default() {
            forget(&alias);
            self.packages[alias_index] = None;
        }
    }

    /// `setSourceDistReferences()` on a copy (and on copies of its aliases, which must alias the copy).
    fn with_root_reference(&mut self, package: &PackageRef, reference: &str) -> PackageRef {
        if let Some((_, copy)) = self.referenced.get(&addr(package)) {
            return copy.clone();
        }
        let copy = match &package.alias_of {
            Some(of) => {
                let of = self.with_root_reference(of, reference);
                let mut alias = Package::alias(&of, &package.version, &package.pretty_version);
                alias.root_package_alias = package.root_package_alias;
                Arc::new(alias)
            }
            None => {
                let mut copy = (**package).clone();
                if let Some(source) = &mut copy.source {
                    source.reference = Some(reference.to_string());
                }
                if let Some(dist) = &mut copy.dist {
                    // only bitbucket, github and gitlab dist urls allow replacing the reference
                    if KNOWN_DIST_HOSTS.is_match(&dist.url) {
                        dist.reference = Some(reference.to_string());
                        dist.url = replace_sha_in_url(&dist.url, reference);
                    } else if dist.reference.as_deref().is_some_and(|r| !r.is_empty() && r != "0") {
                        dist.reference = Some(reference.to_string());
                    }
                }
                Arc::new(copy)
            }
        };
        self.referenced.insert(addr(package), (package.clone(), copy.clone()));
        copy
    }
}

/// Repository identity (`===`).
fn same_repo(a: &Arc<dyn Repository>, b: &Arc<dyn Repository>) -> bool {
    std::ptr::eq(Arc::as_ptr(a) as *const (), Arc::as_ptr(b) as *const ())
}

/// Runs a pool filter with Composer's timing and summary output.
fn run_filter(io: &Io, label: &str, removed: &str, pool: Pool, filter: impl FnOnce(Pool) -> Result<Pool>) -> Result<Pool> {
    io.write_error_at(&format!("Running {}.", label.to_lowercase()), Verbosity::Debug);
    let before = Instant::now();
    let total = pool.len();
    let filtered_pool = filter(pool)?;
    let filtered = total - filtered_pool.len();
    if filtered == 0 {
        return Ok(filtered_pool);
    }
    io.write_at(&format!("{label} completed in {:.3} seconds", before.elapsed().as_secs_f64()), Verbosity::VeryVerbose);
    let percent = (100.0 / total as f64 * filtered as f64).round() as i64;
    io.write_at(
        &format!(
            "<info>Found {} package versions referenced in your dependency graph. {} ({percent}%) {removed}.</info>",
            number_format(total),
            number_format(filtered)
        ),
        Verbosity::VeryVerbose,
    );
    Ok(filtered_pool)
}

/// PHP's `number_format($n)`: thousands separated by commas.
fn number_format(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}
