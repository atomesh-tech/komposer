//! `Downloader\DownloadManager`, `FileDownloader` (dist downloads through the caller's
//! [`Fetch`]) and the `cache-files-dir` `Cache`.

use crate::archive::{Extract, contains_vendor_dir, remove_install};
use crate::manager::{InstallationManager, Installer, Job, Work};
use crate::url::{self, md5_hex, sha1_hex};
use crate::{Out, fs};
use anyhow::{Error, Result, anyhow, bail};
use komposer_package::Package;
use komposer_package::io::Verbosity;
use komposer_package::operation::Operation;
use regex::Regex;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, OnceLock};
use std::time::{Duration, SystemTime};

/// One file to download, for the caller's HTTP client.
#[derive(Clone, Debug)]
pub struct FetchJob {
    /// The processed dist URL (mirror placeholders and reference already substituted).
    pub url: String,
    /// Where to save the file.
    pub dest: PathBuf,
    /// Pretty name of the package, for messages.
    pub package: String,
    /// The package's `transport-options` (repository `options`), for the HTTP client.
    pub transport_options: Value,
}

/// Runs the jobs (concurrently) and returns one result per job, in order. It should retry
/// transient failures like Composer's HTTP downloader does; mirrors are tried by the caller of
/// `Fetch`, each in a later call.
pub type Fetch<'a> = &'a (dyn Fn(Vec<FetchJob>) -> Vec<Result<()>> + Sync);

/// What the `DownloadManager` dispatches on: the downloader registered for a dist/source type.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Downloader {
    /// zip, tar, xz, gzip, phar
    Archive,
    File,
    Path,
    Git,
    /// svn, hg, fossil, perforce and rar: installs from these are removable, not installable
    Unsupported,
}

/// The dist download of a job (`FileDownloader::download()` state).
pub(crate) struct DistDownload {
    /// remaining URLs: (base, processed, cache key)
    urls: Vec<(String, String, String)>,
    fetching: bool,
}

/// `DownloadManager::getDownloaderForPackage()`; `None` for metapackages.
pub(crate) fn downloader_for(p: &Package) -> Result<Option<(Downloader, String)>> {
    if p.package_type == "metapackage" {
        return Ok(None);
    }
    let (source, kind) = match p.installation_source.as_deref() {
        Some("dist") => ("dist", p.dist.as_ref().map(|d| d.kind.as_str())),
        Some("source") => ("source", p.source.as_ref().map(|s| s.kind.as_str())),
        _ => bail!("Package {p} does not have an installation source set"),
    };
    let kind = kind.unwrap_or("").to_lowercase();
    let (downloader, downloader_source) = match kind.as_str() {
        "git" => (Downloader::Git, "source"),
        "svn" | "hg" | "fossil" | "perforce" => (Downloader::Unsupported, "source"),
        "zip" | "tar" | "xz" | "gzip" | "phar" => (Downloader::Archive, "dist"),
        "rar" => (Downloader::Unsupported, "dist"),
        "file" => (Downloader::File, "dist"),
        "path" => (Downloader::Path, "dist"),
        _ => bail!("Unknown downloader type: {kind}. Available types: git, svn, fossil, hg, perforce, zip, rar, tar, gzip, xz, phar, file, path."),
    };
    if downloader_source != source {
        bail!("Downloader \"{kind}\" is a {downloader_source} type downloader and can not be used to download {source} for package {p}");
    }
    Ok(Some((downloader, kind)))
}

fn unsupported(p: &Package) -> Error {
    let (source, kind) = match p.installation_source.as_deref() {
        Some("source") => ("source", p.source.as_ref().map_or("", |s| s.kind.as_str())),
        _ => ("dist", p.dist.as_ref().map_or("", |d| d.kind.as_str())),
    };
    anyhow!("{kind} downloads are not supported by komposer, {} can not be installed from {source}", p.pretty_name)
}

/// `PackageInterface::getDistSha1Checksum()`, if set.
fn checksum(p: &Package) -> Option<&str> {
    p.dist.as_ref().and_then(|d| d.shasum.as_deref()).filter(|s| !s.is_empty())
}

/// `FileDownloader::getDistPath($package, PATHINFO_BASENAME)`: the file name in the dist URL.
fn dist_name(p: &Package) -> String {
    let url = p.dist.as_ref().map_or(String::new(), |d| d.url.replace('\\', "/"));
    fs::basename(url::path(&url)).to_string()
}

impl InstallationManager<'_> {
    /// `resolvePackageInstallPreference()`.
    fn install_preference(&self, p: &Package) -> &'static str {
        for (pattern, preference) in &self.preferences {
            let re = format!("(?i)^{}$", regex::escape(pattern).replace(r"\*", ".*"));
            if Regex::new(&re).is_ok_and(|re| re.is_match(&p.name)) {
                return if preference == "dist" || (!p.is_dev() && preference == "auto") { "dist" } else { "source" };
            }
        }
        if p.is_dev() { "source" } else { "dist" }
    }

    /// `getAvailableSources()`.
    pub(crate) fn available_sources(&self, p: &Package, prev: Option<&Package>) -> Result<Vec<&'static str>> {
        let has = |loc: &Option<komposer_package::Location>| loc.as_ref().is_some_and(|l| !l.kind.is_empty());
        // source before dist by default
        let mut sources = vec![];
        if has(&p.source) {
            sources.push("source");
        }
        if has(&p.dist) {
            sources.push("dist");
        }
        if sources.is_empty() {
            bail!("Package {p} must have a source or dist specified");
        }
        // updates keep the previous source, unless a stable dist install becomes a dev package
        if let Some(prev) = prev
            && let Some(prev_source) = prev.installation_source.as_deref().filter(|s| sources.contains(s))
            && !(!prev.is_dev() && prev_source == "dist" && p.is_dev())
        {
            sources.sort_by_key(|s| *s != prev_source);
            return Ok(sources);
        }
        if !self.prefer_source && (self.prefer_dist || self.install_preference(p) == "dist") {
            sources.reverse();
        }
        Ok(sources)
    }

    /// `DownloadManager::download()` up to starting the transfer; dist files are only queued.
    pub(crate) fn download(&self, job: &mut Job) -> Result<()> {
        match job.installer {
            Installer::Metapackage => return Ok(()),
            Installer::Plugin if job.package.extra.get("class").is_none_or(komposer_package::loader::php_empty) => bail!(
                "Error while installing {}, composer-plugin packages should have a class defined in their extra key to be usable.",
                job.package.pretty_name
            ),
            Installer::Project => {
                let dir = &job.path;
                if Path::new(dir).exists() && !fs::is_dir_empty(dir) {
                    bail!("Project directory {dir} is not empty.");
                }
                fs::ensure_directory_exists(dir)?;
            }
            _ => {}
        }
        fs::ensure_directory_exists(&fs::dirname(&job.path))?;
        job.sources = self.available_sources(&job.package, job.initial.as_ref())?;
        self.next_source(job, false)
    }

    /// The `$download` closure of `DownloadManager::download()`.
    fn next_source(&self, job: &mut Job, retry: bool) -> Result<()> {
        let source = job.sources.remove(0);
        if retry {
            self.out.write(&format!("    <warning>Now trying to download from {source}</warning>"));
        }
        job.package.installation_source = Some(source.into());
        let Some((downloader, _)) = downloader_for(&job.package)? else { return Ok(()) };
        let started = match downloader {
            Downloader::Git => self.git_download(job),
            Downloader::Path => self.path_download(&job.package, &job.path),
            Downloader::Archive | Downloader::File => self.start_dist(job),
            Downloader::Unsupported => Err(unsupported(&job.package)),
        };
        started.or_else(|e| self.source_failed(job, source, e))
    }

    /// `$handleError`: only dist to source fallback is gated (by `source-fallback`).
    fn source_failed(&self, job: &mut Job, source: &str, e: Error) -> Result<()> {
        let next = job.sources.first().copied();
        let blocked = next == Some("source") && !self.source_fallback;
        let warn = || self.out.write(&format!("    <warning>Failed to download {} from {source}: {e}</warning>", job.package.pretty_name));
        if next.is_none() || blocked {
            if blocked {
                warn();
                self.out.write("    <warning>Source fallback is disabled. Not trying alternative sources.</warning>");
            }
            return Err(e);
        }
        warn();
        self.next_source(job, true)
    }

    /// `FileDownloader::processUrl()`.
    pub(crate) fn process_url(&self, p: &Package, url: &str) -> String {
        match p.dist_reference() {
            Some(reference) => url::update_dist_reference(self.config, url, reference),
            None => url.to_string(),
        }
    }

    /// `FileDownloader::getFileName()`: the temporary download in `vendor/composer`.
    fn file_name(&self, p: &Package, path: &str) -> String {
        let name = dist_name(p);
        let ext = match name.rsplit_once('.') {
            Some((_, ext)) if !ext.is_empty() => ext,
            _ => p.dist.as_ref().map_or("", |d| d.kind.as_str()),
        };
        let name = format!("{}/composer/tmp-{}.{ext}", self.vendor_dir, md5_hex(&format!("{p}{path}")));
        name.trim_end_matches('.').to_string()
    }

    /// `FileDownloader::download()`: the cache key is the sha1 of the full processed URL, so
    /// caches are shared with Composer.
    fn start_dist(&self, job: &mut Job) -> Result<()> {
        let p = &job.package;
        if p.dist.as_ref().is_none_or(|d| d.url.is_empty()) {
            bail!("The given package is missing url information");
        }
        let kind = p.dist.as_ref().map_or("", |d| d.kind.as_str());
        let urls = url::package_urls(p, true)
            .into_iter()
            .map(|base| {
                let processed = self.process_url(p, &base);
                let key = format!("{}/{}.{kind}", p.name, sha1_hex(processed.as_bytes()));
                (base, processed, key)
            })
            .collect();
        let file = self.file_name(p, &job.path);
        fs::ensure_directory_exists(&job.path)?;
        fs::ensure_directory_exists(&fs::dirname(&file))?;
        job.file = Some(file);
        job.dist = Some(DistDownload { urls, fetching: false });
        self.try_dist_url(job)
    }

    /// Uses the cache for the current URL if it is valid, else queues the transfer.
    fn try_dist_url(&self, job: &mut Job) -> Result<()> {
        let (Some(dist), Some(file)) = (&mut job.dist, &job.file) else { return Ok(()) };
        let key = &dist.urls[0].2;
        let p = &job.package;
        if let Some(cache) = &self.cache
            && checksum(p).is_none_or(|c| cache.sha1(&self.out, key).as_deref() == Some(c))
            && cache.copy_to(&self.out, key, file)
        {
            let msg = format!("  - Loading <info>{}</info> (<comment>{}</comment>) from cache", p.name, p.full_pretty_version());
            self.out.at(&msg, Verbosity::VeryVerbose);
            // a corrupt cached archive gets deleted when extracting it fails
            if !cache.read_only {
                job.cache_write = Some(key.clone());
            }
            return self.verify_dist(job);
        }
        self.out.write(&format!("  - Downloading <info>{}</info> (<comment>{}</comment>)", p.name, p.full_pretty_version()));
        dist.fetching = true;
        Ok(())
    }

    fn verify_dist(&self, job: &Job) -> Result<()> {
        let (Some(dist), Some(file)) = (&job.dist, &job.file) else { return Ok(()) };
        let base = url::sanitize(&dist.urls[0].0);
        if !Path::new(file).exists() {
            bail!("{base} could not be saved to {file}, make sure the directory is writable and you have internet connectivity");
        }
        if let Some(sum) = checksum(&job.package)
            && sha1_hex(&std::fs::read(file)?) != sum
        {
            bail!("The checksum verification of the file failed (downloaded from {base})");
        }
        Ok(())
    }

    /// Runs the queued dist transfers through [`Fetch`], one call per round of
    /// retries (next mirror URL, or a fallback between source and dist).
    pub(crate) fn fetch_queued(&self, jobs: &mut [Option<Job>]) {
        loop {
            let queued: Vec<usize> =
                (0..jobs.len()).filter(|&i| jobs[i].as_ref().is_some_and(|j| j.dist.as_ref().is_some_and(|d| d.fetching))).collect();
            if queued.is_empty() {
                return;
            }
            let requests = queued
                .iter()
                .filter_map(|&i| jobs[i].as_ref())
                .map(|job| FetchJob {
                    url: job.dist.as_ref().map_or(String::new(), |d| d.urls[0].1.clone()),
                    dest: PathBuf::from(job.file.clone().unwrap_or_default()),
                    package: job.package.pretty_name.clone(),
                    transport_options: job.package.transport_options.clone(),
                })
                .collect();
            let results = (self.fetch)(requests).into_iter().chain(std::iter::repeat_with(|| Err(anyhow!("The download was not run"))));
            for (i, result) in queued.into_iter().zip(results) {
                let Some(job) = jobs[i].as_mut() else { continue };
                if let Some(dist) = &mut job.dist {
                    dist.fetching = false;
                }
                let outcome = match result {
                    Ok(()) => self.verify_dist(job).map(|()| self.cache_download(job)),
                    Err(e) => self.dist_url_failed(job, e),
                };
                if let Err(e) = outcome.or_else(|e| self.source_failed(job, "dist", e)) {
                    job.error = Some(e);
                }
            }
        }
    }

    fn cache_download(&self, job: &mut Job) {
        if let (Some(cache), Some(dist), Some(file)) = (&self.cache, &job.dist, &job.file)
            && !cache.read_only
        {
            let key = dist.urls[0].2.clone();
            cache.copy_from(&self.out, &key, file);
            job.cache_write = Some(key);
        }
    }

    /// The `$reject` closure: clean up, then try the next URL.
    fn dist_url_failed(&self, job: &mut Job, e: Error) -> Result<()> {
        if let Some(file) = &job.file
            && Path::new(file).exists()
        {
            fs::unlink(file)?;
        }
        self.clear_cache_write(job);
        let Some(dist) = &mut job.dist else { return Err(e) };
        dist.urls.remove(0);
        if dist.urls.is_empty() {
            return Err(e);
        }
        let name = &job.package.name;
        if self.out.is_debug() {
            self.out.write(&format!("    Failed downloading {name}: {e:#}"));
            self.out.write(&format!("    Trying the next URL for {name}"));
        } else {
            self.out.write(&format!("    Failed downloading {name}, trying the next URL ({e})"));
        }
        self.try_dist_url(job)
    }

    /// `clearLastCacheWrite()`.
    pub(crate) fn clear_cache_write(&self, job: &mut Job) {
        if let (Some(cache), Some(key)) = (&self.cache, job.cache_write.take()) {
            cache.remove(&self.out, &key);
        }
    }

    /// `DownloadManager::prepare()`.
    pub(crate) fn dm_prepare(&self, job: &mut Job) -> Result<()> {
        if let Some((Downloader::Git, _)) = downloader_for(&job.package)? {
            return self.git_prepare(job);
        }
        Ok(())
    }

    /// `DownloadManager::install()`.
    pub(crate) fn dm_install(&self, job: &mut Job, path: &str, output: bool) -> Result<()> {
        let path = normalize_target_dir(path);
        let p = &job.package;
        let Some((downloader, kind)) = downloader_for(p)? else { return Ok(()) };
        let line = format!("  - {}", Operation::Install(p.clone().into()).show(false));
        match downloader {
            Downloader::Archive => {
                if output {
                    self.out.write(&format!("{line}: Extracting archive"));
                }
                job.work = Some(Work::Extract(self.extraction(job, &kind, &path, None)?));
            }
            Downloader::File => {
                if output {
                    self.out.write(&line);
                }
                self.install_file(job, &path)?;
            }
            Downloader::Path => self.path_install(p, &path, output)?,
            Downloader::Git => self.git_install(job, &path, &line)?,
            Downloader::Unsupported => return Err(unsupported(p)),
        }
        Ok(())
    }

    /// The synchronous half of `ArchiveDownloader::install()`; the rest runs as [`Work`].
    fn extraction(&self, job: &mut Job, kind: &str, path: &str, remove_first: Option<String>) -> Result<Extract> {
        let temp = loop {
            let temp = format!("{}/composer/{}", self.vendor_dir, &md5_hex(&format!("{:?}{path}", SystemTime::now()))[..8]);
            if !Path::new(&temp).is_dir() {
                break temp;
            }
        };
        job.cleanup_paths.push(temp.clone());
        // installing into "." (create-project) must not delete the cwd
        if !Path::new(path).is_dir() || fs::realpath(path) != fs::cwd().ok() {
            job.cleanup_paths.push(path.to_string());
        }
        Ok(Extract {
            kind: kind.to_string(),
            file: job.file.clone().unwrap_or_default(),
            temp,
            path: path.to_string(),
            vendor_dir: self.vendor_dir.clone(),
            dist_name: dist_name(&job.package),
            package: job.package.to_string(),
            name: job.package.name.clone(),
            remove_first,
        })
    }

    /// `FileDownloader::install()` for plain files.
    fn install_file(&self, job: &Job, path: &str) -> Result<()> {
        if !contains_vendor_dir(path, &self.vendor_dir) {
            fs::empty_directory(path, true)?;
        }
        fs::ensure_directory_exists(path)?;
        fs::rename(job.file.as_deref().unwrap_or(""), &format!("{path}/{}", dist_name(&job.package)))?;
        // single files can not carry a mode, so make binaries executable
        for bin in &job.package.bin {
            let bin_path = format!("{path}/{bin}");
            if Path::new(&bin_path).exists() && !is_executable(&bin_path) && crate::binary::is_bin_path_inside_package(path, &bin_path) {
                fs::chmod_executable(&bin_path);
            }
        }
        Ok(())
    }

    /// `DownloadManager::update()`.
    pub(crate) fn dm_update(&self, job: &mut Job, path: &str) -> Result<()> {
        let path = normalize_target_dir(path);
        let initial = job.initial.clone().ok_or_else(|| anyhow!("update without initial package"))?;
        let (target_dl, initial_dl) = (downloader_for(&job.package)?, downloader_for(&initial)?);
        match (initial_dl, target_dl) {
            (None, None) => Ok(()),
            // the package became a metapackage
            (Some(_), None) => self.dm_remove(job, &initial, &path),
            // a metapackage became a concrete package
            (None, Some(_)) => self.dm_install(job, &path, true),
            (Some((_, initial_kind)), Some((downloader, kind))) if initial_kind == kind => {
                let line = format!("  - {}", Operation::Update { initial: initial.clone().into(), target: job.package.clone().into() }.show(false));
                match downloader {
                    Downloader::Archive => {
                        self.out.write(&format!("{line}: Extracting archive"));
                        job.work = Some(Work::Extract(self.extraction(job, &kind, &path, Some(path.clone()))?));
                        Ok(())
                    }
                    Downloader::File => {
                        self.out.write(&line);
                        remove_install(&path)?;
                        self.install_file(job, &path)
                    }
                    Downloader::Path => {
                        self.out.write(&format!("{line}{}", self.path_appendix(&job.package, &path)?));
                        if self.path_remove(&initial, &path, false)? {
                            remove_install(&path)?;
                        }
                        self.path_install(&job.package, &path, false)
                    }
                    Downloader::Git => self.git_update(job, &initial, &path, &line),
                    Downloader::Unsupported => Err(unsupported(&job.package)),
                }
            }
            // a type change wipes the old install, after the uninstall guard of its downloader
            (Some((initial_dl, _)), Some(_)) => {
                if initial_dl == Downloader::Git {
                    self.git_clean_changes(job, &path, false)?;
                }
                self.dm_remove(job, &initial, &path)?;
                if let Some(Work::Remove(dir)) = job.work.take() {
                    remove_install(&dir)?;
                }
                self.dm_install(job, &path, true)
            }
        }
    }

    /// `DownloadManager::remove()`: prints the operation and queues the removal.
    pub(crate) fn dm_remove(&self, job: &mut Job, p: &Package, path: &str) -> Result<()> {
        let path = normalize_target_dir(path);
        let Some((downloader, _)) = downloader_for(p)? else { return Ok(()) };
        let remove = match downloader {
            Downloader::Path => self.path_remove(p, &path, true)?,
            _ => {
                self.out.write(&format!("  - {}", Operation::Uninstall(p.clone().into()).show(false)));
                true
            }
        };
        if remove {
            job.work = Some(Work::Remove(path));
        }
        Ok(())
    }

    /// `DownloadManager::cleanup()`.
    pub(crate) fn dm_cleanup(&self, job: &mut Job) -> Result<()> {
        if job.installer == Installer::Metapackage || job.package.installation_source.is_none() {
            return Ok(());
        }
        match downloader_for(&job.package)? {
            None | Some((Downloader::Unsupported, _)) => return Ok(()),
            Some((Downloader::Git, _)) => return self.git_cleanup(job),
            Some(_) => {}
        }
        if let Some(file) = &job.file
            && Path::new(file).exists()
        {
            fs::unlink(file)?;
        }
        for path in std::mem::take(&mut job.cleanup_paths) {
            fs::remove(&path)?;
        }
        let vendor = &self.vendor_dir;
        let vendor_prefix = job.package.pretty_name.split('/').next().unwrap_or("");
        let cwd = fs::cwd().ok();
        for dir in [job.path.clone(), format!("{vendor}/{vendor_prefix}"), format!("{vendor}/composer/"), vendor.clone()] {
            if Path::new(&dir).is_dir() && fs::is_dir_empty(&dir) && fs::realpath(&dir) != cwd {
                let _ = std::fs::remove_dir(&dir);
            }
        }
        Ok(())
    }
}

/// `DownloadManager::normalizeTargetDir()`.
pub(crate) fn normalize_target_dir(dir: &str) -> String {
    if dir == "/" || dir == "\\" { dir.to_string() } else { dir.trim_end_matches(['/', '\\']).to_string() }
}

fn is_executable(path: &str) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        true
    }
}

/// `Composer\Cache` for `cache-files-dir`, keyed like Composer (`a-z0-9_./`).
pub(crate) struct Cache {
    root: String,
    pub read_only: bool,
    enabled: OnceLock<bool>,
}

impl Cache {
    pub fn new(dir: &str, read_only: bool) -> Cache {
        let enabled = OnceLock::new();
        if !Cache::is_usable(dir) {
            let _ = enabled.set(false);
        }
        Cache { root: format!("{}/", dir.trim_end_matches(['/', '\\'])), read_only, enabled }
    }

    /// `Cache::isUsable()`: null-device paths disable caching.
    pub fn is_usable(path: &str) -> bool {
        static UNUSABLE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(^|[\\/])(\$null|nul|NUL|/dev/null)([\\/]|$)").unwrap());
        !UNUSABLE.is_match(path)
    }

    fn is_enabled(&self, out: &Out) -> bool {
        *self.enabled.get_or_init(|| {
            let root = Path::new(&self.root);
            let writable = (root.is_dir() || std::fs::create_dir_all(root).is_ok())
                && std::fs::metadata(root).is_ok_and(|m| !m.permissions().readonly());
            if !self.read_only && !writable {
                out.write(&format!(
                    "<warning>Cannot create cache directory {}, or directory is not writable. Proceeding without cache. See also cache-read-only config if your filesystem is read-only.</warning>",
                    self.root
                ));
                return false;
            }
            true
        })
    }

    fn path(&self, key: &str) -> String {
        static DISALLOWED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)[^a-z0-9_./]").unwrap());
        format!("{}{}", self.root, DISALLOWED.replace_all(key, "-"))
    }

    fn existing(&self, out: &Out, key: &str) -> Option<String> {
        let path = self.path(key);
        (self.is_enabled(out) && Path::new(&path).is_file()).then_some(path)
    }

    pub fn sha1(&self, out: &Out, key: &str) -> Option<String> {
        std::fs::read(self.existing(out, key)?).ok().map(|b| sha1_hex(&b))
    }

    /// `copyTo()`: also bumps the access time, which the garbage collection sorts by.
    pub fn copy_to(&self, out: &Out, key: &str, target: &str) -> bool {
        let Some(path) = self.existing(out, key) else { return false };
        if let Ok(f) = std::fs::File::open(&path) {
            let _ = f.set_times(std::fs::FileTimes::new().set_accessed(SystemTime::now()));
        }
        out.at(&format!("Reading {path} from cache"), Verbosity::Debug);
        std::fs::copy(&path, target).is_ok()
    }

    pub fn copy_from(&self, out: &Out, key: &str, source: &str) -> bool {
        if !self.is_enabled(out) || self.read_only {
            return false;
        }
        let path = self.path(key);
        let _ = fs::ensure_directory_exists(&fs::dirname(&path));
        out.at(&format!("Writing {path} into cache from {source}"), Verbosity::Debug);
        std::fs::copy(source, &path).is_ok()
    }

    pub fn remove(&self, out: &Out, key: &str) {
        if !self.read_only
            && let Some(path) = self.existing(out, key)
        {
            let _ = std::fs::remove_file(path);
        }
    }

    /// `gcIsNecessary()` + `gc()`: on roughly one run in 51, drop files older than `ttl`
    /// seconds, then the least recently used ones beyond `max_size` bytes.
    pub fn maybe_gc(&self, out: &Out, ttl: u64, max_size: u64) {
        static COLLECTED: AtomicBool = AtomicBool::new(false);
        let lucky = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).is_ok_and(|d| d.subsec_nanos() % 51 == 0);
        if COLLECTED.swap(true, Ordering::Relaxed) || std::env::var_os("COMPOSER_TEST_SUITE").is_some() || !lucky {
            return;
        }
        out.at("Running cache garbage collection", Verbosity::VeryVerbose);
        if self.is_enabled(out) && !self.read_only {
            self.gc(ttl, max_size);
        }
    }

    fn gc(&self, ttl: u64, max_size: u64) {
        let expire = SystemTime::now() - Duration::from_secs(ttl);
        let mut files = vec![];
        collect_files(Path::new(&self.root), &mut files);
        files.retain(|(path, meta)| {
            let expired = meta.modified().is_ok_and(|m| m <= expire);
            if expired {
                let _ = std::fs::remove_file(path);
            }
            !expired
        });
        let mut total: u64 = files.iter().map(|(_, m)| m.len()).sum();
        files.sort_by_key(|(_, m)| m.accessed().ok());
        for (path, meta) in files {
            if total <= max_size {
                break;
            }
            total -= meta.len();
            let _ = std::fs::remove_file(path);
        }
    }
}

fn collect_files(dir: &Path, files: &mut Vec<(PathBuf, std::fs::Metadata)>) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        match entry.metadata() {
            Ok(m) if m.is_dir() => collect_files(&entry.path(), files),
            Ok(m) => files.push((entry.path(), m)),
            Err(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use komposer_package::Location;

    fn package(source: bool, dist: bool, dev: bool) -> Package {
        let (version, pretty) = if dev { ("dev-main", "dev-main") } else { ("1.0.0.0", "1.0.0") };
        let mut p = Package::new("foo/package", version, pretty);
        p.source = source.then(|| Location { kind: "git".into(), url: "x".into(), reference: Some("r".into()), ..Default::default() });
        p.dist = dist.then(|| Location { kind: "zip".into(), url: "y".into(), ..Default::default() });
        p
    }

    #[test]
    fn available_sources_like_download_manager_test() {
        let config = komposer_package::config::Config::new(false, None);
        let io = komposer_package::io::Io::null();
        let fetch = |_: Vec<FetchJob>| vec![];
        let mut im = InstallationManager::new(&config, &io, &fetch);
        im.prefer_dist = false;
        // previous installation source and dev flag, available (source, dist), dev, expected
        type Case<'a> = (Option<&'a str>, bool, (bool, bool), bool, &'a [&'a str]);
        let cases: &[Case] = &[
            (Some("source"), false, (true, true), false, &["source", "dist"]),
            (Some("dist"), false, (true, true), false, &["dist", "source"]),
            (Some("source"), false, (false, true), false, &["dist"]),
            (Some("dist"), false, (true, false), false, &["source"]),
            (Some("source"), false, (true, true), true, &["source", "dist"]),
            (Some("dist"), false, (true, true), true, &["source", "dist"]),
            (None, false, (true, true), true, &["source", "dist"]),
            (None, false, (false, true), true, &["dist"]),
            (None, false, (true, false), true, &["source"]),
            (None, false, (true, true), false, &["dist", "source"]),
            (None, false, (false, true), false, &["dist"]),
            (None, false, (true, false), false, &["source"]),
        ];
        for &(prev_source, prev_dev, (source, dist), dev, expected) in cases {
            let prev = prev_source.map(|s| {
                let mut p = package(true, true, prev_dev);
                p.installation_source = Some(s.into());
                p
            });
            assert_eq!(im.available_sources(&package(source, dist, dev), prev.as_ref()).unwrap(), expected, "{prev_source:?} {source} {dist} {dev}");
        }
        assert!(im.available_sources(&package(false, false, false), None).is_err());

        // preferred-install patterns
        im.preferences = vec![("foo/*".into(), "source".into())];
        assert_eq!(im.available_sources(&package(true, true, false), None).unwrap(), ["source", "dist"]);
        im.preferences = vec![("foo/*".into(), "dist".into())];
        assert_eq!(im.available_sources(&package(true, true, true), None).unwrap(), ["dist", "source"]);
        im.preferences = vec![("foo/*".into(), "auto".into())];
        assert_eq!(im.available_sources(&package(true, true, true), None).unwrap(), ["source", "dist"]);
        assert_eq!(im.available_sources(&package(true, true, false), None).unwrap(), ["dist", "source"]);
        im.preferences = vec![("bar/*".into(), "dist".into())];
        assert_eq!(im.available_sources(&package(true, true, true), None).unwrap(), ["source", "dist"]);
        im.prefer_source = true;
        im.prefer_dist = true;
        assert_eq!(im.available_sources(&package(true, true, false), None).unwrap(), ["source", "dist"], "prefer-source wins");
    }

    #[test]
    fn downloader_resolution() {
        let mut p = package(true, true, false);
        assert!(downloader_for(&p).unwrap_err().to_string().contains("does not have an installation source set"));
        p.installation_source = Some("dist".into());
        assert_eq!(downloader_for(&p).unwrap().unwrap().0, Downloader::Archive);
        p.installation_source = Some("source".into());
        assert_eq!(downloader_for(&p).unwrap().unwrap().0, Downloader::Git);
        p.source.as_mut().unwrap().kind = "hg".into();
        assert_eq!(downloader_for(&p).unwrap().unwrap().0, Downloader::Unsupported);
        assert_eq!(unsupported(&p).to_string(), "hg downloads are not supported by komposer, foo/package can not be installed from source");
        p.package_type = "metapackage".into();
        assert!(downloader_for(&p).unwrap().is_none());
    }

    #[test]
    fn file_names_and_cache_keys() {
        let config = komposer_package::config::Config::new(false, Some(Path::new("/proj")));
        let io = komposer_package::io::Io::null();
        let fetch = |_: Vec<FetchJob>| vec![];
        let im = InstallationManager::new(&config, &io, &fetch);
        let mut p = Package::new("foo/bar", "1.0.0.0", "1.0.0");
        p.dist = Some(Location { kind: "file".into(), url: "http://example.com/script.js".into(), ..Default::default() });
        let name = im.file_name(&p, "/path");
        assert!(Regex::new(r"^/proj/vendor/composer/tmp-[a-z0-9]+\.js$").unwrap().is_match(&name), "{name}");
        assert_eq!(name, im.file_name(&p, "/path"));
        p.dist.as_mut().unwrap().url = "https://api.github.com/repos/a/b/zipball/abc".into();
        p.dist.as_mut().unwrap().kind = "zip".into();
        assert!(im.file_name(&p, "/path").ends_with(".zip"));
        // processUrl only rewrites with a dist reference
        assert_eq!(im.process_url(&p, "https://github.com/composer/composer/zipball/master"), "https://github.com/composer/composer/zipball/master");
        p.dist.as_mut().unwrap().reference = Some("ref".into());
        assert_eq!(
            im.process_url(&p, "https://github.com/composer/composer/zipball/master"),
            "https://api.github.com/repos/composer/composer/zipball/ref"
        );

        let cache = Cache::new("/c/files", false);
        assert!(!Cache::is_usable("/dev/null") && !Cache::is_usable("C:\\nul\\x") && Cache::is_usable("/c/files"));
        assert_eq!(cache.path("foo/bar/abc.zip"), "/c/files/foo/bar/abc.zip");
        assert_eq!(cache.path("Foo/b@r:x.zip"), "/c/files/Foo/b-r-x.zip");
        assert!(Cache::new("/dev/null", false).existing(&im.out, "x").is_none());
    }

    #[test]
    fn gc_drops_expired_then_least_recently_used_files() {
        let dir = crate::tests::tmp_dir("cache-gc");
        let cache = Cache::new(&dir, false);
        let now = SystemTime::now();
        let entry = |name: &str, size: usize, age: u64| {
            let path = format!("{dir}/v/{name}");
            fs::ensure_directory_exists(&fs::dirname(&path)).unwrap();
            std::fs::write(&path, "x".repeat(size)).unwrap();
            let t = now - Duration::from_secs(age);
            std::fs::File::options().write(true).open(&path).unwrap().set_times(std::fs::FileTimes::new().set_modified(t).set_accessed(t)).unwrap();
            path
        };
        let (expired, old, fresh) = (entry("expired.zip", 10, 1000), entry("old.zip", 10, 50), entry("fresh.zip", 10, 10));
        cache.gc(100, 15);
        assert!(!Path::new(&expired).exists(), "older than the ttl");
        assert!(!Path::new(&old).exists(), "least recently used beyond the size limit");
        assert!(Path::new(&fresh).exists());
    }
}
