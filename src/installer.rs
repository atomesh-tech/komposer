//! `Composer\Installer`: `install` and `update` (resolve, write composer.lock, install from it,
//! dump the autoloader, run the script events).

use crate::audit;
use crate::commands::dump_autoload;
use crate::composer::Composer;
use crate::locker::LockData;
use crate::suggest::Suggestions;
use anyhow::Result;
use indexmap::IndexMap;
use komposer_autoload::AutoloadOptions;
use komposer_installer::{FetchJob, InstallationManager, InstalledRepo};
use komposer_package::io::Verbosity;
use komposer_package::operation::Operation;
use komposer_package::repository::{ArrayRepository, RepoKind, Repository};
use komposer_package::{Link, Package, PackageRef, RootAlias, is_platform_package};
use komposer_repository::http::RequestOptions;
use komposer_repository::platform::{PhpRuntime, PlatformRepository, detected_runtime};
use komposer_semver::{Constraint, Op};
use komposer_solver::{
    DefaultPolicy, LockTransaction, PlatformRequirementFilter, PolicyOptions, PoolOptions, PrettyConstraint, RepositorySet, Request, Solver,
    SolverProblems, UpdateAllowTransitive, local_repo_transaction,
};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::sync::Arc;

pub const ERROR_GENERIC_FAILURE: i32 = 1;
pub const ERROR_NO_LOCK_FILE_FOR_PARTIAL_UPDATE: i32 = 3;
pub const ERROR_LOCK_FILE_INVALID: i32 = 4;
pub const ERROR_AUDIT_FAILED: i32 = 5;
pub const ERROR_PSR_AUTOLOAD_VIOLATION: i32 = 6;

/// Package types Composer never installs during install/update.
const IGNORED_TYPES: [&str; 2] = ["php-ext", "php-ext-zend"];

pub struct Installer<'a> {
    pub c: &'a mut Composer,
    pub dev_mode: bool,
    pub dry_run: bool,
    pub download_only: bool,
    pub update: bool,
    pub install: bool,
    pub dump_autoloader: bool,
    pub run_scripts: bool,
    pub prefer_source: bool,
    pub prefer_dist: bool,
    pub optimize_autoloader: bool,
    pub classmap_authoritative: bool,
    pub strict_psr_autoloader: bool,
    pub apcu: bool,
    pub apcu_prefix: Option<String>,
    pub prefer_stable: bool,
    pub prefer_lowest: bool,
    pub minimal_update: bool,
    pub write_lock: bool,
    pub execute_operations: bool,
    pub audit: bool,
    pub audit_format: String,
    pub error_on_audit: bool,
    pub update_mirrors: bool,
    pub update_allow_list: Option<Vec<String>>,
    pub update_allow_transitive: UpdateAllowTransitive,
    /// what the user asked to ignore (`--ignore-platform-req(s)`)
    pub platform_filter: PlatformRequirementFilter,
    /// the same for the autoloader's platform_check.php
    pub autoload_platform_ignore: (bool, Vec<String>),
    pub temporary_constraints: IndexMap<String, PrettyConstraint>,
    pub no_blocking: bool,
    /// filled after an update, for callers like `require`
    pub lock_transaction: Option<LockTransaction>,
    pub suggestions: Suggestions,
    /// Test seam: the PHP the platform repository describes, instead of the detected one.
    pub php: Option<Arc<PhpRuntime>>,
    /// Test seam (`InstallationManagerMock`): record operations instead of executing them.
    pub trace: Option<Vec<String>>,
    /// platform requirements that can not be checked because PHP is not installed
    solver_filter: PlatformRequirementFilter,
}

impl<'a> Installer<'a> {
    pub fn new(c: &'a mut Composer) -> Installer<'a> {
        let write_lock = c.config.get_bool("lock");
        Installer {
            c,
            dev_mode: true,
            dry_run: false,
            download_only: false,
            update: false,
            install: true,
            dump_autoloader: true,
            run_scripts: true,
            prefer_source: false,
            prefer_dist: false,
            optimize_autoloader: false,
            classmap_authoritative: false,
            strict_psr_autoloader: false,
            apcu: false,
            apcu_prefix: None,
            prefer_stable: false,
            prefer_lowest: false,
            minimal_update: false,
            write_lock,
            execute_operations: true,
            audit: true,
            audit_format: "summary".into(),
            error_on_audit: false,
            update_mirrors: false,
            update_allow_list: None,
            update_allow_transitive: UpdateAllowTransitive::OnlyListed,
            platform_filter: PlatformRequirementFilter::IgnoreNothing,
            autoload_platform_ignore: (false, vec![]),
            temporary_constraints: IndexMap::new(),
            no_blocking: false,
            lock_transaction: None,
            suggestions: Suggestions::default(),
            php: None,
            trace: None,
            solver_filter: PlatformRequirementFilter::IgnoreNothing,
        }
    }

    /// `Installer::run()`.
    pub fn run(&mut self) -> Result<i32> {
        if self.update_allow_list.is_some() && self.update_mirrors {
            anyhow::bail!("The installer options updateMirrors and updateAllowList are mutually exclusive.");
        }
        let installed_json = self.c.installed_json();
        let is_fresh_install = !installed_json.is_file();

        if !self.update && !self.c.locker.is_locked()? {
            let fallback =
                "Updating dependencies to latest instead of installing from lock file. See https://getcomposer.org/install for more information.";
            if !self.write_lock {
                self.c.io.warning(&format!("Lockfile creation is disabled in config. {fallback}"));
            } else {
                self.c.io.warning(&format!("No composer.lock file present. {fallback}"));
            }
            self.update = true;
        }

        if self.dry_run {
            self.run_scripts = false;
            self.execute_operations = false;
            self.write_lock = false;
            self.dump_autoloader = false;
        }
        if self.download_only || (self.update && !self.install) {
            self.dump_autoloader = false;
        }
        self.solver_filter = self.solver_platform_filter();

        if self.run_scripts {
            set_env("COMPOSER_DEV_MODE", if self.dev_mode { "1" } else { "0" });
            let event = if self.update { "pre-update-cmd" } else { "pre-install-cmd" };
            self.c.scripts(true).dispatch(event, &[])?;
        }

        let root = Some(self.c.root.clone());
        let mut local_repo = InstalledRepo::load(&installed_json.to_string_lossy(), root)?;
        if self.dry_run {
            // ponytail: dry runs keep the loaded repository in memory instead of mocking every repository
            local_repo.file = None;
        }

        let res = if self.update { self.do_update(&mut local_repo)? } else { self.do_install(&mut local_repo, false)? };
        if res != 0 {
            return Ok(res);
        }

        if self.update {
            if is_fresh_install {
                self.suggestions.add_from_package(&self.c.root.package);
            }
            let mut installed: Vec<PackageRef> = self.c.locker.locked_repository(self.dev_mode)?.packages()?;
            installed.extend(self.platform_repo(&self.c.locker.platform_overrides()?)?.packages()?);
            installed.push(Arc::new(self.c.root.package.clone()));
            self.suggestions.output_minimalistic(&self.c.io, &installed);
        }

        for p in self.c.locker.locked_repository(true)?.packages()? {
            if p.is_alias() || !p.is_abandoned() {
                continue;
            }
            let replacement = match p.replacement_package() {
                Some(r) => format!("Use {r} instead"),
                None => "No replacement was suggested".to_string(),
            };
            self.c.io.warning(&format!("Package {} is abandoned, you should avoid using it. {replacement}.", p.pretty_name));
        }

        if self.dump_autoloader {
            self.c.io.write_error(if self.optimize_autoloader {
                "<info>Generating optimized autoload files</info>"
            } else {
                "<info>Generating autoload files</info>"
            });
        }
        if self.dump_autoloader && self.trace.is_none() {
            let packages: Vec<PackageRef> = local_repo.packages.canonical();
            let opts = AutoloadOptions {
                dev_mode: Some(self.dev_mode),
                optimize: self.optimize_autoloader,
                classmap_authoritative: self.classmap_authoritative,
                apcu: self.apcu,
                apcu_prefix: self.apcu_prefix.clone(),
                lock_content_hash: self.c.locker.locked_content_hash(),
                dev_package_names: local_repo.dev_package_names.clone(),
                is_platform_req_ignored: crate::platform_filter::autoload(self.autoload_platform_ignore.0, &self.autoload_platform_ignore.1),
                ..Default::default()
            };
            let class_map = dump_autoload::dump(self.c, &packages, &opts, self.run_scripts, self.dev_mode)?;
            if self.strict_psr_autoloader && !class_map.psr_violations().is_empty() {
                return Ok(ERROR_PSR_AUTOLOAD_VIOLATION);
            }
        }

        if self.install && self.execute_operations && self.trace.is_none() {
            // force binaries re-generation in case they are missing
            let fetch = |_: Vec<FetchJob>| -> Vec<Result<()>> { vec![] };
            let im = InstallationManager::new(&self.c.config, &self.c.io, &fetch);
            for p in local_repo.packages.all() {
                im.ensure_binaries_presence(p)?;
            }
        }

        let show_funding = std::env::var("COMPOSER_FUND").ok().and_then(|v| v.parse::<i64>().ok()).is_none_or(|v| v != 0);
        if show_funding {
            let n = local_repo.packages.all().iter().filter(|p| !p.is_alias() && p.funding().is_some()).count();
            if n > 0 {
                self.c.io.write_error(&format!(
                    "<info>{n} package{} you are using {} looking for funding.</info>\n<info>Use the `composer fund` command to find out more!</info>",
                    if n == 1 { "" } else { "s" },
                    if n == 1 { "is" } else { "are" }
                ));
            }
        }

        if self.run_scripts {
            let event = if self.update { "post-update-cmd" } else { "post-install-cmd" };
            self.c.scripts(true).dispatch(event, &[])?;
        }

        if self.audit {
            let (packages, target) = if self.update && !self.install {
                (self.c.locker.locked_repository(true)?.packages()?, "locked")
            } else {
                (local_repo.packages.all().to_vec(), "installed")
            };
            if packages.is_empty() {
                self.c.io.write_error(&format!("No {target} packages - skipping audit."));
            } else {
                let mut set = RepositorySet::new(komposer_semver::Stability::Stable);
                for repo in self.c.repositories()? {
                    set.add_repository(repo);
                }
                match audit::audit_with(&self.c.io, &set, &audit::AuditOptions::from_config(&self.c.config)?, &packages, &self.audit_format, true) {
                    Ok(found) if found > 0 && self.error_on_audit => return Ok(ERROR_AUDIT_FAILED),
                    Ok(_) => {}
                    Err(e) => {
                        self.c.io.error(&format!("Failed to audit {target} packages."));
                        if self.c.io.is_verbose() {
                            self.c.io.error(&format!("{e:#}"));
                        }
                    }
                }
            }
        }
        Ok(0)
    }

    fn do_update(&mut self, local_repo: &mut InstalledRepo) -> Result<i32> {
        let overrides = self.c.config.raw("platform").and_then(Value::as_object).cloned().unwrap_or_default();
        let platform_repo = self.platform_repo(&overrides)?;
        let aliases = self.c.root.aliases.clone();

        let locked: Option<Arc<dyn Repository>> = match self.c.locker.is_locked() {
            Ok(true) => match self.c.locker.locked_repository(true) {
                Ok(r) => Some(Arc::new(r)),
                Err(e) if self.update_allow_list.is_some() || self.update_mirrors => return Err(e),
                // a full update regenerates a broken lock file from scratch
                Err(_) => None,
            },
            Ok(false) => None,
            Err(e) if self.update_allow_list.is_some() || self.update_mirrors => return Err(e),
            Err(_) => None,
        };

        if (self.update_allow_list.is_some() || self.update_mirrors) && locked.is_none() {
            self.c.io.write_error_at(
                &format!(
                    "<error>Cannot update {} without a lock file present. Run `composer update` to generate a lock file.</error>",
                    if self.update_mirrors { "lock file information" } else { "only a partial set of packages" }
                ),
                Verbosity::Quiet,
            );
            return Ok(ERROR_NO_LOCK_FILE_FOR_PARTIAL_UPDATE);
        }

        self.c.io.write_error("<info>Loading composer repositories with package information</info>");

        let policy = self.create_policy(true, locked.as_deref())?;
        let (mut set, fixed_root) = self.create_repository_set(true, &platform_repo, &aliases, None)?;
        for repo in self.c.repositories()? {
            set.add_repository(repo);
        }
        if let Some(l) = &locked {
            set.add_repository(l.clone());
        }

        // a full update will almost certainly need the metadata of every locked (or installed) package:
        // fetch it in one concurrent batch instead of one dependency level after the other
        if self.update_allow_list.is_none() {
            let known: Vec<PackageRef> = match &locked {
                Some(l) => l.packages()?,
                None => local_repo.packages.packages()?,
            };
            let root = &self.c.root.package;
            let mut names: Vec<String> = root.requires.iter().chain(&root.dev_requires).map(|l| l.target.clone()).collect();
            // what they require too: virtual names like psr/log-implementation are never locked themselves
            let required = known.iter().flat_map(|p| p.requires.iter().map(|l| l.target.clone()));
            names.extend(known.iter().filter(|p| !p.is_alias()).map(|p| p.name.clone()).chain(required));
            // and what the previous update of this project ended up loading (dependencies of older versions)
            let (hinted_dev, hinted): (Vec<String>, Vec<String>) = self.prefetch_hints().into_iter().partition(|f| f.ends_with("~dev"));
            names.extend(hinted);
            names.sort();
            names.dedup();
            let mut dev_names: Vec<String> =
                names.iter().filter(|n| set.is_package_acceptable(std::slice::from_ref(*n), komposer_semver::Stability::Dev)).cloned().collect();
            dev_names.extend(hinted_dev.into_iter().map(|f| f.trim_end_matches("~dev").to_string()));
            for repo in set.repositories() {
                repo.prefetch(&names, &dev_names);
            }
        }

        let mut request = self.create_request(&fixed_root, &platform_repo, locked.clone())?;
        self.require_packages_for_update(&mut request, locked.as_deref(), true)?;
        if let Some(list) = &self.update_allow_list {
            request.set_update_allow_list(list.clone(), self.update_allow_transitive);
        }

        let configured = self.c.repositories()?;
        let pool_options = PoolOptions {
            ignored_types: IGNORED_TYPES.map(String::from).to_vec(),
            ..PoolOptions::for_update(&policy, &self.policy()?, self.update_mirrors, &configured)
        };
        let pool = set.create_pool(&mut request, &self.c.io, &pool_options)?;

        self.c.io.write_error("<info>Updating dependencies</info>");
        let mut solver = Solver::new(&policy, &pool, &self.c.io);
        let mut transaction = match solver.solve(&request, &self.solver_filter) {
            Ok(t) => t,
            Err(e) => {
                let Some(problems) = e.downcast_ref::<SolverProblems>() else { return Err(e) };
                self.c
                    .io
                    .write_error_at("<error>Your requirements could not be resolved to an installable set of packages.</error>", Verbosity::Quiet);
                self.c.io.write_error(&problems.pretty_string(&set, &request, &pool, self.c.io.is_verbose(), false, &self.ini_files())?);
                if !self.dev_mode {
                    self.c.io.write_error_at(
                        "<warning>Running update with --no-dev does not mean require-dev is ignored, it just means the packages will not be installed. If dev requirements are blocking the update you have to resolve those problems.</warning>",
                        Verbosity::Quiet,
                    );
                }
                return Ok(SolverProblems::EXIT_CODE);
            }
        };
        self.c.io.write_error_at(&format!("Analyzed {} packages to resolve dependencies", pool.len()), Verbosity::Verbose);
        self.c.io.write_error_at(&format!("Analyzed {} rules to resolve dependencies", solver.rule_set_size()), Verbosity::Verbose);
        drop(solver);

        if transaction.operations().is_empty() {
            self.c.io.write_error("Nothing to modify in lock file");
            if self.minimal_update && self.update_allow_list.is_none() && self.c.locker.is_fresh().unwrap_or(false) {
                self.c.io.warning("The --minimal-changes option should be used with package arguments or after modifying composer.json requirements, otherwise it will likely not yield any dependency changes.");
            }
        }

        let code = self.extract_dev_packages(&mut transaction, &platform_repo, &aliases, &policy, locked.as_deref())?;
        if code != 0 {
            return Ok(code);
        }

        let platform = platform_requirements(&self.c.root.package.requires);
        let platform_dev = platform_requirements(&self.c.root.package.dev_requires);

        let lock_enabled = self.c.config.get_bool("lock");
        let ops = transaction.operations().to_vec();
        if !ops.is_empty() {
            let (mut installs, mut updates, mut removals) = (vec![], vec![], vec![]);
            for op in &ops {
                match op {
                    Operation::Install(p) => installs.push(format!("{}:{}", p.pretty_name, p.full_pretty_version())),
                    Operation::Update { initial, target } => {
                        // a mirrors update that only changed metadata is not an update for the user
                        if self.update_mirrors && initial.name == target.name && initial.version == target.version {
                            continue;
                        }
                        updates.push(format!("{}:{}", target.pretty_name, target.full_pretty_version()));
                    }
                    Operation::Uninstall(p) => removals.push(p.pretty_name.clone()),
                    _ => {}
                }
            }
            if lock_enabled {
                self.c.io.write_error(&format!(
                    "<info>Lock file operations: {} install{}, {} update{}, {} removal{}</info>",
                    installs.len(),
                    plural(installs.len()),
                    updates.len(),
                    plural(updates.len()),
                    removals.len(),
                    plural(removals.len())
                ));
                for (label, list) in [("Installs", &installs), ("Updates", &updates), ("Removals", &removals)] {
                    if !list.is_empty() {
                        self.c.io.write_error_at(&format!("{label}: {}", list.join(", ")), Verbosity::Verbose);
                    }
                }
            }
        }

        let name_of = |op: &Operation| op.package().name.clone();
        let mut uninstalls: Vec<&Operation> = ops.iter().filter(|op| matches!(op, Operation::Uninstall(_))).collect();
        let mut installs_updates: Vec<&Operation> = ops.iter().filter(|op| matches!(op, Operation::Install(_) | Operation::Update { .. })).collect();
        if self.update_mirrors {
            installs_updates.retain(
                |op| !matches!(op, Operation::Update { initial, target } if initial.name == target.name && initial.version == target.version),
            );
        }
        uninstalls.sort_by_key(|op| name_of(op));
        installs_updates.sort_by_key(|op| name_of(op));
        // alias operations are not listed (Composer's debug-only branch for them is unreachable)
        for op in uninstalls.into_iter().chain(installs_updates) {
            if let Operation::Install(p) = op {
                self.suggestions.add_from_package(p);
            }
            if lock_enabled {
                self.c.io.write_error(&format!("  - {}", op.show(true)));
            }
        }

        let packages = transaction.new_lock_packages(false, self.update_mirrors);
        let dev_packages = transaction.new_lock_packages(true, self.update_mirrors);
        let lock_aliases = transaction.aliases(&aliases);
        let flags: Vec<_> = self.c.root.stability_flags.clone();
        let platform_overrides = self.c.config.raw("platform").and_then(Value::as_object).cloned().unwrap_or_default();
        let write = self.write_lock && self.execute_operations;
        let data = LockData {
            packages: &packages,
            dev_packages: Some(&dev_packages),
            platform,
            platform_dev,
            aliases: &lock_aliases,
            minimum_stability: self.c.root.minimum_stability,
            stability_flags: &flags,
            prefer_stable: self.prefer_stable || self.c.root.prefer_stable,
            prefer_lowest: self.prefer_lowest,
            platform_overrides,
        };
        let commit_times: HashMap<String, String> = packages
            .iter()
            .chain(&dev_packages)
            .filter(|p| p.is_dev() && p.installation_source.as_deref() == Some("source"))
            .filter_map(|p| Some((p.name.clone(), self.package_commit_time(p)?)))
            .collect();
        let updated = self.c.locker.set_lock_data(data, write, |p| commit_times.get(&p.name).cloned())?;
        if write {
            let files: Vec<String> = set.repositories().iter().flat_map(|r| r.loaded_files()).collect();
            self.save_prefetch_hints(&files);
        }
        if updated && write {
            self.c.io.write_error("<info>Writing lock file</info>");
        }
        self.lock_transaction = Some(transaction);

        if self.install {
            return self.do_install(local_repo, true);
        }
        Ok(0)
    }

    /// `extractDevPackages()`: solve again without require-dev to know which packages are dev only.
    fn extract_dev_packages(
        &mut self,
        transaction: &mut LockTransaction,
        platform_repo: &Arc<dyn Repository>,
        aliases: &[RootAlias],
        policy: &DefaultPolicy,
        locked: Option<&dyn Repository>,
    ) -> Result<i32> {
        if self.c.root.package.dev_requires.is_empty() {
            return Ok(0);
        }
        let mut result = ArrayRepository::new("result repo", RepoKind::Remote);
        let opts = komposer_package::loader::LoadOptions { transport_options: true, ..Default::default() };
        for p in transaction.new_lock_packages(false, false) {
            for loaded in komposer_package::loader::load(&komposer_package::dumper::dump(&p), opts)? {
                result.add(loaded);
            }
        }
        let (mut set, fixed_root) = self.create_repository_set(true, platform_repo, aliases, None)?;
        set.add_repository(Arc::new(result));
        let mut request = self.create_request(&fixed_root, platform_repo, None)?;
        self.require_packages_for_update(&mut request, locked, false)?;
        let pool = set.create_pool_with_all_packages()?;
        let mut solver = Solver::new(policy, &pool, &self.c.io);
        match solver.solve(&request, &self.solver_filter) {
            Ok(non_dev) => {
                transaction.set_non_dev_packages(&non_dev);
                Ok(0)
            }
            Err(e) => {
                let Some(problems) = e.downcast_ref::<SolverProblems>() else { return Err(e) };
                self.c.io.write_error_at(
                    "<error>Unable to find a compatible set of packages based on your non-dev requirements alone.</error>",
                    Verbosity::Quiet,
                );
                self.c.io.write_error("Your requirements can be resolved successfully when require-dev packages are present.");
                self.c.io.write_error("You may need to move packages from require-dev or some of their dependencies to require.");
                self.c.io.write_error(&problems.pretty_string(&set, &request, &pool, self.c.io.is_verbose(), true, &self.ini_files())?);
                Ok(SolverProblems::EXIT_CODE)
            }
        }
    }

    fn do_install(&mut self, local_repo: &mut InstalledRepo, already_solved: bool) -> Result<i32> {
        if self.c.config.get_bool("lock") {
            self.c.io.write_error(&format!(
                "<info>Installing dependencies from lock file{}</info>",
                if self.dev_mode { " (including require-dev)" } else { "" }
            ));
        }
        let locked: Arc<dyn Repository> = Arc::new(self.c.locker.locked_repository(self.dev_mode)?);

        if !already_solved {
            self.c.io.write_error("<info>Verifying lock file contents can be installed on current platform.</info>");
            let platform_repo = self.platform_repo(&self.c.locker.platform_overrides()?)?;
            let policy = self.create_policy(false, None)?;
            let (mut set, fixed_root) = self.create_repository_set(false, &platform_repo, &[], Some(locked.as_ref()))?;
            set.add_repository(locked.clone());
            let mut request = self.create_request(&fixed_root, &platform_repo, Some(locked.clone()))?;

            if !self.c.locker.is_fresh()? {
                self.c.io.write_error_at(
                    "<warning>Warning: The lock file is not up to date with the latest changes in composer.json. You may be getting outdated dependencies. It is recommended that you run `composer update` or `composer update <package name>`.</warning>",
                    Verbosity::Quiet,
                );
            }
            let missing = self.c.locker.missing_requirement_info(&self.c.root, self.dev_mode)?;
            if !missing.is_empty() {
                for line in &missing {
                    self.c.io.write_error(line);
                }
                if !self.c.config.get_bool("allow-missing-requirements") {
                    return Ok(ERROR_LOCK_FILE_INVALID);
                }
            }

            for p in locked.packages()? {
                request.fix_locked_package(p);
            }
            let mut root_requires: Vec<Link> = self.c.root.package.requires.clone();
            if self.dev_mode {
                root_requires.extend(self.c.root.package.dev_requires.iter().cloned());
            }
            for link in &root_requires {
                if is_platform_package(&link.target) {
                    request.require_name(&link.target, Some(PrettyConstraint::new(link.constraint.clone(), &link.pretty_constraint)))?;
                }
            }
            for link in self.c.locker.platform_requirements(self.dev_mode)? {
                if !root_requires.iter().any(|l| l.target == link.target) {
                    request.require_name(&link.target, Some(PrettyConstraint::new(link.constraint.clone(), &link.pretty_constraint)))?;
                }
            }

            let configured = self.c.repositories()?;
            let pool_options =
                PoolOptions { ignored_types: IGNORED_TYPES.map(String::from).to_vec(), ..PoolOptions::for_install(&self.policy()?, &configured) };
            let pool = set.create_pool(&mut request, &self.c.io, &pool_options)?;
            let mut solver = Solver::new(&policy, &pool, &self.c.io);
            match solver.solve(&request, &self.solver_filter) {
                Ok(t) if !t.operations().is_empty() => {
                    self.c.io.write_error_at(
                        "<error>Your lock file cannot be installed on this system without changes. Please run composer update.</error>",
                        Verbosity::Quiet,
                    );
                    return Ok(ERROR_LOCK_FILE_INVALID);
                }
                Ok(_) => {}
                Err(e) => {
                    let Some(problems) = e.downcast_ref::<SolverProblems>() else { return Err(e) };
                    self.c.io.write_error_at(
                        "<error>Your lock file does not contain a compatible set of packages. Please run composer update.</error>",
                        Verbosity::Quiet,
                    );
                    self.c.io.write_error(&problems.pretty_string(&set, &request, &pool, self.c.io.is_verbose(), false, &self.ini_files())?);
                    return Ok(SolverProblems::EXIT_CODE);
                }
            }
        }

        let ops = local_repo_transaction(locked.as_ref(), &local_repo.packages)?;
        let scripts = self.c.scripts(self.run_scripts);
        if scripts.has("pre-operations-exec") {
            let operations = ops.iter().filter_map(crate::scripts::operation_json).collect();
            scripts.dispatch_event(
                "pre-operations-exec",
                &crate::scripts::EventKind::Installer { operations, execute_operations: self.execute_operations },
            )?;
        }
        let (mut installs, mut updates, mut removals) = (vec![], vec![], vec![]);
        for op in &ops {
            match op {
                Operation::Install(p) => installs.push(format!("{}:{}", p.pretty_name, p.full_pretty_version())),
                Operation::Update { target, .. } => updates.push(format!("{}:{}", target.pretty_name, target.full_pretty_version())),
                Operation::Uninstall(p) => removals.push(p.pretty_name.clone()),
                _ => {}
            }
        }
        if installs.is_empty() && updates.is_empty() && removals.is_empty() {
            self.c.io.write_error("Nothing to install, update or remove");
        } else {
            self.c.io.write_error(&format!(
                "<info>Package operations: {} install{}, {} update{}, {} removal{}</info>",
                installs.len(),
                plural(installs.len()),
                updates.len(),
                plural(updates.len()),
                removals.len(),
                plural(removals.len())
            ));
            for (label, list) in [("Installs", &installs), ("Updates", &updates), ("Removals", &removals)] {
                if !list.is_empty() {
                    self.c.io.write_error_at(&format!("{label}: {}", list.join(", ")), Verbosity::Verbose);
                }
            }
        }

        if self.execute_operations
            && let Some(trace) = &mut self.trace
        {
            // InstallationManagerMock: apply the operations to the local repository, without downloads
            local_repo.dev_package_names = self.c.locker.dev_package_names()?;
            for op in &ops {
                trace.push(strip_tags(&op.show(false)));
                match op {
                    Operation::Install(p) => local_repo.packages.add(p.clone()),
                    Operation::Update { initial, target } => {
                        local_repo.remove(initial);
                        if !local_repo.has(target) {
                            local_repo.packages.add(target.clone());
                        }
                    }
                    Operation::MarkAliasInstalled(p) if !local_repo.has(p) => local_repo.packages.add(p.clone()),
                    Operation::MarkAliasInstalled(_) => {}
                    Operation::Uninstall(p) | Operation::MarkAliasUninstalled(p) => local_repo.remove(p),
                }
            }
            local_repo.write(self.dev_mode, |p| self.c.install_path(p).map(|p| p.to_string_lossy().into_owned()))?;
        } else if self.execute_operations {
            local_repo.dev_package_names = self.c.locker.dev_package_names()?;
            let fetch = fetcher(self.c.http.clone());
            let mut im = InstallationManager::new(&self.c.config, &self.c.io, &fetch);
            if self.prefer_source {
                im.prefer_source = true;
            }
            if self.prefer_dist {
                im.prefer_dist = true;
            }
            let scripts = self.c.scripts(self.run_scripts);
            let mut events = scripts.package_events(&ops);
            let plugins = im.execute(local_repo, &ops, self.dev_mode, self.download_only, events.as_mut().map(|f| f as _))?;
            for plugin in plugins {
                self.c.io.warning(&format!("{plugin} is a Composer plugin, komposer installed it but does not run plugins."));
            }
            // see https://github.com/composer/composer/issues/2764
            if !ops.is_empty() {
                let vendor = self.c.vendor_dir();
                if vendor.is_dir() {
                    let _ = std::fs::File::open(&vendor).and_then(|f| f.set_modified(std::time::SystemTime::now()));
                }
            }
        } else {
            for op in &ops {
                if !op.is_alias_op() || self.c.io.is_debug() {
                    self.c.io.write_error(&format!("  - {}", op.show(false)));
                }
            }
        }
        Ok(0)
    }

    /// `createRepositorySet()`; also returns the "fixed" root package (without requirements).
    fn create_repository_set(
        &self,
        for_update: bool,
        platform_repo: &Arc<dyn Repository>,
        root_aliases: &[RootAlias],
        locked: Option<&dyn Repository>,
    ) -> Result<(RepositorySet, Vec<PackageRef>)> {
        let root = &self.c.root;
        let (minimum_stability, mut flags, requires): (_, HashMap<String, _>, Vec<(String, PrettyConstraint)>) = if for_update {
            let links = root.package.requires.iter().chain(&root.package.dev_requires);
            let requires = links.map(|l| (l.target.clone(), PrettyConstraint::new(l.constraint.clone(), &l.pretty_constraint))).collect();
            (root.minimum_stability, root.stability_flags.iter().cloned().collect(), requires)
        } else {
            // the lock file's own minimum stability and flags apply when installing from it
            let requires = locked
                .map(|l| l.packages())
                .transpose()?
                .unwrap_or_default()
                .into_iter()
                .map(|p| (p.name.clone(), PrettyConstraint::new(Constraint::single(Op::Eq, &p.version), &p.pretty_version)))
                .collect();
            (self.c.locker.minimum_stability()?, self.c.locker.stability_flags()?.into_iter().collect(), requires)
        };

        let mut root_requires = IndexMap::new();
        for (name, constraint) in requires {
            // skip ignored platform requirements so existing platform packages are not filtered out
            if self.solver_filter.is_ignored(&name) {
                continue;
            }
            let filtered = self.solver_filter.filter_constraint(&name, &constraint.constraint, true).into_owned();
            root_requires.insert(name, PrettyConstraint::new(filtered, &constraint.pretty));
        }

        let mut fixed = root.package.clone();
        fixed.requires.clear();
        fixed.dev_requires.clear();
        let fixed = Arc::new(fixed);
        flags.insert(root.package.name.clone(), komposer_semver::parse_stability(&root.package.version));

        let mut fixed_root = vec![fixed.clone()];
        let mut root_repo = ArrayRepository::new("root package repo", RepoKind::Root);
        if let Some((version, pretty)) = &root.branch_alias {
            let alias = Arc::new(Package::alias(&fixed, version, pretty));
            root_repo.add(alias.clone());
            fixed_root.insert(0, alias);
        } else {
            root_repo.add(fixed);
        }

        let mut set = RepositorySet::new(minimum_stability)
            .with_stability_flags(flags)
            .with_root_aliases(root_aliases)
            .with_root_references(root.references.iter().cloned().collect())
            .with_root_requires(root_requires)
            .with_temporary_constraints(self.temporary_constraints.clone());
        set.add_repository(Arc::new(root_repo));
        set.add_repository(platform_repo.clone());
        Ok((set, fixed_root))
    }

    /// `createRequest()`. `fixed_root` is the root package (or its alias first, then the root).
    fn create_request(&self, fixed_root: &[PackageRef], platform_repo: &Arc<dyn Repository>, locked: Option<Arc<dyn Repository>>) -> Result<Request> {
        let mut request = match locked {
            Some(l) => Request::with_locked_repository(l)?,
            None => Request::new(),
        };
        for p in fixed_root {
            request.fix_package(p.clone());
        }
        // fix the version of all platform packages to prevent the solver trying to remove or update those
        let provided = &self.c.root.package.provides;
        for p in platform_repo.packages()? {
            let provided_by_root = provided.iter().any(|l| l.target == p.name && l.constraint.matches_version(&p.version));
            if !provided_by_root {
                request.fix_package(p);
            }
        }
        Ok(request)
    }

    /// `requirePackagesForUpdate()`.
    fn require_packages_for_update(&self, request: &mut Request, locked: Option<&dyn Repository>, include_dev: bool) -> Result<()> {
        if self.update_mirrors {
            let excluded: Vec<String> = if include_dev { vec![] } else { self.c.locker.dev_package_names()? };
            for p in locked.map(|l| l.packages()).transpose()?.unwrap_or_default() {
                // root aliases put both alias and aliased in the lock repo, only require the aliased version
                if !p.is_alias() && !excluded.contains(&p.name) {
                    request.require_name(&p.name, Some(PrettyConstraint::plain(Constraint::single(Op::Eq, &p.version))))?;
                }
            }
            return Ok(());
        }
        let root = &self.c.root.package;
        let dev: &[Link] = if include_dev { &root.dev_requires } else { &[] };
        for link in root.requires.iter().chain(dev) {
            request.require_name(&link.target, Some(PrettyConstraint::new(link.constraint.clone(), &link.pretty_constraint)))?;
        }
        Ok(())
    }

    /// `createPolicy()`.
    fn create_policy(&self, for_update: bool, locked: Option<&dyn Repository>) -> Result<DefaultPolicy> {
        let (mut prefer_stable, mut prefer_lowest) = (None, None);
        if !for_update {
            prefer_stable = self.c.locker.prefer_stable()?;
            prefer_lowest = self.c.locker.prefer_lowest()?;
        }
        // old lock files without prefer stable/lowest use the composer.json info
        let prefer_stable = prefer_stable.unwrap_or(self.prefer_stable || self.c.root.prefer_stable);
        let prefer_lowest = prefer_lowest.unwrap_or(self.prefer_lowest);

        let mut preferred = None;
        if for_update
            && self.minimal_update
            && let Some(locked) = locked
        {
            let mut versions = HashMap::new();
            for p in locked.packages()? {
                if p.is_alias() || self.update_allow_list.as_ref().is_some_and(|l| l.contains(&p.name)) {
                    continue;
                }
                versions.insert(p.name.clone(), p.version.clone());
            }
            preferred = Some(versions);
        }
        Ok(DefaultPolicy::new(prefer_stable, prefer_lowest, preferred))
    }

    fn policy(&self) -> Result<PolicyOptions> {
        let policy = PolicyOptions::from_config(&self.c.config)?;
        let no_blocking = self.no_blocking
            || ["COMPOSER_NO_BLOCKING", "COMPOSER_NO_SECURITY_BLOCKING"].iter().any(|v| std::env::var(v).is_ok_and(|v| !v.is_empty() && v != "0"));
        Ok(if no_blocking { policy.without_blocking() } else { policy })
    }

    fn runtime(&self) -> Option<&PhpRuntime> {
        self.php.as_deref().or_else(|| detected_runtime())
    }

    /// Where the metadata files the last update of this project loaded are remembered: per root package
    /// name, so a project made from a skeleton (laravel/laravel) starts with the hints of the previous one.
    fn prefetch_hints_file(&self) -> Option<std::path::PathBuf> {
        let name = &self.c.root.package.name;
        let key = if name == komposer_package::ROOT_NAME {
            crate::locker::md5_hex(self.c.dir.canonicalize().ok()?.to_string_lossy().as_bytes())
        } else {
            crate::locker::md5_hex(name.as_bytes())
        };
        Some(self.c.config.get_path("cache-dir").join("komposer-prefetch").join(format!("{key}.txt")))
    }

    fn prefetch_hints(&self) -> Vec<String> {
        let Some(file) = self.prefetch_hints_file() else { return vec![] };
        std::fs::read_to_string(file).map(|s| s.lines().map(String::from).collect()).unwrap_or_default()
    }

    fn save_prefetch_hints(&self, files: &[String]) {
        if files.is_empty() || self.c.config.get_bool("cache-read-only") {
            return;
        }
        if let Some(file) = self.prefetch_hints_file()
            && let Some(dir) = file.parent()
            && std::fs::create_dir_all(dir).is_ok()
        {
            let _ = std::fs::write(file, files.join("\n"));
        }
    }

    /// `createPlatformRepo()`.
    fn platform_repo(&self, overrides: &Map<String, Value>) -> Result<Arc<dyn Repository>> {
        Ok(Arc::new(PlatformRepository::new(&Value::Object(overrides.clone()), self.runtime())?))
    }

    /// `IniHelper::getAll()`, for the hint about missing extensions.
    fn ini_files(&self) -> Vec<String> {
        self.runtime().map(|r| r.ini_files.clone()).unwrap_or_default()
    }

    fn solver_platform_filter(&self) -> PlatformRequirementFilter {
        if self.php.is_some() {
            return self.platform_filter.clone();
        }
        effective_platform_filter(&self.c.config, &self.platform_filter, &self.autoload_platform_ignore.1, &self.c.io)
    }

    /// `Locker::getPackageTime()`: commit time of a dev package installed from git source.
    fn package_commit_time(&self, p: &Package) -> Option<String> {
        if p.source.as_ref()?.kind != "git" {
            return None;
        }
        let path = self.c.install_path(p)?;
        let reference = p.source_reference().or(p.dist_reference())?;
        let out = komposer_repository::git::run(&["log", "-n1", "--format=%ct", reference], Some(&path));
        let ts: i64 = out.stdout.trim().parse().ok()?;
        komposer_package::loader::format_date(&ts.to_string())
    }
}

/// The installation manager's download callback: dist files through the HTTP client, concurrently.
pub fn fetcher(http: komposer_repository::http::HttpClient) -> impl Fn(Vec<FetchJob>) -> Vec<Result<()>> + Sync {
    move |jobs: Vec<FetchJob>| {
        let requests = jobs.into_iter().map(|j| (j.url, j.dest, RequestOptions::from_context(&j.transport_options))).collect();
        http.download_many(requests).into_iter().map(|r| r.map(drop)).collect()
    }
}

/// The platform filter the solver uses. Without a PHP binary the php/ext/lib requirements can
/// not be checked, so they are ignored (except for packages configured in `config.platform`).
pub fn effective_platform_filter(
    config: &komposer_package::config::Config,
    user: &PlatformRequirementFilter,
    user_list: &[String],
    io: &komposer_package::io::Io,
) -> PlatformRequirementFilter {
    if komposer_repository::platform::php_detected() || matches!(user, PlatformRequirementFilter::IgnoreAll) {
        return user.clone();
    }
    let overrides = config.raw("platform").and_then(Value::as_object).cloned().unwrap_or_default();
    let mut ignore: Vec<String> = user_list.to_vec();
    if !overrides.contains_key("php") {
        ignore.push("php".into());
    }
    ignore.extend(["php-*", "hhvm", "ext-*", "lib-*"].map(String::from));
    static WARNED: std::sync::Once = std::sync::Once::new();
    WARNED.call_once(|| io.warning(if overrides.contains_key("php") {
        "PHP is not installed: ext-* and lib-* requirements are not checked."
    } else {
        "PHP is not installed: php, ext-* and lib-* requirements are not checked. Set config.platform.php in composer.json to resolve for a specific PHP version."
    }));
    PlatformRequirementFilter::ignore_list(&ignore)
}

/// `extractPlatformRequirements()`.
fn platform_requirements(links: &[Link]) -> Map<String, Value> {
    links.iter().filter(|l| is_platform_package(&l.target)).map(|l| (l.target.clone(), Value::String(l.pretty_constraint.clone()))).collect()
}

/// PHP `strip_tags()` for Composer's `<info>`-style tags.
fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

fn set_env(key: &str, value: &str) {
    // SAFETY: komposer is single-threaded at the points it sets env vars
    unsafe { std::env::set_var(key, value) };
}
