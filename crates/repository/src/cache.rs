//! `Composer\Cache`: a directory of files keyed by sanitized names, shared with PHP Composer.

use crate::util::{sha1_hex, sha256_hex, unique_suffix};
use komposer_package::io::{Io, Verbosity};
use regex::Regex;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, OnceLock};
use std::time::{Duration, SystemTime};

pub struct Cache {
    io: Arc<Io>,
    root: PathBuf,
    /// `[^<allowlist>]` with Composer's case-insensitive matching
    disallowed: Regex,
    read_only: bool,
    enabled: OnceLock<bool>,
}

impl Cache {
    /// `new Cache($io, $cacheDir, $allowlist)`; Composer's default allowlist is `a-z0-9._`.
    pub fn new(io: Arc<Io>, dir: impl Into<PathBuf>, allowlist: &str) -> Cache {
        let root = dir.into();
        let enabled = OnceLock::new();
        if !Cache::is_usable(&root.to_string_lossy()) {
            let _ = enabled.set(false);
        }
        let disallowed = Regex::new(&format!("(?i)[^{allowlist}]")).expect("valid cache allowlist");
        Cache { io, root, disallowed, read_only: false, enabled }
    }

    pub fn read_only(mut self, read_only: bool) -> Cache {
        self.read_only = read_only;
        self
    }

    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// `Cache::isUsable()`: `/dev/null`-like paths disable caching.
    pub fn is_usable(path: &str) -> bool {
        static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(^|[\\/])(\$null|nul|NUL|/dev/null)([\\/]|$)").unwrap());
        !RE.is_match(path)
    }

    pub fn is_enabled(&self) -> bool {
        *self.enabled.get_or_init(|| {
            if !self.read_only && (fs::create_dir_all(&self.root).is_err() || !is_writable(&self.root)) {
                self.io.warning(&format!(
                    "Cannot create cache directory {}/, or directory is not writable. Proceeding without cache. See also cache-read-only config if your filesystem is read-only.",
                    self.root.display()
                ));
                return false;
            }
            true
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Full path of a cache entry (after name sanitization).
    pub fn path(&self, file: &str) -> PathBuf {
        self.root.join(self.disallowed.replace_all(file, "-").as_ref())
    }

    fn existing(&self, file: &str) -> Option<PathBuf> {
        let path = self.path(file);
        (self.is_enabled() && path.is_file()).then_some(path)
    }

    pub fn read(&self, file: &str) -> Option<String> {
        let path = self.existing(file)?;
        self.io.write_error_at(&format!("Reading {} from cache", path.display()), Verbosity::Debug);
        fs::read_to_string(path).ok()
    }

    /// Atomic write through a temporary file.
    pub fn write(&self, file: &str, contents: &[u8]) -> bool {
        if !self.is_enabled() || self.read_only {
            return false;
        }
        let path = self.path(file);
        self.io.write_error_at(&format!("Writing {} into cache", path.display()), Verbosity::Debug);
        let tmp = PathBuf::from(format!("{}{}.tmp", path.display(), unique_suffix()));
        let written = (|| -> std::io::Result<()> {
            if let Some(dir) = path.parent() {
                fs::create_dir_all(dir)?;
            }
            fs::write(&tmp, contents)?;
            fs::rename(&tmp, &path)
        })();
        if let Err(e) = written {
            let _ = fs::remove_file(&tmp);
            self.io.write_error_at(&format!("<warning>Failed to write into cache: {e}</warning>"), Verbosity::Debug);
            return false;
        }
        true
    }

    /// `copyFrom()`: stores a downloaded file.
    pub fn copy_from(&self, file: &str, source: &Path) -> bool {
        if !self.is_enabled() || self.read_only {
            return false;
        }
        let path = self.path(file);
        if !source.exists() {
            self.io.error(&format!("{} does not exist, can not write into cache", source.display()));
        } else if self.io.is_debug() {
            self.io.write_error(&format!("Writing {} into cache from {}", path.display(), source.display()));
        }
        path.parent().is_some_and(|d| fs::create_dir_all(d).is_ok()) && fs::copy(source, &path).is_ok()
    }

    /// `copyTo()`: copies an entry out, refreshing its access time for gc.
    pub fn copy_to(&self, file: &str, target: &Path) -> bool {
        let Some(path) = self.existing(file) else { return false };
        if let Ok(f) = fs::File::options().append(true).open(&path) {
            let _ = f.set_times(fs::FileTimes::new().set_accessed(SystemTime::now()));
        }
        self.io.write_error_at(&format!("Reading {} from cache", path.display()), Verbosity::Debug);
        fs::copy(path, target).is_ok()
    }

    pub fn remove(&self, file: &str) -> bool {
        if self.read_only {
            return false;
        }
        self.existing(file).is_some_and(|p| fs::remove_file(p).is_ok())
    }

    /// `clear()`: empties the cache directory.
    pub fn clear(&self) -> bool {
        if !self.is_enabled() || self.read_only {
            return false;
        }
        if let Ok(entries) = fs::read_dir(&self.root) {
            for entry in entries.flatten() {
                let p = entry.path();
                let _ = if p.is_dir() && !p.is_symlink() { fs::remove_dir_all(p) } else { fs::remove_file(p) };
            }
        }
        true
    }

    /// Seconds since the entry was last modified.
    pub fn age(&self, file: &str) -> Option<u64> {
        let mtime = fs::metadata(self.existing(file)?).and_then(|m| m.modified()).ok()?;
        Some(match SystemTime::now().duration_since(mtime) {
            Ok(d) => d.as_secs(),
            Err(e) => e.duration().as_secs(),
        })
    }

    /// `gcIsNecessary()`: true roughly once every 50 runs, and at most once per process.
    pub fn gc_is_necessary() -> bool {
        static COLLECTED: AtomicBool = AtomicBool::new(false);
        if COLLECTED.swap(true, Ordering::Relaxed) || std::env::var_os("COMPOSER_TEST_SUITE").is_some() {
            return false;
        }
        u64::from_str_radix(&unique_suffix(), 16).unwrap_or(1).is_multiple_of(51)
    }

    /// `gc($ttl, $maxSize)`: drops files older than `ttl` seconds, then the least recently
    /// accessed ones until the cache fits in `max_size` bytes.
    pub fn gc(&self, ttl: u64, max_size: u64) -> bool {
        if !self.is_enabled() || self.read_only {
            return false;
        }
        let expire = SystemTime::now() - Duration::from_secs(ttl);
        let mut files = vec![];
        collect_files(&self.root, &mut files);
        files.retain(|(path, meta)| {
            let expired = meta.modified().is_ok_and(|m| m <= expire);
            if expired {
                let _ = fs::remove_file(path);
            }
            !expired
        });
        let mut total: u64 = files.iter().map(|(_, m)| m.len()).sum();
        if total > max_size {
            files.sort_by_key(|(_, m)| m.accessed().ok());
            for (path, meta) in files {
                if total <= max_size {
                    break;
                }
                total -= meta.len();
                let _ = fs::remove_file(path);
            }
        }
        true
    }

    /// `gcVcsCache($ttl)`: drops VCS mirrors not touched within `ttl` seconds.
    pub fn gc_vcs_cache(&self, ttl: u64) -> bool {
        if !self.is_enabled() {
            return false;
        }
        let expire = SystemTime::now() - Duration::from_secs(ttl);
        for entry in fs::read_dir(&self.root).into_iter().flatten().flatten() {
            if entry.metadata().is_ok_and(|m| m.is_dir() && m.modified().is_ok_and(|t| t <= expire)) {
                let _ = fs::remove_dir_all(entry.path());
            }
        }
        true
    }

    pub fn sha1(&self, file: &str) -> Option<String> {
        fs::read(self.existing(file)?).ok().map(|b| sha1_hex(&b))
    }

    pub fn sha256(&self, file: &str) -> Option<String> {
        fs::read(self.existing(file)?).ok().map(|b| sha256_hex(&b))
    }
}

fn is_writable(dir: &Path) -> bool {
    fs::metadata(dir).is_ok_and(|m| m.is_dir() && !m.permissions().readonly())
}

fn collect_files(dir: &Path, out: &mut Vec<(PathBuf, fs::Metadata)>) {
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            collect_files(&entry.path(), out);
        } else {
            out.push((entry.path(), meta));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::temp_dir;
    use std::fs::FileTimes;

    fn setup() -> (PathBuf, Cache, Vec<PathBuf>) {
        let root = temp_dir("cache");
        let files: Vec<PathBuf> = (0..4).map(|i| root.join(format!("cached.file{i}.zip"))).collect();
        for (i, f) in files.iter().enumerate() {
            fs::write(f, "0".repeat(1000)).unwrap();
            // file0 is the newest and the most recently accessed
            let t = SystemTime::now() - Duration::from_secs(if i == 0 { 10 } else { 3600 + i as u64 * 10 });
            fs::File::options().append(true).open(f).unwrap().set_times(FileTimes::new().set_modified(t).set_accessed(t)).unwrap();
        }
        let cache = Cache::new(Arc::new(Io::null()), &root, "a-z0-9.");
        (root, cache, files)
    }

    #[test]
    fn removes_outdated_files() {
        let (_root, cache, files) = setup();
        assert!(cache.gc(600, 1024 * 1024 * 1024));
        assert!(files[0].exists());
        assert!(files[1..].iter().all(|f| !f.exists()));
    }

    #[test]
    fn removes_files_when_cache_is_too_large() {
        let (_root, cache, files) = setup();
        assert!(cache.gc(86400, 1500));
        // least recently accessed go first: 3, 2, 1
        assert!(files[0].exists());
        assert!(files[1..].iter().all(|f| !f.exists()));
    }

    #[test]
    fn clears_and_sanitizes_names() {
        let (root, cache, _files) = setup();
        assert!(cache.write("provider-a/b$c.json", b"{}"));
        assert!(root.join("provider-a-b-c.json").exists());
        assert_eq!(cache.read("provider-a/b$c.json").as_deref(), Some("{}"));
        assert!(cache.age("provider-a/b$c.json").unwrap() < 5);
        assert!(cache.clear());
        assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
        let ro = Cache::new(Arc::new(Io::null()), &root, "a-z0-9.").read_only(true);
        assert!(!ro.write("x", b"1"));
        assert!(!Cache::is_usable("/dev/null"));
    }
}
