//! `Downloader\GitDownloader` (with `VcsDownloader`) and the `Util\Git` helpers it uses,
//! driving the `git` CLI.

use crate::download::Cache;
use crate::fs::{self, is_local_path, realpath};
use crate::manager::{InstallationManager, Job, JobKind};
use crate::url::{self, raw_url_encode, sanitize, strip_credentials};
use anyhow::{Result, bail};
use komposer_package::Package;
use komposer_package::io::Verbosity;
use regex::Regex;
use serde_json::Value;
use std::cmp::Ordering;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{LazyLock, OnceLock};

struct Output {
    ok: bool,
    stdout: String,
    stderr: String,
}

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).unwrap()
}

static SHA1: LazyLock<Regex> = LazyLock::new(|| re(r"^[a-f0-9]{40}$"));

/// Strips `dev-` / `-dev` from a pretty version to get the branch name.
fn branch_name(pretty_version: &str) -> String {
    static RE: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)(?:^dev-|(?:\.x)?-dev$)"));
    RE.replace_all(pretty_version, "").into_owned()
}

/// `Git::getVersion()`.
fn git_version() -> Option<&'static str> {
    static VERSION: OnceLock<Option<String>> = OnceLock::new();
    VERSION
        .get_or_init(|| {
            let out = Command::new("git").arg("--version").output().ok()?;
            let text = String::from_utf8_lossy(&out.stdout);
            re(r"(?m)^git version (\d+(?:\.\d+)+)").captures(&text).map(|c| c[1].to_string())
        })
        .as_deref()
}

fn git_at_least(version: &str) -> bool {
    git_version().is_some_and(|v| komposer_semver::version_compare(v, version) != Ordering::Less)
}

fn source_reference(p: &Package) -> Result<&str> {
    match p.source_reference().filter(|r| !r.is_empty()) {
        Some(reference) => Ok(reference),
        None => bail!("Package {} is missing reference information", p.pretty_name),
    }
}

fn has_metadata(path: &str) -> bool {
    Path::new(&format!("{path}/.git")).is_dir()
}

/// `VcsDownloader::prepareUrls()`: local paths are realpath'd.
fn prepare_urls(urls: Vec<String>) -> Vec<String> {
    urls.into_iter()
        .map(|url| {
            if !is_local_path(&url) {
                return url;
            }
            let (file_protocol, path) = match url.strip_prefix("file://") {
                Some(rest) => (true, rest.to_string()),
                None => (false, url.clone()),
            };
            let path = if path.contains('%') { percent_decode(&path) } else { path };
            let real = realpath(&path).unwrap_or_default();
            if file_protocol { format!("file://{real}") } else { real }
        })
        .collect()
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match (b[i], s.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok())) {
            (b'%', Some(byte)) => {
                out.push(byte);
                i += 3;
            }
            (c, _) => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

impl InstallationManager<'_> {
    fn git(&self, args: &[&str], cwd: Option<&str>) -> Output {
        let shown: Vec<String> = args.iter().map(|a| crate::binary::shell_escape(a)).collect();
        self.out.at(&format!("Executing command ({}): {}", cwd.unwrap_or("CWD"), sanitize(&format!("git {}", shown.join(" ")))), Verbosity::Debug);
        let mut cmd = Command::new("git");
        cmd.args(args)
            .stdin(Stdio::null())
            // `Git::cleanEnv()`
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("LANGUAGE", "C")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("DYLD_LIBRARY_PATH");
        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        }
        match cmd.output() {
            Ok(o) => Output {
                ok: o.status.success(),
                stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
            },
            Err(e) => Output { ok: false, stdout: String::new(), stderr: e.to_string() },
        }
    }

    /// Runs a command that must succeed, failing like `ProcessExecutor` callers do.
    fn git_ok(&self, args: &[&str], cwd: &str) -> Result<String> {
        let o = self.git(args, Some(cwd));
        if !o.ok {
            bail!("Failed to execute git {}\n\n{}", args.join(" "), o.stderr);
        }
        Ok(o.stdout)
    }

    fn cache_path(&self, url: &str) -> String {
        static RE: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)[^a-z0-9.]"));
        format!("{}/{}/", self.config.get_path("cache-vcs-dir").display(), RE.replace_all(&sanitize(url), "-"))
    }

    /// Tries each URL until one works, like `VcsDownloader`.
    fn each_url(&self, urls: &[String], mut f: impl FnMut(&str) -> Result<()>) -> Result<()> {
        for (i, url) in urls.iter().enumerate() {
            match f(url) {
                Ok(()) => return Ok(()),
                Err(e) if i + 1 == urls.len() => return Err(e),
                Err(e) if self.out.is_debug() => self.out.write(&format!("Failed: {e}")),
                Err(_) => self.out.write("    Failed, trying the next URL"),
            }
        }
        Ok(())
    }

    /// `VcsDownloader::download()` + `doDownload()`: syncs the `cache-vcs-dir` mirror.
    pub(crate) fn git_download(&self, job: &mut Job) -> Result<()> {
        let p = &job.package;
        let reference = source_reference(p)?;
        let urls = prepare_urls(url::package_urls(p, false));
        let mut cached = false;
        self.each_url(&urls, |url| {
            // no extra local cache for local repositories
            if is_local_path(url) {
                return Ok(());
            }
            let cache_path = self.cache_path(url);
            match git_version() {
                None => bail!("git was not found in your PATH, skipping source download"),
                // --dissociate is only available since git 2.3.0-rc0
                Some(_) if git_at_least("2.3.0-rc0") && Cache::is_usable(&cache_path) => {
                    self.out.write(&format!("  - Syncing <info>{}</info> (<comment>{}</comment>) into cache", p.name, p.full_pretty_version()));
                    self.out.at(&format!("    Cloning to cache at {cache_path}"), Verbosity::Debug);
                    cached = self.fetch_ref_or_sync_mirror(url, &cache_path, reference, &p.pretty_version)? && Path::new(&cache_path).is_dir();
                }
                Some(_) => {}
            }
            Ok(())
        })?;
        job.git_cached = cached;
        Ok(())
    }

    /// `VcsDownloader::prepare()`.
    pub(crate) fn git_prepare(&self, job: &mut Job) -> Result<()> {
        let path = job.path.clone();
        match job.kind {
            JobKind::Install => fs::empty_directory(&path, true),
            JobKind::Update => {
                self.git_clean_changes(job, &path, true)?;
                job.git_cleaned = true;
                Ok(())
            }
            JobKind::Uninstall => self.git_clean_changes(job, &path, false),
        }
    }

    /// `VcsDownloader::cleanup()`: re-applies stashed changes after an update.
    pub(crate) fn git_cleanup(&self, job: &mut Job) -> Result<()> {
        if !std::mem::take(&mut job.git_cleaned) {
            return Ok(());
        }
        job.git_discarded = false;
        if std::mem::take(&mut job.git_stashed) {
            self.out.write("    <info>Re-applying stashed changes</info>");
            let o = self.git(&["stash", "pop"], Some(&job.path));
            if !o.ok {
                bail!("Failed to apply stashed changes:\n\n{}", o.stderr);
            }
        }
        Ok(())
    }

    fn short_hash<'r>(&self, reference: &'r str) -> &'r str {
        if !self.out.is_verbose() && SHA1.is_match(reference) { &reference[..10] } else { reference }
    }

    /// `VcsDownloader::install()` + `GitDownloader::doInstall()`.
    pub(crate) fn git_install(&self, job: &Job, path: &str, line: &str) -> Result<()> {
        let p = &job.package;
        let reference = source_reference(p)?;
        let mut prefix = Some(format!("{line}: "));
        let urls = prepare_urls(url::package_urls(p, false));
        self.each_url(&urls, |url| {
            let cache_path = self.cache_path(url);
            let single_use = p.transport_options.pointer("/git/single_use_clone").is_some_and(|v| v == &Value::Bool(true));
            let (msg, commands): (String, Vec<Vec<&str>>) = if job.git_cached {
                let mut clone = vec!["clone", "--no-checkout", &cache_path, path];
                if !single_use {
                    clone.extend(["--dissociate", "--reference", &cache_path]);
                }
                let commands = vec![
                    clone,
                    vec!["remote", "set-url", "origin", "--", "%sanitizedUrl%"],
                    vec!["remote", "add", "composer", "--", "%sanitizedUrl%"],
                ];
                (format!("Cloning {} from cache", self.short_hash(reference)), commands)
            } else {
                if network_disabled() {
                    bail!("The required git reference for {} is not in cache and network is disabled, aborting", p.name);
                }
                let commands = vec![
                    vec!["clone", "--no-checkout", "--", "%url%", path],
                    vec!["remote", "add", "composer", "--", "%url%"],
                    vec!["fetch", "composer"],
                    vec!["remote", "set-url", "origin", "--", "%sanitizedUrl%"],
                    vec!["remote", "set-url", "composer", "--", "%sanitizedUrl%"],
                ];
                (format!("Cloning {}", self.short_hash(reference)), commands)
            };
            self.out.write(&format!("{}{msg}", prefix.take().unwrap_or_default()));
            self.run_commands(&commands, url, Some(path), true)?;

            match p.source.as_ref().map(|s| s.url.as_str()) {
                Some(source_url) if source_url != url => self.update_origin_url(path, source_url),
                _ => self.set_push_url(path, url),
            }
            self.update_to_commit(job, p, path, reference)
        })
    }

    /// `VcsDownloader::update()` + `GitDownloader::doUpdate()`.
    pub(crate) fn git_update(&self, job: &Job, initial: &Package, path: &str, line: &str) -> Result<()> {
        let target = &job.package;
        let reference = source_reference(target)?;
        let mut prefix = Some(format!("{line}: "));
        let urls = prepare_urls(url::package_urls(target, false));
        self.each_url(&urls, |url| {
            if !has_metadata(path) {
                bail!("The .git directory is missing from {path}, see https://getcomposer.org/commit-deps for more information");
            }
            let cache_path = self.cache_path(url);
            let (msg, remote) = if job.git_cached {
                (format!("Checking out {} from cache", self.short_hash(reference)), cache_path.as_str())
            } else {
                if network_disabled() {
                    bail!("The required git reference for {} is not in cache and network is disabled, aborting", target.name);
                }
                (format!("Checking out {}", self.short_hash(reference)), "%url%")
            };
            self.out.write(&format!("{}{msg}", prefix.take().unwrap_or_default()));
            if !self.git(&["rev-parse", "--quiet", "--verify", &format!("{reference}^{{commit}}")], Some(path)).ok {
                let commands = [vec!["remote", "set-url", "composer", "--", remote], vec!["fetch", "composer"], vec!["fetch", "--tags", "composer"]];
                self.run_commands(&commands, url, Some(path), false)?;
            }
            self.run_commands(&[vec!["remote", "set-url", "composer", "--", "%sanitizedUrl%"]], url, Some(path), false)?;
            self.update_to_commit(job, target, path, reference)?;

            let remotes = self.git(&["remote", "-v"], Some(path));
            let origin = re(r"(?m)^origin\s+(\S+)").captures(&remotes.stdout).map(|c| c[1].to_string());
            let composer = re(r"(?m)^composer\s+(\S+)").captures(&remotes.stdout).map(|c| c[1].to_string());
            let target_url = target.source.as_ref().map(|s| s.url.as_str());
            if remotes.ok
                && let (Some(origin), Some(composer), Some(target_url)) = (origin, composer, target_url)
                && origin == composer
                && composer != target_url
            {
                self.update_origin_url(path, target_url);
            }
            Ok(())
        })?;

        // the commit logs, when verbose
        if self.out.is_verbose() && has_metadata(path) {
            let (from, to) = (initial.source_reference().unwrap_or(""), reference);
            let mut message = "Pulling in changes:";
            let mut logs = self.commit_logs(from, to, path)?;
            if logs.trim().is_empty() {
                message = "Rolling back changes:";
                logs = self.commit_logs(to, from, path)?;
            }
            if !logs.trim().is_empty() {
                let logs: Vec<String> = logs.split('\n').map(|l| format!("      {l}")).collect();
                self.out.write(&format!("    {message}"));
                self.out.write(&logs.join("\n").replace('<', "\\<"));
            }
        }
        Ok(())
    }

    fn commit_logs(&self, from: &str, to: &str, path: &str) -> Result<String> {
        let range = format!("{from}..{to}");
        let mut args = vec!["rev-list"];
        let no_header = git_at_least("2.33.0-rc0");
        if no_header {
            args.push("--no-commit-header");
        }
        args.extend(["--format=%h - %an: %s", &range]);
        if git_at_least("2.10.0-rc0") {
            args.push("--no-show-signature");
        }
        let out = self.git_ok(&args, path)?;
        Ok(if no_header { out } else { re(r"(?m)^commit [a-f0-9]{40}\n?").replace_all(&out, "").into_owned() })
    }

    /// `GitDownloader::updateToCommit()`.
    fn update_to_commit(&self, job: &Job, p: &Package, path: &str, reference: &str) -> Result<()> {
        let force: &[&str] = if job.git_discarded || job.git_stashed { &["-f"] } else { &[] };
        let mut branch = branch_name(&p.pretty_version);
        let mut stderr = String::new();
        let mut run = |args: &[&str]| {
            let o = self.git(args, Some(path));
            stderr = o.stderr;
            o.ok
        };
        let with_force = |args: &[&'static str], tail: &[&str]| -> Vec<String> {
            let mut cmd: Vec<String> = vec!["checkout".into()];
            cmd.extend(force.iter().map(|s| s.to_string()));
            cmd.extend(args.iter().chain(tail).map(|s| s.to_string()));
            cmd
        };

        let branches = {
            let o = self.git(&["branch", "-r"], Some(path));
            o.ok.then_some(o.stdout)
        };
        let has_remote_branch =
            |name: &str| branches.as_deref().is_some_and(|b| re(&format!(r"(?m)^\s+composer/{}$", regex::escape(name))).is_match(b));
        let is_sha = SHA1.is_match(reference);

        // non-commitish references that are branches or tags, checked out with the remote name
        if !is_sha && has_remote_branch(reference) {
            let remote = format!("composer/{reference}");
            if run(&strs(&with_force(&["-B"], &[&branch, &remote, "--"]))) && run(&["reset", "--hard", &remote, "--"]) {
                return Ok(());
            }
        }
        // check out the branch by name and reset it, so it is on the proper branch name
        if is_sha {
            // the 'v' may have been stripped when generating the pretty name
            if branches.is_some() && !has_remote_branch(&branch) && has_remote_branch(&format!("v{branch}")) {
                branch = format!("v{branch}");
            }
            let remote = format!("composer/{branch}");
            if (run(&["checkout", &branch, "--"]) || run(&strs(&with_force(&["-B"], &[&branch, &remote, "--"]))))
                && run(&["reset", "--hard", reference, "--"])
            {
                return Ok(());
            }
        }
        let checkout = with_force(&[], &[reference, "--"]);
        if run(&strs(&checkout)) && run(&["reset", "--hard", reference, "--"]) {
            return Ok(());
        }
        let mut extra = String::new();
        // "fatal: reference is not a tree: $ref"
        if stderr.contains(reference) {
            self.out.write(&format!("    <warning>{reference} is gone (history was rewritten?)</warning>"));
            extra = format!(
                "\nIt looks like the commit hash is not available in the repository, maybe {}? Run \"composer update {}\" to resolve this.",
                if p.is_dev() { "the commit was removed from the branch" } else { "the tag was recreated" },
                p.pretty_name
            );
        }
        bail!(sanitize(&format!("Failed to execute git {} && git reset --hard {reference} --\n\n{stderr}{extra}", checkout.join(" "))))
    }

    fn update_origin_url(&self, path: &str, url: &str) {
        self.git(&["remote", "set-url", "origin", "--", url], Some(path));
        self.set_push_url(path, url);
    }

    /// `setPushUrl()`: ssh push URLs for GitHub clones.
    fn set_push_url(&self, path: &str, url: &str) {
        let domains = self.domains_regex("github-domains");
        let Some(c) = re(&format!(r"^(?:https?|git)://({domains})/([^/]+)/([^/]+?)(?:\.git)?$")).captures(url) else { return };
        let push_url = if url::string_list(self.config, "github-protocols").iter().any(|p| p == "ssh") {
            format!("git@{}:{}/{}.git", &c[1], &c[2], &c[3])
        } else {
            format!("https://{}/{}/{}.git", &c[1], &c[2], &c[3])
        };
        self.git(&["remote", "set-url", "--push", "origin", "--", &push_url], Some(path));
    }

    /// `Git::getGitHubDomainsRegex()` / `getGitLabDomainsRegex()`, without the group.
    fn domains_regex(&self, key: &str) -> String {
        url::string_list(self.config, key).iter().map(|d| regex::escape(d)).collect::<Vec<_>>().join("|")
    }

    /// `GitDownloader::cleanChanges()`, non-interactive: `discard-changes` decides.
    pub(crate) fn git_clean_changes(&self, job: &mut Job, path: &str, update: bool) -> Result<()> {
        let discard = self.config.get("discard-changes");
        if let Some(unpushed) = self.unpushed_changes(path)?
            && (self.out.is_interactive() || discard != Some(Value::Bool(true)))
        {
            bail!("Source directory {path} has unpushed changes on the current branch: \n{unpushed}");
        }
        if self.local_changes(path)?.is_none() {
            return Ok(());
        }
        match discard {
            Some(Value::Bool(true)) => {
                for args in [&["clean", "-df"][..], &["reset", "--hard"]] {
                    let o = self.git(args, Some(path));
                    if !o.ok {
                        bail!("Could not reset changes\n\n:{}", o.stdout);
                    }
                }
                job.git_discarded = true;
                Ok(())
            }
            Some(Value::String(s)) if s == "stash" && update => {
                let o = self.git(&["stash", "--include-untracked"], Some(path));
                if !o.ok {
                    bail!("Could not stash changes\n\n:{}", o.stdout);
                }
                job.git_stashed = true;
                Ok(())
            }
            _ => bail!("Source directory {path} has uncommitted changes."),
        }
    }

    /// `getLocalChanges()`.
    fn local_changes(&self, path: &str) -> Result<Option<String>> {
        if !has_metadata(path) {
            return Ok(None);
        }
        let out = self.git_ok(&["status", "--porcelain", "--untracked-files=no"], path)?;
        Ok(Some(out.trim().to_string()).filter(|s| !s.is_empty()))
    }

    /// `getUnpushedChanges()`.
    fn unpushed_changes(&self, path: &str) -> Result<Option<String>> {
        if !has_metadata(path) {
            return Ok(None);
        }
        unpushed_changes(&|args| self.git_ok(args, path))
    }

    /// `Git::syncMirror()`: updates or creates a bare mirror clone.
    fn sync_mirror(&self, url: &str, dir: &str) -> Result<bool> {
        if std::env::var("COMPOSER_DISABLE_NETWORK").is_ok_and(|v| !v.is_empty() && v != "0" && v != "prime") {
            self.out.write(&format!("<warning>Aborting git mirror sync of {} as network is disabled</warning>", sanitize(url)));
            return Ok(false);
        }
        let git_dir = self.git(&["rev-parse", "--git-dir"], Some(dir));
        if Path::new(dir).is_dir() && git_dir.ok && git_dir.stdout.trim() == "." {
            let commands = [vec!["remote", "set-url", "origin", "--", "%url%"], vec!["remote", "update", "--prune", "origin"], vec!["gc", "--auto"]];
            let synced = self.run_commands(&commands, url, Some(dir), false);
            let reset = self.run_commands(&[vec!["remote", "set-url", "origin", "--", "%sanitizedUrl%"]], url, Some(dir), false);
            if let Err(e) = synced.and(reset) {
                self.out.at(&format!("<error>Sync mirror failed: {e}</error>"), Verbosity::Debug);
                return Ok(false);
            }
            return Ok(true);
        }
        check_ownership(&git_dir.stderr, dir)?;
        fs::remove_directory(dir)?;
        self.run_commands(&[vec!["clone", "--mirror", "--", "%url%", dir]], url, Some(dir), true)?;
        self.run_commands(&[vec!["remote", "set-url", "origin", "--", "%sanitizedUrl%"]], url, Some(dir), false)?;
        Ok(true)
    }

    /// `Git::fetchRefOrSyncMirror()`.
    pub(crate) fn fetch_ref_or_sync_mirror(&self, url: &str, dir: &str, reference: &str, pretty_version: &str) -> Result<bool> {
        if self.ref_in_mirror(dir, reference)? {
            if SHA1.is_match(reference) {
                let branch = regex::escape(&branch_name(pretty_version));
                let branches = self.git(&["branch"], Some(dir));
                let tags = self.git(&["tag"], Some(dir));
                // a tag created after the ref was cached needs a sync, or the install would fail
                if branches.ok
                    && !re(&format!(r"(?m)^[\s*]*v?{branch}$")).is_match(&branches.stdout)
                    && tags.ok
                    && !re(&format!(r"(?m)^[\s*]*{branch}$")).is_match(&tags.stdout)
                {
                    self.sync_mirror(url, dir)?;
                }
            }
            return Ok(true);
        }
        if self.sync_mirror(url, dir)? {
            return self.ref_in_mirror(dir, reference);
        }
        Ok(false)
    }

    fn ref_in_mirror(&self, dir: &str, reference: &str) -> Result<bool> {
        if !Path::new(dir).is_dir() {
            return Ok(false);
        }
        let git_dir = self.git(&["rev-parse", "--git-dir"], Some(dir));
        if git_dir.ok && git_dir.stdout.trim() == "." {
            return Ok(self.git(&["rev-parse", "--quiet", "--verify", &format!("{reference}^{{commit}}")], Some(dir)).ok);
        }
        check_ownership(&git_dir.stderr, dir)?;
        Ok(false)
    }

    /// `Git::runCommands()`: `%url%` placeholders, GitHub protocol fallbacks and configured
    /// credentials. Never prompts.
    fn run_commands(&self, commands: &[Vec<&str>], url: &str, cwd: Option<&str>, initial_clone: bool) -> Result<()> {
        static SSH_PORT: LazyLock<Regex> = LazyLock::new(|| re(r"^ssh://[^@]+@[^:]+:[^0-9]+"));
        if SSH_PORT.is_match(url) {
            bail!(
                "The source URL {} is invalid, ssh URLs should have a port number after \":\".\nUse ssh://git@example.com:22/path or just git@example.com:path if you do not want to provide a password or custom port.",
                sanitize(url)
            );
        }
        // runs all commands with `u` as %url%, returning the failed command and its error output
        let run = |u: &str| -> Result<(), (String, String)> {
            let sanitized = strip_credentials(u);
            for (i, cmd) in commands.iter().enumerate() {
                let args: Vec<&str> = cmd
                    .iter()
                    .map(|a| match *a {
                        "%url%" => u,
                        "%sanitizedUrl%" => sanitized.as_str(),
                        a => a,
                    })
                    .collect();
                let o = self.git(&args, if initial_clone && i == 0 { None } else { cwd });
                if !o.ok {
                    return Err((format!("git {}", args.join(" ")), o.stderr));
                }
            }
            Ok(())
        };
        let wipe_clone = || {
            if initial_clone && let Some(dir) = cwd {
                let _ = fs::remove_directory(dir);
            }
        };

        let domains = self.domains_regex("github-domains");
        let protocols = url::string_list(self.config, "github-protocols");
        // public github: try each configured protocol
        if let Some(c) = re(&format!(r"^(?:https?|git)://({domains})/(.*)")).captures(url) {
            let mut messages = vec![];
            for protocol in &protocols {
                let proto_url = if protocol == "ssh" { format!("git@{}:{}", &c[1], &c[2]) } else { format!("{protocol}://{}/{}", &c[1], &c[2]) };
                let Err((_, stderr)) = run(&proto_url) else { return Ok(()) };
                messages.push(format!("- {proto_url}\n{}", re("(?m)^").replace_all(&stderr, "  ")));
                wipe_clone();
            }
            if self.auth(&c[1]).is_none() {
                return self.git_failure(
                    &format!("Failed to clone {url} via {} protocols, aborting.\n\n{}", protocols.join(", "), messages.join("\n")),
                    url,
                );
            }
        }

        // private github repositories without ssh access are retried over https with credentials
        let ssh_github = re(&format!(r"(?i)^git@({domains}):(.+?)\.git$"));
        let bypass_ssh = ssh_github.is_match(url) && !protocols.iter().any(|p| p == "ssh");
        let mut failure = if bypass_ssh {
            (String::new(), String::new())
        } else {
            match run(url) {
                Ok(()) => return Ok(()),
                Err(failure) => failure,
            }
        };
        if let Some(auth_url) = self.auth_url(url, &failure.1) {
            match run(&auth_url) {
                Ok(()) => return Ok(()),
                Err(f) => failure = f,
            }
        }
        wipe_clone();
        self.git_failure(&format!("Failed to execute {}\n\n{}", failure.0, failure.1), url)
    }

    /// `Git::throwException()`.
    fn git_failure(&self, message: &str, url: &str) -> Result<()> {
        if git_version().is_none() {
            bail!(sanitize(&format!("Failed to clone {url}, git was not found, check that it is installed and in your PATH env.\n\n")));
        }
        bail!(sanitize(message))
    }

    /// Credentials from `auth.json`/config, as `IOInterface::loadConfiguration()` registers them.
    fn auth(&self, host: &str) -> Option<(String, String)> {
        let get = |key: &str| self.config.raw(key).and_then(|v| v.get(host)).cloned();
        if let Some(basic) = get("http-basic") {
            return Some((basic.get("username")?.as_str()?.into(), basic.get("password")?.as_str()?.into()));
        }
        match get("gitlab-token") {
            Some(Value::String(token)) => return Some((token, "private-token".into())),
            Some(Value::Object(t)) => return Some((t.get("username")?.as_str()?.into(), t.get("token")?.as_str()?.into())),
            _ => {}
        }
        if let Some(Value::String(token)) = get("gitlab-oauth") {
            return Some((token, "oauth2".into()));
        }
        if let Some(Value::String(token)) = get("github-oauth") {
            return Some((token, "x-oauth-basic".into()));
        }
        None
    }

    /// The authenticated URL `Git::runCommand()` retries with, for GitHub, GitLab and other hosts.
    fn auth_url(&self, url: &str, stderr: &str) -> Option<String> {
        let (github, gitlab) = (self.domains_regex("github-domains"), self.domains_regex("gitlab-domains"));
        let enc = |(user, pass): (String, String)| (raw_url_encode(&user), raw_url_encode(&pass));
        if let Some(c) = re(&format!(r"(?i)^git@({github}):(.+?)\.git$"))
            .captures(url)
            .or_else(|| re(&format!(r"(?i)^https?://({github})/(.*?)(?:\.git)?$")).captures(url))
        {
            let (user, pass) = enc(self.auth(&c[1])?);
            return Some(format!("https://{user}:{pass}@{}/{}.git", &c[1], &c[2]));
        }
        if let Some(c) = re(&format!(r"(?i)^(git)@({gitlab}):(.+?\.git)$"))
            .captures(url)
            .or_else(|| re(&format!(r"(?i)^(https?)://({gitlab})/(.*)")).captures(url))
        {
            let scheme = if &c[1] == "git" { "https" } else { &c[1] };
            let (user, pass) = self.auth(&c[2])?;
            // tokens go in the user slot
            let (user, pass) = if ["private-token", "oauth2", "gitlab-ci-token"].contains(&pass.as_str()) { (pass, user) } else { (user, pass) };
            let (user, pass) = enc((user, pass));
            return Some(format!("{scheme}://{user}:{pass}@{}/{}", &c[2], &c[3]));
        }
        const AUTH_FAILURES: [&str; 5] = [
            "fatal: Authentication failed",
            "remote error: Invalid username or password.",
            "error: 401 Unauthorized",
            "fatal: unable to access",
            "fatal: could not read Username",
        ];
        let c = re(r"(?i)^(https?://)([^/]+)(.*)$").captures(url)?;
        if !AUTH_FAILURES.iter().any(|f| stderr.contains(f)) {
            return None;
        }
        let host = c[2].rsplit('@').next().unwrap_or(&c[2]);
        let (user, pass) = enc(self.auth(host)?);
        Some(format!("{}{user}:{pass}@{host}{}", &c[1], &c[3]))
    }
}

fn strs(v: &[String]) -> Vec<&str> {
    v.iter().map(String::as_str).collect()
}

fn network_disabled() -> bool {
    std::env::var("COMPOSER_DISABLE_NETWORK").is_ok_and(|v| !v.is_empty() && v != "0")
}

/// `Git::checkForRepoOwnershipError()`.
fn check_ownership(stderr: &str, path: &str) -> Result<()> {
    if stderr.contains("fatal: detected dubious ownership") {
        bail!("The repository at \"{path}\" does not have the correct ownership and git refuses to use it:\n\n{stderr}");
    }
    Ok(())
}

/// `GitDownloader::getUnpushedChanges()` for the checkout `git` runs in (it returns stdout, or fails).
pub fn unpushed_changes(git: &dyn Fn(&[&str]) -> Result<String>) -> Result<Option<String>> {
    let mut refs = git(&["show-ref", "--head", "-d"])?.trim().to_string();
    let Some(head) = re(r"(?mi)^([a-f0-9]+) HEAD$").captures(&refs).map(|c| c[1].to_string()) else { return Ok(None) };
    let heads = re(&format!(r"(?mi)^{} refs/heads/(.+)$", regex::escape(&head)));
    let candidates: Vec<String> = heads.captures_iter(&refs).map(|c| c[1].to_string()).collect();
    // not on a branch: an unmodified tag or a detached head
    let Some(mut branch) = candidates.first().cloned() else { return Ok(None) };

    // PHP treats "0" as false
    let has_changes = |u: &Option<String>| u.as_deref().is_some_and(|u| !u.is_empty() && u != "0");
    let mut unpushed: Option<String> = None;
    let mut branch_not_found = false;
    for pass in 0..=1 {
        let mut remote_branches = vec![];
        for candidate in &candidates {
            let remote = re(&format!(r"(?mi)^[a-f0-9]+ refs/remotes/((?:[^/]+)/{})$", regex::escape(candidate)));
            let found: Vec<String> = remote.captures_iter(&refs).map(|c| c[1].to_string()).collect();
            if !found.is_empty() {
                branch = candidate.clone();
                remote_branches = found;
                break;
            }
        }
        if remote_branches.is_empty() {
            unpushed = Some(format!("Branch {branch} could not be found on any remote and appears to be unpushed"));
            branch_not_found = true;
        } else {
            if branch_not_found {
                unpushed = None;
            }
            for remote in remote_branches {
                let diff = git(&["diff", "--name-status", &format!("{remote}...{branch}"), "--"])?.trim().to_string();
                // keep the shortest diff of all remote branches
                if unpushed.as_ref().is_none_or(|u| diff.len() < u.len()) {
                    unpushed = Some(diff);
                }
            }
        }
        if !has_changes(&unpushed) {
            break;
        }
        // outdated remotes can cause false positives, so fetch and check again
        if pass == 0 {
            let _ = git(&["fetch", "--all"]);
            refs = git(&["show-ref", "--head", "-d"])?.trim().to_string();
        }
    }
    Ok(if has_changes(&unpushed) { unpushed } else { None })
}

#[cfg(test)]
mod tests {
    use crate::installed::InstalledRepo;
    use crate::manager::InstallationManager;
    use crate::tests::{CWD, commit, config, git, package, tmp_dir};
    use komposer_package::PackageRef;
    use komposer_package::io::{Io, Verbosity};
    use komposer_package::operation::Operation;
    use serde_json::json;

    fn source(repo: &str, reference: &str) -> PackageRef {
        package(json!({"name": "foo/bar", "version": "dev-main", "source": {"type": "git", "url": repo, "reference": reference}}))
    }

    fn remote(dir: &str) -> String {
        let repo = format!("{dir}/remote");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        repo
    }

    fn no_fetch(_: Vec<crate::FetchJob>) -> Vec<anyhow::Result<()>> {
        panic!("git packages are not fetched over http")
    }

    #[test]
    fn clones_updates_and_guards_local_changes() {
        // the initial clone runs in the process cwd, which other tests change
        let _cwd = CWD.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tmp_dir("git-install");
        let repo = remote(&dir);
        let first = commit(&repo, "file", "one");
        let (config, io) = (config(&dir, json!({})), Io::new(Verbosity::Normal, Some(false), false));
        let mut im = InstallationManager::new(&config, &io, &no_fetch);
        let installed_json = format!("{dir}/vendor/composer/installed.json");
        let mut repo_state = InstalledRepo::load(&installed_json, None).unwrap();
        let path = format!("{dir}/vendor/foo/bar");

        let v1 = source(&repo, &first);
        im.execute(&mut repo_state, &[Operation::Install(v1.clone())], true, false, None).unwrap();
        assert_eq!(im.output(), [format!("  - Installing foo/bar (dev-main {}): Cloning {}", &first[..7], &first[..10])]);
        assert_eq!(git(&path, &["rev-parse", "HEAD"]), first);
        assert_eq!(git(&path, &["rev-parse", "--abbrev-ref", "HEAD"]), "main", "checked out on the branch, not detached");
        assert_eq!(std::fs::read_to_string(format!("{path}/file")).unwrap(), "one");
        assert!(std::fs::read_to_string(&installed_json).unwrap().contains("\"installation-source\": \"source\""));

        let second = commit(&repo, "file", "two");
        let v2 = source(&repo, &second);
        im.execute(&mut repo_state, &[Operation::Update { initial: v1, target: v2.clone() }], true, false, None).unwrap();
        assert_eq!(
            im.output(),
            [format!("  - Upgrading foo/bar (dev-main {} => dev-main {}): Checking out {}", &first[..7], &second[..7], &second[..10])]
        );
        assert_eq!(git(&path, &["rev-parse", "HEAD"]), second);

        // uncommitted changes block the update unless discard-changes says otherwise
        std::fs::write(format!("{path}/file"), "local edit").unwrap();
        let third = commit(&repo, "file", "three");
        let v3 = source(&repo, &third);
        let err = im.execute(&mut repo_state, &[Operation::Update { initial: v2.clone(), target: v3.clone() }], true, false, None).unwrap_err();
        assert_eq!(err.to_string(), format!("Source directory {path} has uncommitted changes."));
        assert_eq!(im.output(), ["    Update of foo/bar failed"]);
        assert_eq!(std::fs::read_to_string(format!("{path}/file")).unwrap(), "local edit", "nothing was touched");

        let discarding = crate::tests::config(&dir, json!({"discard-changes": true}));
        let mut im = InstallationManager::new(&discarding, &io, &no_fetch);
        im.execute(&mut repo_state, &[Operation::Update { initial: v2, target: v3.clone() }], true, false, None).unwrap();
        assert_eq!(std::fs::read_to_string(format!("{path}/file")).unwrap(), "three");
        im.output();

        // unpushed commits block updates, unless discard-changes is true (non-interactive)
        let mut im = InstallationManager::new(&config, &io, &no_fetch);
        commit(&path, "mine", "unpushed work");
        let fourth = commit(&repo, "file", "four");
        let err =
            im.execute(&mut repo_state, &[Operation::Update { initial: v3.clone(), target: source(&repo, &fourth) }], true, false, None).unwrap_err();
        assert_eq!(err.to_string(), format!("Source directory {path} has unpushed changes on the current branch: \nA\tmine"));

        // ...and uninstalls
        assert!(im.execute(&mut repo_state, &[Operation::Uninstall(v3.clone())], true, false, None).is_err());
        assert_eq!(im.output(), ["    Update of foo/bar failed", "    Uninstall of foo/bar failed"]);
        git(&path, &["reset", "-q", "--hard", "HEAD~1"]);
        im.execute(&mut repo_state, &[Operation::Uninstall(v3)], true, false, None).unwrap();
        assert_eq!(im.output(), [format!("  - Removing foo/bar (dev-main {})", &third[..7])]);
        assert!(!std::path::Path::new(&path).exists());
    }

    #[test]
    fn syncs_the_vcs_cache_and_clones_from_it() {
        let _cwd = CWD.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tmp_dir("git-mirror");
        let repo = remote(&dir);
        let first = commit(&repo, "file", "one");
        let (config, io) = (config(&dir, json!({})), Io::new(Verbosity::Normal, Some(false), false));
        let im = InstallationManager::new(&config, &io, &no_fetch);
        let cache = im.cache_path(&repo);
        assert!(cache.starts_with(&format!("{dir}/cache/vcs/")) && cache.ends_with("-remote/"), "{cache}");

        assert!(im.fetch_ref_or_sync_mirror(&repo, &cache, &first, "dev-main").unwrap());
        assert_eq!(git(&cache, &["rev-parse", "--git-dir"]), ".", "a bare mirror");
        let second = commit(&repo, "file", "two");
        assert!(im.fetch_ref_or_sync_mirror(&repo, &cache, &second, "dev-main").unwrap(), "missing refs trigger a sync");
        assert!(!im.fetch_ref_or_sync_mirror(&repo, &cache, &"0".repeat(40), "dev-main").unwrap());

        // the cached clone branch of doInstall()
        let op = Operation::Install(source(&repo, &second));
        let mut job = im.job(&InstalledRepo::in_memory(), &op).unwrap();
        job.package.installation_source = Some("source".into());
        job.git_cached = true;
        let path = job.path.clone();
        im.git_install(&job, &path, "  - Installing foo/bar (dev-main x)").unwrap();
        assert_eq!(im.output(), [format!("  - Installing foo/bar (dev-main x): Cloning {} from cache", &second[..10])]);
        assert_eq!(git(&path, &["rev-parse", "HEAD"]), second);
        assert!(!std::path::Path::new(&format!("{path}/.git/objects/info/alternates")).exists(), "dissociated from the cache");
        assert_eq!(git(&path, &["remote", "get-url", "origin"]), repo);
    }
}
