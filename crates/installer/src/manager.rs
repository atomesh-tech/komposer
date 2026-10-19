//! `Installer\InstallationManager` and the installers it dispatches to: `LibraryInstaller`,
//! `PluginInstaller` (plugins are installed but never activated), `MetapackageInstaller`
//! and `ProjectInstaller` (create-project).

use crate::archive::{Extract, remove_install};
use crate::binary::BinaryInstaller;
use crate::download::{Cache, DistDownload, Fetch, normalize_target_dir};
use crate::installed::InstalledRepo;
use crate::{Out, fs};
use anyhow::{Error, Result, bail};
use komposer_package::config::Config;
use komposer_package::io::Io;
use komposer_package::operation::Operation;
use komposer_package::{Package, PackageRef};
use regex::Regex;
use serde_json::Value;
use std::path::Path;
use std::sync::{Arc, Mutex};

/// Runs operations from the solver: downloads, installs, updates and removals, then
/// writes the installed repository.
pub struct InstallationManager<'a> {
    pub(crate) config: &'a Config,
    pub(crate) out: Out<'a>,
    pub(crate) fetch: Fetch<'a>,
    /// `DownloadManager::setPreferSource()`; initialized from `preferred-install`.
    pub prefer_source: bool,
    /// `DownloadManager::setPreferDist()`; initialized from `preferred-install`.
    pub prefer_dist: bool,
    /// `DownloadManager::setPreferences()`: `preferred-install` package patterns => `source`, `dist` or `auto`.
    pub preferences: Vec<(String, String)>,
    /// `source-fallback`: allow falling back to source when a dist download fails.
    pub source_fallback: bool,
    pub(crate) cache: Option<Cache>,
    /// The configured vendor dir (not realpath'd).
    pub(crate) vendor_dir: String,
    project_dir: Option<String>,
    binaries: BinaryInstaller,
}

/// Which installer handles a package type.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Installer {
    Library,
    Plugin,
    Metapackage,
    Project,
}

/// What [`InstallationManager::execute`] calls with each `pre-package-*` / `post-package-*` event and its operation.
pub type PackageEvents<'a> = &'a mut dyn FnMut(&str, &Operation) -> Result<()>;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum JobKind {
    Install,
    Update,
    Uninstall,
}

impl JobKind {
    /// As in the `*-package-<name>` event names.
    fn name(self) -> &'static str {
        match self {
            JobKind::Install => "install",
            JobKind::Update => "update",
            JobKind::Uninstall => "uninstall",
        }
    }
}

/// The slow part of an operation, run on worker threads.
pub(crate) enum Work {
    Remove(String),
    Extract(Extract),
}

/// The state of one install/update/uninstall operation across the execution phases.
pub(crate) struct Job {
    pub kind: JobKind,
    pub installer: Installer,
    /// The (target) package, carrying the installation source once downloaded.
    pub package: Package,
    /// For updates, the installed package.
    pub initial: Option<Package>,
    /// Updates changing the package type uninstall and reinstall.
    pub type_changed: bool,
    /// Install path of `package`.
    pub path: String,
    pub sources: Vec<&'static str>,
    pub dist: Option<DistDownload>,
    /// The downloaded dist file in `vendor/composer`.
    pub file: Option<String>,
    /// `lastCacheWrites`
    pub cache_write: Option<String>,
    /// `additionalCleanupPaths`
    pub cleanup_paths: Vec<String>,
    pub work: Option<Work>,
    pub error: Option<Error>,
    pub git_cached: bool,
    pub git_cleaned: bool,
    pub git_stashed: bool,
    pub git_discarded: bool,
}

impl<'a> InstallationManager<'a> {
    /// `Factory::createDownloadManager()` and `createDefaultInstallers()`.
    pub fn new(config: &'a Config, io: &'a Io, fetch: Fetch<'a>) -> InstallationManager<'a> {
        let dir = |key: &str| config.get_string(key).unwrap_or_default().trim_end_matches('/').to_string();
        let vendor_dir = dir("vendor-dir");
        let mut im = InstallationManager {
            config,
            out: Out::new(io),
            fetch,
            prefer_source: false,
            prefer_dist: false,
            preferences: vec![],
            source_fallback: config.get_bool("source-fallback"),
            cache: None,
            project_dir: None,
            binaries: BinaryInstaller {
                bin_dir: dir("bin-dir"),
                bin_compat: config.get_string("bin-compat").unwrap_or_else(|| "auto".into()),
                vendor_dir: vendor_dir.clone(),
            },
            vendor_dir,
        };
        match config.get("preferred-install") {
            Some(Value::String(s)) if s == "dist" => im.prefer_dist = true,
            Some(Value::String(s)) if s == "source" => im.prefer_source = true,
            Some(Value::Object(m)) => im.preferences = m.iter().filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string()))).collect(),
            _ => {}
        }
        if im.source_fallback {
            im.out.write("<warning>The source-fallback option is deprecated and will be removed in Composer 2.11 as automatic fallback between dist and source has security implications.</warning>");
            im.out.write("<warning>If you have a legitimate use case and do not want us to remove it in 2.11, please open an issue at https://github.com/composer/composer/issues to let us know.</warning>");
        }
        let ttl = config.cache_ttl("cache-files-ttl");
        if ttl > 0 {
            let cache = Cache::new(&dir("cache-files-dir"), config.get_bool("cache-read-only"));
            cache.maybe_gc(&im.out, ttl, config.cache_files_maxsize());
            im.cache = Some(cache);
        }
        im
    }

    /// Adds the `ProjectInstaller` for create-project: every package installs into `dir`.
    pub fn set_project_dir(&mut self, dir: &str) {
        self.project_dir = Some(format!("{}/", dir.replace('\\', "/").trim_end_matches('/')));
    }

    pub(crate) fn installer_for(&self, package_type: &str) -> Installer {
        match package_type.to_lowercase().as_str() {
            _ if self.project_dir.is_some() => Installer::Project,
            "metapackage" => Installer::Metapackage,
            "composer-plugin" | "composer-installer" => Installer::Plugin,
            _ => Installer::Library,
        }
    }

    /// `getInstallPath()`: `vendor-dir/pretty-name[/target-dir]` with the vendor dir created and
    /// realpath'd; `None` for metapackages.
    pub fn install_path(&self, p: &Package) -> Option<String> {
        match self.installer_for(&p.package_type) {
            Installer::Project => self.project_dir.clone(),
            Installer::Metapackage => None,
            Installer::Library | Installer::Plugin => {
                let _ = fs::ensure_directory_exists(&self.vendor_dir);
                let vendor = fs::realpath(&self.vendor_dir).unwrap_or_else(|| self.vendor_dir.clone());
                let base = format!("{vendor}/{}", p.pretty_name);
                Some(match p.target_dir.as_deref().filter(|t| !t.is_empty()) {
                    Some(target) => format!("{base}/{target}"),
                    None => base,
                })
            }
        }
    }

    /// `LibraryInstaller::getPackageBasePath()`: the install path without the target dir.
    fn base_path(&self, p: &Package) -> String {
        let path = self.install_path(p).unwrap_or_default();
        match p.target_dir.as_deref().filter(|t| !t.is_empty()) {
            Some(target) => {
                let re = Regex::new(&format!("/*{}/?$", regex::escape(target).replace('/', "/+"))).unwrap();
                re.replace(&path, "").into_owned()
            }
            None => path,
        }
    }

    /// `isPackageInstalled()`: in the repository, with its files present.
    pub fn is_package_installed(&self, repo: &InstalledRepo, p: &Package) -> bool {
        if let Some(of) = &p.alias_of {
            return repo.has(p) && self.is_package_installed(repo, of);
        }
        match self.installer_for(&p.package_type) {
            Installer::Project => false,
            Installer::Metapackage => repo.has(p),
            Installer::Library | Installer::Plugin => {
                let path = self.install_path(p).unwrap_or_default();
                repo.has(p) && (fs::is_readable(&path) || (fs::is_link(&path) && fs::realpath(&path).is_some()))
            }
        }
    }

    /// `ensureBinariesPresence()`: (re)creates missing bin proxies, without overwrite warnings.
    pub fn ensure_binaries_presence(&self, p: &Package) -> Result<()> {
        match self.installer_for(&p.package_type) {
            Installer::Library | Installer::Plugin => self.binaries.install(&self.out, p, &self.install_path(p).unwrap_or_default(), false),
            Installer::Metapackage | Installer::Project => Ok(()),
        }
    }

    /// `InstallationManager::execute()`: downloads every package first (one [`Fetch`] call
    /// per round), then runs the operations, extracting archives in parallel while printing
    /// in operation order, and finally writes the repository. On failure the downloads and
    /// partial installs are cleaned up and the operations that succeeded are still recorded.
    ///
    /// Returns the pretty names of installed or updated plugins, which komposer can not run.
    /// "Package operations: ..." is the caller's to print. `events` gets the `pre-package-*` and
    /// `post-package-*` events (`dispatchPackageEvent()`) with their operation.
    pub fn execute(
        &mut self,
        repo: &mut InstalledRepo,
        operations: &[Operation],
        dev_mode: bool,
        download_only: bool,
        mut events: Option<PackageEvents<'_>>,
    ) -> Result<Vec<String>> {
        let mut jobs: Vec<Option<Job>> = operations.iter().map(|op| self.job(repo, op)).collect();

        let mut downloaded = Ok(());
        for job in jobs.iter_mut().flatten().filter(|j| j.kind != JobKind::Uninstall) {
            if let Err(e) = self.download(job) {
                downloaded = Err(e);
                break;
            }
        }
        if downloaded.is_ok() {
            self.fetch_queued(&mut jobs);
            if let Some(e) = jobs.iter_mut().flatten().find_map(|j| j.error.take()) {
                downloaded = Err(e);
            }
        }
        if downloaded.is_err() || download_only {
            for job in jobs.iter_mut().flatten() {
                let _ = self.dm_cleanup(job);
            }
            return downloaded.map(|()| vec![]);
        }

        // `executeBatch()` per batch, plugins and installers each in their own: each operation starts after its
        // pre-package-* event, and the batch's post-package-* events follow once all of them are done
        let mut batches: Vec<std::ops::Range<usize>> = vec![];
        let mut start = 0;
        for (i, slot) in jobs.iter().enumerate() {
            let alone = slot
                .as_ref()
                .is_some_and(|j| j.kind != JobKind::Uninstall && matches!(j.package.package_type.as_str(), "composer-plugin" | "composer-installer"));
            if alone {
                if start < i {
                    batches.push(start..i);
                }
                batches.push(i..i + 1);
                start = i + 1;
            }
        }
        if start < jobs.len() {
            batches.push(start..jobs.len());
        }

        let mut first_error = None;
        let mut plugins = vec![];
        // jobs before this have had their downloads cleaned up
        let mut cleaned = 0;
        for batch in batches {
            if first_error.is_some() {
                break;
            }
            for i in batch.clone() {
                if let (Some(events), Some(job)) = (events.as_mut(), &jobs[i])
                    && let Err(e) = events(&format!("pre-package-{}", job.kind.name()), &operations[i])
                {
                    first_error = Some(e);
                    break;
                }
                match &mut jobs[i] {
                    // alias operations only show at debug verbosity
                    None if self.out.is_debug() => self.out.write(&format!("  - {}", operations[i].show(false))),
                    None => {}
                    Some(job) => {
                        if let Err(e) = self.start(repo, job) {
                            job.error = Some(e);
                        }
                    }
                }
            }
            if first_error.is_some() {
                break;
            }
            run_work(&mut jobs[batch.clone()]);

            for i in batch.clone() {
                let Some(job) = &mut jobs[i] else {
                    mark_alias(repo, &operations[i]);
                    continue;
                };
                let result = match job.error.take() {
                    Some(e) => {
                        // a corrupt archive must not stay in the cache
                        if matches!(job.work, Some(Work::Extract(_))) {
                            self.clear_cache_write(job);
                        }
                        Err(e)
                    }
                    None => self.finish(repo, job),
                };
                match result {
                    Ok(()) if job.installer == Installer::Plugin && job.kind != JobKind::Uninstall => plugins.push(job.package.pretty_name.clone()),
                    Ok(()) => {}
                    Err(e) => {
                        let kind = match job.kind {
                            JobKind::Install => "Install",
                            JobKind::Update => "Update",
                            JobKind::Uninstall => "Uninstall",
                        };
                        self.out.write(&format!("    <error>{kind} of {} failed</error>", job.package.pretty_name));
                        first_error.get_or_insert(e);
                    }
                }
                if let Err(e) = self.dm_cleanup(job) {
                    first_error.get_or_insert(e);
                }
            }
            cleaned = batch.end;

            if first_error.is_none()
                && let Some(events) = events.as_mut()
            {
                for i in batch {
                    if let Some(job) = &jobs[i]
                        && let Err(e) = events(&format!("post-package-{}", job.kind.name()), &operations[i])
                    {
                        first_error = Some(e);
                        break;
                    }
                }
            }
        }
        // the operations a failure kept from running leave no downloads behind
        for job in jobs[cleaned..].iter_mut().flatten() {
            let _ = self.dm_cleanup(job);
        }

        // Composer writes after each successful operation, so this happens on failures too
        repo.write(dev_mode, |p| self.install_path(p))?;
        match first_error {
            Some(e) => Err(e),
            None => Ok(plugins),
        }
    }

    pub(crate) fn job(&self, repo: &InstalledRepo, op: &Operation) -> Option<Job> {
        // the repository's copy knows how it was installed
        let installed = |p: &PackageRef| repo.find(p).map_or_else(|| (**p).clone(), |p| (*p).clone());
        let (kind, package, initial) = match op {
            Operation::Install(p) => (JobKind::Install, (**p).clone(), None),
            Operation::Update { initial, target } => (JobKind::Update, (**target).clone(), Some(installed(initial))),
            Operation::Uninstall(p) => (JobKind::Uninstall, installed(p), None),
            Operation::MarkAliasInstalled(_) | Operation::MarkAliasUninstalled(_) => return None,
        };
        let path = self.install_path(&package).unwrap_or_default();
        Some(Job {
            kind,
            installer: self.installer_for(&package.package_type),
            type_changed: initial.as_ref().is_some_and(|i| i.package_type != package.package_type),
            path: normalize_target_dir(&path),
            package,
            initial,
            sources: vec![],
            dist: None,
            file: None,
            cache_write: None,
            cleanup_paths: vec![],
            work: None,
            error: None,
            git_cached: false,
            git_cleaned: false,
            git_stashed: false,
            git_discarded: false,
        })
    }

    /// `prepare()` and the synchronous part of `install()`/`update()`/`uninstall()`.
    fn start(&self, repo: &mut InstalledRepo, job: &mut Job) -> Result<()> {
        if job.installer != Installer::Metapackage {
            self.dm_prepare(job)?;
        }
        let path = job.path.clone();
        let not_installed = |p: &Package| -> Result<()> {
            if !repo.has(p) {
                bail!("Package is not installed: {p}");
            }
            Ok(())
        };
        let line = |op: Operation| -> Result<()> {
            self.out.write(&format!("  - {}", op.show(false)));
            Ok(())
        };
        let initial = job.initial.clone();
        match (job.installer, job.kind) {
            (Installer::Project, JobKind::Install) => self.dm_install(job, &path, true),
            (Installer::Project, _) => bail!("not supported"),
            (_, JobKind::Update) if job.type_changed => {
                let initial = initial.expect("updates have an initial package");
                self.uninstall_now(repo, job, &initial)?;
                match job.installer {
                    Installer::Metapackage => line(Operation::Install(job.package.clone().into())),
                    _ => self.dm_install(job, &path, true),
                }
            }
            (Installer::Metapackage, JobKind::Install) => line(Operation::Install(job.package.clone().into())),
            (Installer::Metapackage, JobKind::Update) => {
                let initial = initial.expect("updates have an initial package");
                not_installed(&initial)?;
                line(Operation::Update { initial: initial.into(), target: job.package.clone().into() })
            }
            (Installer::Metapackage, JobKind::Uninstall) => {
                not_installed(&job.package)?;
                line(Operation::Uninstall(job.package.clone().into()))
            }
            (_, JobKind::Install) => {
                // remove the binaries if it appears the package files are missing
                if !fs::is_readable(&path) && repo.has(&job.package) {
                    self.binaries.remove(&job.package)?;
                }
                self.dm_install(job, &path, true)
            }
            (_, JobKind::Update) => {
                let initial = initial.expect("updates have an initial package");
                not_installed(&initial)?;
                self.binaries.remove(&initial)?;
                let initial_path = self.install_path(&initial).unwrap_or_default();
                if initial_path != path {
                    // intersecting dirs: remove and install, so the rename can not wipe the target
                    if initial_path.starts_with(&path) || path.starts_with(&initial_path) {
                        self.dm_remove(job, &initial, &self.base_path(&initial))?;
                        if let Some(Work::Remove(dir)) = job.work.take() {
                            remove_install(&dir)?;
                        }
                        return self.dm_install(job, &path, true);
                    }
                    fs::rename(&initial_path, &path)?;
                }
                self.dm_update(job, &path)
            }
            (_, JobKind::Uninstall) => {
                not_installed(&job.package)?;
                let package = job.package.clone();
                self.dm_remove(job, &package, &self.base_path(&package))
            }
        }
    }

    /// The uninstall half of an update that changes the package type, run synchronously.
    fn uninstall_now(&self, repo: &mut InstalledRepo, job: &mut Job, initial: &Package) -> Result<()> {
        if !repo.has(initial) {
            bail!("Package is not installed: {initial}");
        }
        match self.installer_for(&initial.package_type) {
            Installer::Project => bail!("not supported"),
            Installer::Metapackage => {
                self.out.write(&format!("  - {}", Operation::Uninstall(initial.clone().into()).show(false)));
                repo.remove(initial);
                Ok(())
            }
            Installer::Library | Installer::Plugin => {
                self.dm_remove(job, initial, &self.base_path(initial))?;
                if let Some(Work::Remove(dir)) = job.work.take() {
                    remove_install(&dir)?;
                }
                self.finish_uninstall(repo, initial)
            }
        }
    }

    /// What runs after the operation's work: bin proxies and repository bookkeeping.
    fn finish(&self, repo: &mut InstalledRepo, job: &mut Job) -> Result<()> {
        if matches!(job.work, Some(Work::Extract(_))) {
            // the extraction succeeded, keep its paths
            job.cleanup_paths.clear();
        }
        match (job.installer, job.kind) {
            (Installer::Project, _) => {}
            (Installer::Metapackage, JobKind::Uninstall) => repo.remove(&job.package),
            (_, JobKind::Uninstall) => self.finish_uninstall(repo, &job.package)?,
            (installer, kind) => {
                if installer != Installer::Metapackage {
                    self.binaries.install(&self.out, &job.package, &job.path, true)?;
                }
                if let (JobKind::Update, false, Some(initial)) = (kind, job.type_changed, &job.initial) {
                    repo.remove(initial);
                }
                if !repo.has(&job.package) {
                    repo.packages.add(Arc::new(job.package.clone()));
                }
            }
        }
        Ok(())
    }

    fn finish_uninstall(&self, repo: &mut InstalledRepo, p: &Package) -> Result<()> {
        self.binaries.remove(p)?;
        repo.remove(p);
        if p.name.find('/').is_some_and(|i| i > 0) {
            let vendor_dir = fs::dirname(&self.base_path(p));
            if Path::new(&vendor_dir).is_dir() && fs::is_dir_empty(&vendor_dir) {
                let _ = std::fs::remove_dir(&vendor_dir);
            }
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn output(&self) -> Vec<String> {
        self.out.take()
    }
}

/// `markAliasInstalled()` / `markAliasUninstalled()`.
fn mark_alias(repo: &mut InstalledRepo, op: &Operation) {
    match op {
        Operation::MarkAliasInstalled(alias) if !repo.has(alias) => {
            let mut alias = (**alias).clone();
            // point at the installed copy, which knows its installation source
            if let Some(base) = alias.alias_of.as_ref().and_then(|of| repo.find(of)) {
                alias.alias_of = Some(base);
            }
            repo.packages.add(Arc::new(alias));
        }
        Operation::MarkAliasUninstalled(alias) => repo.remove(alias),
        _ => {}
    }
}

/// Runs the queued extractions and removals on a few threads.
fn run_work(jobs: &mut [Option<Job>]) {
    let pending: Vec<&mut Job> = jobs.iter_mut().flatten().filter(|j| j.error.is_none() && j.work.is_some()).collect();
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(1, 8).min(pending.len());
    let queue = Mutex::new(pending.into_iter());
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                loop {
                    let Some(job) = queue.lock().unwrap().next() else { break };
                    let result = match &job.work {
                        Some(Work::Remove(path)) => remove_install(path),
                        Some(Work::Extract(extract)) => extract.run(),
                        None => Ok(()),
                    };
                    if let Err(e) = result {
                        job.error = Some(e);
                    }
                }
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::tests::zip;
    use crate::tests::{Remote, config, fetcher, package, read, tmp_dir};
    use crate::url::sha1_hex;
    use komposer_package::io::Verbosity;
    use komposer_package::loader;
    use serde_json::json;

    fn io() -> Io {
        Io::new(Verbosity::Normal, Some(false), false)
    }

    fn exists(path: impl AsRef<Path>) -> bool {
        std::fs::symlink_metadata(path).is_ok()
    }

    fn root() -> komposer_package::RootPackage {
        loader::load_root(json!({"name": "acme/app"}).as_object().unwrap(), || None).unwrap()
    }

    /// Serves `a/lib` `version` as a zip with a top-level dir, like GitHub zipballs.
    fn lib(remote: &Remote, dir: &str, version: &str, extra: Value) -> PackageRef {
        let url = format!("https://example.org/a-lib-{version}.zip");
        let file = format!("{dir}/a-lib-{version}.zip");
        zip(
            &file,
            &[
                ("a-lib-abc/", "", 0o755),
                ("a-lib-abc/bin/tool", "#!/usr/bin/env php\n<?php echo 1;", 0o755),
                ("a-lib-abc/src/A.php", &format!("<?php // {version}"), 0o644),
            ],
        );
        remote.serve(&url, &file);
        let mut data = json!({"name": "a/lib", "version": version, "dist": {"type": "zip", "url": url, "reference": "abc"}});
        if let (Value::Object(d), Value::Object(e)) = (&mut data, extra) {
            d.extend(e);
        }
        package(data)
    }

    fn meta(version: &str) -> PackageRef {
        package(json!({"name": "meta/pkg", "version": version, "type": "metapackage"}))
    }

    #[test]
    fn package_events_come_per_batch_with_plugins_alone() {
        let dir = tmp_dir("im-events");
        let (config, io, remote) = (config(&dir, json!({})), io(), Remote::default());
        let fetch = fetcher(&remote);
        let mut im = InstallationManager::new(&config, &io, &fetch);
        let mut repo = InstalledRepo::load(&format!("{dir}/vendor/composer/installed.json"), Some(root())).unwrap();
        let two = package(json!({"name": "meta/two", "version": "1.0.0", "type": "metapackage"}));
        let plugin = lib(&remote, &dir, "1.0.0", json!({"type": "composer-plugin", "extra": {"class": "A\\Plugin"}}));
        let ops = [Operation::Install(meta("1.0.0")), Operation::Install(two.clone()), Operation::Install(plugin), Operation::Install(meta("2.0.0"))];
        let mut seen = vec![];
        let mut record = |event: &str, op: &Operation| -> Result<()> {
            let Operation::Install(p) = op else { unreachable!() };
            seen.push(format!("{event} {} {}", p.name, p.pretty_version));
            Ok(())
        };
        im.execute(&mut repo, &ops, true, false, Some(&mut record)).unwrap();
        assert_eq!(
            seen,
            [
                "pre-package-install meta/pkg 1.0.0",
                "pre-package-install meta/two 1.0.0",
                "post-package-install meta/pkg 1.0.0",
                "post-package-install meta/two 1.0.0",
                "pre-package-install a/lib 1.0.0",
                "post-package-install a/lib 1.0.0",
                "pre-package-install meta/pkg 2.0.0",
                "post-package-install meta/pkg 2.0.0",
            ]
        );

        // a failing listener stops the operations that follow it
        let mut fail =
            |event: &str, _: &Operation| -> Result<()> { if event.starts_with("pre-") { anyhow::bail!("listener failed") } else { Ok(()) } };
        let mut fresh = InstalledRepo::load(&format!("{dir}/other/installed.json"), Some(root())).unwrap();
        let err = im.execute(&mut fresh, &[Operation::Install(two)], true, false, Some(&mut fail)).unwrap_err();
        assert_eq!(err.to_string(), "listener failed");
        assert!(fresh.packages.canonical().is_empty());
    }

    #[test]
    fn installs_updates_and_removes_dist_packages() {
        let dir = tmp_dir("im-lifecycle");
        let (config, io, remote) = (config(&dir, json!({})), io(), Remote::default());
        let fetch = fetcher(&remote);
        let mut im = InstallationManager::new(&config, &io, &fetch);
        let installed_json = format!("{dir}/vendor/composer/installed.json");
        let mut repo = InstalledRepo::load(&installed_json, Some(root())).unwrap();

        let v1 = lib(&remote, &dir, "1.0.0", json!({"bin": ["bin/tool"]}));
        let ops = [Operation::Install(v1.clone()), Operation::Install(meta("1.0.0"))];
        assert!(im.execute(&mut repo, &ops, true, false, None).unwrap().is_empty());
        assert_eq!(
            im.output(),
            ["  - Downloading a/lib (1.0.0)", "  - Installing a/lib (1.0.0): Extracting archive", "  - Installing meta/pkg (1.0.0)"]
        );
        assert_eq!(remote.calls.lock().unwrap().len(), 1, "one fetch round");
        assert_eq!(read(format!("{dir}/vendor/a/lib/src/A.php")), "<?php // 1.0.0");
        let proxy = read(format!("{dir}/vendor/bin/tool"));
        assert!(proxy.contains("This file includes the referenced bin path (../a/lib/bin/tool)"), "{proxy}");
        assert!(proxy.ends_with("return include __DIR__ . '/..'.'/a/lib/bin/tool';\n"));
        assert_eq!(
            read(&installed_json),
            r#"{
    "packages": [
        {
            "name": "a/lib",
            "version": "1.0.0",
            "version_normalized": "1.0.0.0",
            "dist": {
                "type": "zip",
                "url": "https://example.org/a-lib-1.0.0.zip",
                "reference": "abc"
            },
            "bin": [
                "bin/tool"
            ],
            "type": "library",
            "installation-source": "dist",
            "install-path": "../a/lib"
        },
        {
            "name": "meta/pkg",
            "version": "1.0.0",
            "version_normalized": "1.0.0.0",
            "type": "metapackage",
            "install-path": null
        }
    ],
    "dev": true,
    "dev-package-names": []
}
"#
        );
        let php = read(format!("{dir}/vendor/composer/installed.php"));
        assert!(php.contains("        'a/lib' => array(\n            'pretty_version' => '1.0.0',\n            'version' => '1.0.0.0',\n            'reference' => 'abc',\n            'type' => 'library',\n            'install_path' => __DIR__ . '/../a/lib',"), "{php}");
        let mut leftovers: Vec<_> = std::fs::read_dir(format!("{dir}/vendor/composer")).unwrap().map(|e| e.unwrap().file_name()).collect();
        leftovers.sort();
        assert_eq!(leftovers, ["InstalledVersions.php", "installed.json", "installed.php"], "temp files are cleaned up");
        let cached = format!("{dir}/cache/files/a/lib/{}.zip", sha1_hex(b"https://example.org/a-lib-1.0.0.zip"));
        assert!(Path::new(&cached).is_file(), "cache key is the sha1 of the processed url");

        // another project sharing the cache does not download again
        let other = tmp_dir("im-lifecycle-cached");
        let other_config = crate::tests::config(&other, json!({"cache-dir": format!("{dir}/cache")}));
        let mut other_im = InstallationManager::new(&other_config, &io, &fetch);
        let mut other_repo = InstalledRepo::load(&format!("{other}/vendor/composer/installed.json"), None).unwrap();
        other_im.execute(&mut other_repo, &[Operation::Install(v1.clone())], true, false, None).unwrap();
        assert_eq!(other_im.output(), ["  - Installing a/lib (1.0.0): Extracting archive"]);
        assert_eq!(remote.calls.lock().unwrap().len(), 1);
        assert!(Path::new(&format!("{other}/vendor/a/lib/src/A.php")).is_file());

        // update: the old binary goes, the new version is extracted in place
        let v2 = lib(&remote, &dir, "2.0.0", json!({}));
        im.execute(&mut repo, &[Operation::Update { initial: v1, target: v2.clone() }], true, false, None).unwrap();
        assert_eq!(im.output(), ["  - Downloading a/lib (2.0.0)", "  - Upgrading a/lib (1.0.0 => 2.0.0): Extracting archive"]);
        assert_eq!(read(format!("{dir}/vendor/a/lib/src/A.php")), "<?php // 2.0.0");
        assert!(!exists(format!("{dir}/vendor/bin")), "empty bin dir is removed");
        assert_eq!(repo.packages.all().iter().map(|p| p.unique_name()).collect::<Vec<_>>(), ["meta/pkg-1.0.0.0", "a/lib-2.0.0.0"]);

        im.execute(&mut repo, &[Operation::Uninstall(v2), Operation::Uninstall(meta("1.0.0"))], true, false, None).unwrap();
        assert_eq!(im.output(), ["  - Removing a/lib (2.0.0)", "  - Removing meta/pkg (1.0.0)"]);
        assert!(!exists(format!("{dir}/vendor/a")), "empty vendor namespace dir is removed");
        assert!(repo.packages.is_empty());
        assert!(read(&installed_json).contains("\"packages\": []"));
    }

    #[test]
    fn checksum_failures_abort_before_installing() {
        let dir = tmp_dir("im-checksum");
        let (config, io, remote) = (config(&dir, json!({})), io(), Remote::default());
        let fetch = fetcher(&remote);
        let mut im = InstallationManager::new(&config, &io, &fetch);
        let mut repo = InstalledRepo::load(&format!("{dir}/vendor/composer/installed.json"), None).unwrap();
        let p = lib(&remote, &dir, "1.0.0", json!({}));
        let mut bad = (*p).clone();
        bad.dist.as_mut().unwrap().shasum = Some("0".repeat(40));
        let err = im.execute(&mut repo, &[Operation::Install(bad.into())], true, false, None).unwrap_err();
        assert_eq!(err.to_string(), "The checksum verification of the file failed (downloaded from https://example.org/a-lib-1.0.0.zip)");
        assert_eq!(im.output(), ["  - Downloading a/lib (1.0.0)"]);
        assert!(!exists(format!("{dir}/vendor")), "nothing is left behind");
    }

    #[test]
    fn mirrors_and_source_fallback() {
        let dir = tmp_dir("im-mirrors");
        let (config, io, remote) = (config(&dir, json!({})), io(), Remote::default());
        let fetch = fetcher(&remote);
        let mut im = InstallationManager::new(&config, &io, &fetch);
        let mut repo = InstalledRepo::in_memory();
        let mirrors = json!({"dist": {"type": "zip", "url": "https://example.org/a-lib-1.0.0.zip", "reference": "abc", "mirrors": [{"url": "https://mirror.example.org/%package%/%reference%.%type%", "preferred": true}]}});
        let p = lib(&remote, &dir, "1.0.0", mirrors);
        im.execute(&mut repo, &[Operation::Install(p)], true, false, None).unwrap();
        assert_eq!(
            im.output(),
            [
                "  - Downloading a/lib (1.0.0)",
                "    Failed downloading a/lib, trying the next URL (The \"https://mirror.example.org/a/lib/abc.zip\" file could not be downloaded (HTTP/2 404 ))",
                "  - Downloading a/lib (1.0.0)",
                "  - Installing a/lib (1.0.0): Extracting archive",
            ]
        );
        assert_eq!(
            *remote.calls.lock().unwrap(),
            [vec!["https://mirror.example.org/a/lib/abc.zip".to_string()], vec!["https://example.org/a-lib-1.0.0.zip".to_string()]]
        );

        // dist failures do not fall back to source unless source-fallback is enabled
        let missing = package(json!({
            "name": "b/lib", "version": "1.0.0",
            "dist": {"type": "zip", "url": "https://example.org/missing.zip"},
            "source": {"type": "git", "url": "https://example.org/b.git", "reference": "abc"},
        }));
        let err = im.execute(&mut repo, &[Operation::Install(missing)], true, false, None).unwrap_err();
        assert_eq!(err.to_string(), "The \"https://example.org/missing.zip\" file could not be downloaded (HTTP/2 404 )");
        assert_eq!(
            im.output(),
            [
                "  - Downloading b/lib (1.0.0)",
                "    Failed to download b/lib from dist: The \"https://example.org/missing.zip\" file could not be downloaded (HTTP/2 404 )",
                "    Source fallback is disabled. Not trying alternative sources.",
            ]
        );
    }

    #[test]
    fn unsupported_sources_fall_back_to_dist() {
        let dir = tmp_dir("im-hg");
        let (config, io, remote) = (config(&dir, json!({})), io(), Remote::default());
        let fetch = fetcher(&remote);
        let mut im = InstallationManager::new(&config, &io, &fetch);
        im.prefer_source = true;
        let p = lib(&remote, &dir, "1.0.0", json!({"source": {"type": "hg", "url": "https://example.org/hg", "reference": "abc"}}));
        im.execute(&mut InstalledRepo::in_memory(), &[Operation::Install(p)], true, false, None).unwrap();
        assert_eq!(
            im.output(),
            [
                "    Failed to download a/lib from source: hg downloads are not supported by komposer, a/lib can not be installed from source",
                "    Now trying to download from dist",
                "  - Downloading a/lib (1.0.0)",
                "  - Installing a/lib (1.0.0): Extracting archive",
            ]
        );
        assert_eq!(read(format!("{dir}/vendor/a/lib/src/A.php")), "<?php // 1.0.0");
    }

    #[test]
    fn failed_extraction_is_rolled_back_and_others_recorded() {
        let dir = tmp_dir("im-corrupt");
        let (config, io, remote) = (config(&dir, json!({})), io(), Remote::default());
        let fetch = fetcher(&remote);
        let mut im = InstallationManager::new(&config, &io, &fetch);
        let installed_json = format!("{dir}/vendor/composer/installed.json");
        let mut repo = InstalledRepo::load(&installed_json, None).unwrap();
        let p = lib(&remote, &dir, "1.0.0", json!({}));
        std::fs::write(format!("{dir}/a-lib-1.0.0.zip"), "not a zip").unwrap();
        let err = im.execute(&mut repo, &[Operation::Install(p), Operation::Install(meta("1.0.0"))], true, false, None).unwrap_err();
        assert!(err.to_string().ends_with(".zip' is not a zip archive."), "{err}");
        assert_eq!(
            im.output(),
            [
                "  - Downloading a/lib (1.0.0)",
                "  - Installing a/lib (1.0.0): Extracting archive",
                "  - Installing meta/pkg (1.0.0)",
                "    Install of a/lib failed"
            ]
        );
        assert!(!exists(format!("{dir}/vendor/a")));
        let cached = format!("{dir}/cache/files/a/lib/{}.zip", sha1_hex(b"https://example.org/a-lib-1.0.0.zip"));
        assert!(!exists(cached), "the corrupt archive is dropped from the cache");
        let json = read(&installed_json);
        assert!(json.contains("meta/pkg") && !json.contains("a/lib"), "{json}");
        assert_eq!(std::fs::read_dir(format!("{dir}/vendor/composer")).unwrap().count(), 1, "only installed.json remains");
    }

    #[test]
    fn metapackages_like_metapackage_installer_test() {
        let dir = tmp_dir("im-meta");
        let (config, io) = (config(&dir, json!({})), io());
        let fetch = |_: Vec<crate::FetchJob>| -> Vec<Result<()>> { panic!("nothing to download") };
        let mut im = InstallationManager::new(&config, &io, &fetch);
        let mut repo = InstalledRepo::in_memory();
        im.execute(&mut repo, &[Operation::Install(meta("1.0.0"))], true, false, None).unwrap();
        im.execute(&mut repo, &[Operation::Update { initial: meta("1.0.0"), target: meta("2.0.0") }], true, false, None).unwrap();
        assert_eq!(repo.packages.all().iter().map(|p| p.unique_name()).collect::<Vec<_>>(), ["meta/pkg-2.0.0.0"]);
        im.execute(&mut repo, &[Operation::Uninstall(meta("2.0.0"))], true, false, None).unwrap();
        assert!(repo.packages.is_empty());
        assert_eq!(im.output(), ["  - Installing meta/pkg (1.0.0)", "  - Upgrading meta/pkg (1.0.0 => 2.0.0)", "  - Removing meta/pkg (2.0.0)"]);

        let err = im.execute(&mut repo, &[Operation::Uninstall(meta("2.0.0"))], true, false, None).unwrap_err();
        assert_eq!(err.to_string(), "Package is not installed: meta/pkg-2.0.0.0");
        assert_eq!(im.output(), ["    Uninstall of meta/pkg failed"]);
    }

    #[test]
    fn target_dirs_plugins_aliases_and_files() {
        let dir = tmp_dir("im-misc");
        let (config, io, remote) = (config(&dir, json!({})), io(), Remote::default());
        let fetch = fetcher(&remote);
        let mut im = InstallationManager::new(&config, &io, &fetch);
        let mut repo = InstalledRepo::in_memory();

        // LibraryInstallerTest: install paths and a target-dir rename on update
        let old = lib(&remote, &dir, "1.0.0", json!({"target-dir": "oldtarget"}));
        assert_eq!(im.install_path(&old).unwrap(), format!("{dir}/vendor/a/lib/oldtarget"));
        assert!(!im.is_package_installed(&repo, &old));
        im.execute(&mut repo, &[Operation::Install(old.clone())], true, false, None).unwrap();
        assert!(im.is_package_installed(&repo, &old));
        let new = lib(&remote, &dir, "2.0.0", json!({"target-dir": "newtarget"}));
        im.execute(&mut repo, &[Operation::Update { initial: old, target: new.clone() }], true, false, None).unwrap();
        assert_eq!(read(format!("{dir}/vendor/a/lib/newtarget/src/A.php")), "<?php // 2.0.0");
        assert!(!exists(format!("{dir}/vendor/a/lib/oldtarget")));
        im.execute(&mut repo, &[Operation::Uninstall(new)], true, false, None).unwrap();
        assert!(!exists(format!("{dir}/vendor/a")));
        im.output();

        // plugins install like libraries and are reported, but need a class
        let plugin = lib(&remote, &dir, "1.0.0", json!({"type": "composer-plugin", "extra": {"class": "A\\Plugin"}}));
        assert_eq!(im.execute(&mut repo, &[Operation::Install(plugin.clone())], true, false, None).unwrap(), ["a/lib"]);
        let classless =
            package(json!({"name": "c/plugin", "version": "1.0.0", "type": "composer-plugin", "dist": {"type": "zip", "url": "https://x/y.zip"}}));
        let err = im.execute(&mut repo, &[Operation::Install(classless)], true, false, None).unwrap_err();
        assert_eq!(
            err.to_string(),
            "Error while installing c/plugin, composer-plugin packages should have a class defined in their extra key to be usable."
        );

        // aliases point at the installed copy
        let alias = Arc::new(Package::alias(&plugin, "1.1.0.0", "1.1.0"));
        im.execute(&mut repo, &[Operation::MarkAliasInstalled(alias.clone())], true, false, None).unwrap();
        let installed_alias = repo.find(&alias).unwrap();
        assert_eq!(installed_alias.alias_of.as_ref().unwrap().installation_source.as_deref(), Some("dist"));
        im.execute(&mut repo, &[Operation::MarkAliasUninstalled(alias.clone())], true, false, None).unwrap();
        assert!(!repo.has(&alias));

        // plain file dists are moved into place and their binaries made executable
        let tool = format!("{dir}/tool.phar");
        std::fs::write(&tool, "#!/usr/bin/env php\n<?php").unwrap();
        remote.serve("https://example.org/tool.phar", &tool);
        let file = package(
            json!({"name": "d/tool", "version": "1.0.0", "bin": ["tool.phar"], "dist": {"type": "file", "url": "https://example.org/tool.phar"}}),
        );
        im.execute(&mut repo, &[Operation::Install(file)], true, false, None).unwrap();
        assert!(Path::new(&format!("{dir}/vendor/d/tool/tool.phar")).is_file());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(format!("{dir}/vendor/d/tool/tool.phar")).unwrap().permissions().mode();
            assert_ne!(mode & 0o111, 0);
        }
    }

    #[test]
    fn create_project_and_download_only() {
        let dir = tmp_dir("im-project");
        let (config, io, remote) = (config(&dir, json!({})), io(), Remote::default());
        let fetch = fetcher(&remote);
        let p = lib(&remote, &dir, "1.0.0", json!({"bin": ["bin/tool"]}));

        let mut im = InstallationManager::new(&config, &io, &fetch);
        assert!(im.execute(&mut InstalledRepo::in_memory(), &[Operation::Install(p.clone())], true, true, None).unwrap().is_empty());
        assert_eq!(im.output(), ["  - Downloading a/lib (1.0.0)"]);
        assert!(!exists(format!("{dir}/vendor")), "download-only installs nothing");
        assert!(exists(format!("{dir}/cache/files/a/lib")), "but fills the cache");

        let project = format!("{dir}/project");
        im.set_project_dir(&project);
        im.execute(&mut InstalledRepo::in_memory(), &[Operation::Install(p)], true, false, None).unwrap();
        assert_eq!(im.output(), ["  - Installing a/lib (1.0.0): Extracting archive"]);
        assert_eq!(read(format!("{project}/src/A.php")), "<?php // 1.0.0");
        assert!(!exists(format!("{dir}/vendor")), "no bin proxies, no leftovers");
    }
}
