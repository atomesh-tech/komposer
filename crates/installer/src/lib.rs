//! Composer's installers and downloaders: `InstallationManager`, `LibraryInstaller`,
//! `BinaryInstaller`, `DownloadManager` with its file/archive/path/git downloaders, and
//! the `vendor/composer/installed.{json,php}` writer of `FilesystemRepository`.
//!
//! HTTP is the caller's: dist files are fetched through a [`Fetch`] callback.

mod archive;
mod binary;
mod download;
pub mod fs;
mod git;
mod installed;
mod manager;
mod path;
mod url;

pub use download::{Fetch, FetchJob};
pub use git::unpushed_changes;
pub use installed::InstalledRepo;
pub use manager::InstallationManager;

use komposer_package::io::{Io, Verbosity};
use std::sync::Mutex;

/// Console output. Test builds record the lines instead of printing them.
pub(crate) struct Out<'a> {
    io: &'a Io,
    log: Option<Mutex<Vec<String>>>,
}

impl<'a> Out<'a> {
    fn new(io: &'a Io) -> Out<'a> {
        Out { io, log: cfg!(test).then(|| Mutex::new(vec![])) }
    }

    /// `writeError()` at normal verbosity.
    pub fn write(&self, msg: &str) {
        self.at(msg, Verbosity::Normal)
    }

    pub fn at(&self, msg: &str, verbosity: Verbosity) {
        match &self.log {
            Some(log) => {
                let shown = verbosity == Verbosity::Quiet || (self.io.verbosity() != Verbosity::Quiet && verbosity <= self.io.verbosity());
                if shown {
                    log.lock().unwrap().push(self.io.format(msg));
                }
            }
            None => self.io.write_error_at(msg, verbosity),
        }
    }

    pub fn is_verbose(&self) -> bool {
        self.io.is_verbose()
    }

    pub fn is_interactive(&self) -> bool {
        self.io.is_interactive()
    }

    pub fn is_debug(&self) -> bool {
        self.io.is_debug()
    }

    #[cfg(test)]
    pub fn take(&self) -> Vec<String> {
        std::mem::take(&mut self.log.as_ref().unwrap().lock().unwrap())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use crate::FetchJob;
    use komposer_package::PackageRef;
    use komposer_package::config::Config;
    use komposer_package::loader::{LoadOptions, load_package};
    use serde_json::{Value, json};
    use std::collections::HashMap;
    use std::process::Command;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    /// Serializes tests that change or depend on the process working directory.
    pub static CWD: Mutex<()> = Mutex::new(());

    /// A scratch directory, deleted when dropped.
    pub struct TmpDir(String);

    impl std::ops::Deref for TmpDir {
        type Target = str;
        fn deref(&self) -> &str {
            &self.0
        }
    }

    impl std::fmt::Display for TmpDir {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(&self.0)
        }
    }

    impl Drop for TmpDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A fresh, canonical scratch directory under the system temp dir.
    pub fn tmp_dir(name: &str) -> TmpDir {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!("komposer-installer-{name}-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        TmpDir(dir.canonicalize().unwrap().to_string_lossy().into_owned())
    }

    /// A project config rooted at `dir`, with its own home and cache.
    pub fn config(dir: &str, extra: Value) -> Config {
        let mut config = Config::new(false, Some(std::path::Path::new(dir)));
        config.merge_config(&json!({"home": format!("{dir}/home"), "cache-dir": format!("{dir}/cache")}));
        config.merge_config(&extra);
        config
    }

    pub fn read(path: impl AsRef<std::path::Path>) -> String {
        std::fs::read_to_string(path.as_ref()).unwrap_or_else(|e| panic!("{}: {e}", path.as_ref().display()))
    }

    pub fn package(data: Value) -> PackageRef {
        Arc::new(load_package(data.as_object().unwrap(), LoadOptions::default()).unwrap())
    }

    /// Serves dist URLs from local files and records the requested URLs per `Fetch` call.
    #[derive(Default)]
    pub struct Remote {
        pub files: Mutex<HashMap<String, String>>,
        pub calls: Mutex<Vec<Vec<String>>>,
    }

    impl Remote {
        pub fn serve(&self, url: &str, file: &str) {
            self.files.lock().unwrap().insert(url.into(), file.into());
        }

        pub fn fetch(&self, jobs: Vec<FetchJob>) -> Vec<anyhow::Result<()>> {
            self.calls.lock().unwrap().push(jobs.iter().map(|j| j.url.clone()).collect());
            let files = self.files.lock().unwrap();
            jobs.iter()
                .map(|j| match files.get(&j.url) {
                    Some(src) => std::fs::copy(src, &j.dest).map(drop).map_err(Into::into),
                    None => Err(anyhow::anyhow!("The \"{}\" file could not be downloaded (HTTP/2 404 )", j.url)),
                })
                .collect()
        }
    }

    /// `remote.fetch` as a [`Fetch`] closure.
    pub fn fetcher(remote: &Remote) -> impl Fn(Vec<FetchJob>) -> Vec<anyhow::Result<()>> + Sync + '_ {
        move |jobs| remote.fetch(jobs)
    }

    /// Runs git in `dir` without the user's global config getting in the way.
    pub fn git(dir: &str, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(["-c", "user.email=t@example.org", "-c", "user.name=T", "-c", "commit.gpgsign=false", "-c", "init.defaultBranch=main"])
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// Commits `file` with `contents`, returning the commit hash.
    pub fn commit(repo: &str, file: &str, contents: &str) -> String {
        std::fs::write(format!("{repo}/{file}"), contents).unwrap();
        git(repo, &["add", "."]);
        git(repo, &["commit", "-q", "-m", contents]);
        git(repo, &["rev-parse", "HEAD"])
    }
}
