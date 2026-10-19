//! `Composer\Util\Git`: running git with Composer's environment, protocol fallbacks and
//! credential handling for GitHub, GitLab, Bitbucket and http-basic hosts, plus the mirror cache.

use crate::auth::{bitbucket_authorize_oauth, github_authorize_oauth, gitlab_authorize_oauth, string_list};
use crate::http::HttpClient;
use crate::url::{NON_SECRET_CREDENTIALS, sanitize};
use crate::util::{ProcessOutput, execute, rawurldecode, split_lines};
use anyhow::{Result, bail};
use komposer_package::io::Verbosity;
use regex::Regex;
use std::cmp::Ordering;
use std::ffi::OsStr;
use std::path::Path;
use std::process::Command;
use std::sync::{LazyLock, OnceLock};

/// `git` with `Git::cleanEnv()` applied: no prompts, no inherited `GIT_DIR`/`GIT_WORK_TREE`,
/// predictable `LANGUAGE`.
pub fn command<I, S>(args: I) -> Command
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut cmd = Command::new("git");
    cmd.args(args);
    clean_env(&mut cmd);
    cmd
}

fn clean_env(cmd: &mut Command) {
    // GIT_TERMINAL_PROMPT exists since git 2.3.0, older ones get an askpass that answers nothing
    if version_at_least("2.3.0") {
        cmd.env("GIT_TERMINAL_PROMPT", "0");
    } else {
        cmd.env("GIT_ASKPASS", "echo");
    }
    cmd.env_remove("GIT_DIR").env_remove("GIT_WORK_TREE").env("LANGUAGE", "C").env_remove("DYLD_LIBRARY_PATH");
}

/// Runs `git <args>` in `cwd`.
pub fn run(args: &[&str], cwd: Option<&Path>) -> ProcessOutput {
    execute(command(args), cwd)
}

/// `Git::getVersion()`, cached.
pub fn version() -> Option<String> {
    static VERSION: OnceLock<Option<String>> = OnceLock::new();
    VERSION
        .get_or_init(|| {
            static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^git version (\d+(?:\.\d+)+)").unwrap());
            let mut cmd = Command::new("git");
            cmd.arg("--version").env("LANGUAGE", "C");
            let out = execute(cmd, None);
            out.ok().then(|| RE.captures(&out.stdout).map(|c| c[1].to_string())).flatten()
        })
        .clone()
}

fn version_at_least(min: &str) -> bool {
    version().is_some_and(|v| komposer_semver::version_compare(&v, min) != Ordering::Less)
}

/// `Git::getNoShowSignatureFlags()`.
pub fn no_show_signature_flags() -> Vec<&'static str> {
    if version_at_least("2.10.0-rc0") { vec!["--no-show-signature"] } else { vec![] }
}

/// `Git::buildRevListCommand()`.
pub fn rev_list_command<'a>(args: &[&'a str]) -> Vec<&'a str> {
    let mut cmd = vec!["rev-list"];
    if version_at_least("2.33.0-rc0") {
        cmd.push("--no-commit-header");
    }
    cmd.extend_from_slice(args);
    cmd
}

/// `Git::parseRevListOutput()`: drops `commit <hash>` lines of old gits.
pub fn parse_rev_list_output(output: &str) -> String {
    static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^commit [a-f0-9]{40}\n?").unwrap());
    if version_at_least("2.33.0-rc0") { output.to_string() } else { RE.replace_all(output, "").into_owned() }
}

/// `Git::checkForRepoOwnershipError()`.
pub fn check_for_repo_ownership_error(output: &str, path: &Path) -> Result<()> {
    if output.contains("fatal: detected dubious ownership") {
        bail!("The repository at \"{}\" does not have the correct ownership and git refuses to use it:\n\n{output}", path.display());
    }
    Ok(())
}

/// `Git::maskCredentials()`.
fn mask_credentials(text: &str, credentials: &[String]) -> String {
    let mut out = text.to_string();
    for c in credentials.iter().filter(|c| !c.is_empty()) {
        let masked = if NON_SECRET_CREDENTIALS.contains(&c.as_str()) {
            c.clone()
        } else if c.len() > 6 {
            format!("{}...{}", &c[..3], &c[c.len() - 3..])
        } else if c.len() > 3 {
            format!("{}...", &c[..3])
        } else {
            "XXX".into()
        };
        out = out.replace(c.as_str(), &masked);
    }
    out
}

fn rawurlencode(s: &str) -> String {
    crate::util::urlencode(s).replace('+', "%20").replace("%7E", "~")
}

/// One run of a command list against one URL.
struct Attempt {
    ok: bool,
    output: String,
    last_command: String,
    stderr: String,
}

/// `Composer\Util\Git` bound to the HTTP client's config and credentials.
#[derive(Clone)]
pub struct Git {
    http: HttpClient,
}

impl Git {
    pub fn new(http: HttpClient) -> Git {
        Git { http }
    }

    fn domains_regex(&self, key: &str) -> String {
        let domains = match key {
            "github-domains" => self.http.github_domains().to_vec(),
            _ => self.http.gitlab_domains().to_vec(),
        };
        format!("({})", domains.iter().map(|d| regex::escape(d)).collect::<Vec<_>>().join("|"))
    }

    fn attempt(commands: &[Vec<String>], url: &str, cwd: Option<&Path>, initial_clone: bool) -> Attempt {
        let mut attempt = Attempt { ok: true, output: String::new(), last_command: String::new(), stderr: String::new() };
        for (i, cmd) in commands.iter().enumerate() {
            let args: Vec<String> = cmd
                .iter()
                .map(|a| match a.as_str() {
                    "%url%" => url.to_string(),
                    "%sanitizedUrl%" => crate::url::strip_credentials(url),
                    _ => a.clone(),
                })
                .collect();
            attempt.last_command = format!("git {}", args.join(" "));
            let out = execute(command(&args), if initial_clone && i == 0 { None } else { cwd });
            attempt.output.push_str(&out.stdout);
            attempt.ok = out.ok();
            attempt.stderr = out.stderr;
            if !attempt.ok {
                break;
            }
        }
        attempt
    }

    /// `Git::runCommands()`: runs git subcommands (`%url%`/`%sanitizedUrl%` placeholders) against
    /// `url`, retrying with other protocols or stored credentials like Composer. Returns stdout.
    pub fn run_commands(&self, commands: &[&[&str]], url: &str, cwd: Option<&Path>, initial_clone: bool) -> Result<String> {
        static SSH_PORT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^ssh://[^@]+@[^:]+:[^0-9]+").unwrap());
        static REMOTE_CREDS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?im)^(?:composer|origin)\s+https?://(.+):(.+)@([^/]+)").unwrap());
        let commands: Vec<Vec<String>> = commands.iter().map(|c| c.iter().map(|s| s.to_string()).collect()).collect();
        let auth = self.http.auth();
        let config = self.http.config();
        let io = self.http.io();
        self.http.prohibit_url_by_config(url, None)?;
        if SSH_PORT.is_match(url) {
            bail!(
                "The source URL {} is invalid, ssh URLs should have a port number after \":\".\nUse ssh://git@example.com:22/path or just git@example.com:path if you do not want to provide a password or custom port.",
                sanitize(url)
            );
        }
        let remove_clone_dir = || {
            if let (true, Some(dir)) = (initial_clone, cwd) {
                let _ = std::fs::remove_dir_all(dir);
            }
        };
        if !initial_clone {
            // capture username/password from the remote URL if there is one and we have no auth configured yet
            let out = run(&["remote", "-v"], cwd);
            if let Some(c) = REMOTE_CREDS.captures(&out.stdout)
                && !auth.has(&c[3])
            {
                auth.set(&c[3], &rawurldecode(&c[1]), Some(&rawurldecode(&c[2])));
            }
        }

        let gh = self.domains_regex("github-domains");
        let protocols = string_list(config, "github-protocols");
        // public github, autoswitch protocols
        if let Some(c) = Regex::new(&format!("^(?:https?|git)://{gh}/(.*)")).unwrap().captures(url) {
            let (domain, path) = (c[1].to_string(), c[2].to_string());
            let mut messages = vec![];
            for protocol in &protocols {
                let proto_url = if protocol == "ssh" { format!("git@{domain}:{path}") } else { format!("{protocol}://{domain}/{path}") };
                let attempt = Git::attempt(&commands, &proto_url, cwd, initial_clone);
                if attempt.ok {
                    return Ok(attempt.output);
                }
                messages.push(format!("- {proto_url}\n{}", attempt.stderr.lines().map(|l| format!("  {l}")).collect::<Vec<_>>().join("\n")));
                remove_clone_dir();
            }
            if !auth.has(&domain) {
                return Err(self.failure(
                    &format!("Failed to clone {url} via {} protocols, aborting.\n\n{}", protocols.join(", "), messages.join("\n")),
                    url,
                    "",
                ));
            }
        }

        let gh_ssh = Regex::new(&format!("(?i)^git@{gh}:(.+?)\\.git$")).unwrap();
        // a private github url with the ssh protocol disabled goes straight to https
        let bypass_ssh = gh_ssh.is_match(url) && !protocols.iter().any(|p| p == "ssh");
        let first = (!bypass_ssh).then(|| Git::attempt(&commands, url, cwd, initial_clone));
        let mut last = match first {
            Some(a) if a.ok => return Ok(a.output),
            Some(a) => a,
            None => Attempt { ok: false, output: String::new(), last_command: String::new(), stderr: String::new() },
        };
        let mut credentials: Vec<String> = vec![];
        let mut try_auth_url = |auth_url: &str, user: &str, pass: &str, last: &mut Attempt| -> Option<String> {
            let attempt = Git::attempt(&commands, auth_url, cwd, initial_clone);
            if attempt.ok {
                return Some(attempt.output);
            }
            credentials = vec![rawurlencode(user), rawurlencode(pass)];
            *last = attempt;
            None
        };

        let gh_https = Regex::new(&format!("(?i)^https?://{gh}/(.*?)(?:\\.git)?$")).unwrap();
        let gl = self.domains_regex("gitlab-domains");
        static BB_HTTPS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^(https?)://(bitbucket\.org)/(.*?)(?:\.git)?$").unwrap());
        static BB_SSH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^(git)@(bitbucket\.org):(.+?\.git)$").unwrap());
        let gl_ssh = Regex::new(&format!("(?i)^(git)@{gl}:(.+?\\.git)$")).unwrap();
        let gl_https = Regex::new(&format!("(?i)^(https?)://{gl}/(.*)")).unwrap();

        if let Some(c) = gh_ssh.captures(url).or_else(|| gh_https.captures(url)) {
            // private github repository without ssh key access, try https with auth
            let (domain, path) = (c[1].to_string(), c[2].to_string());
            if !auth.has(&domain) {
                github_authorize_oauth(auth, self.http.github_domains(), &domain);
            }
            if let Some(a) = auth.get(&domain) {
                let pass = a.password.unwrap_or_default();
                let auth_url = format!("https://{}:{}@{domain}/{path}.git", rawurlencode(&a.username), rawurlencode(&pass));
                if let Some(out) = try_auth_url(&auth_url, &a.username, &pass, &mut last) {
                    return Ok(out);
                }
            }
        } else if let Some(c) = BB_HTTPS.captures(url).or_else(|| BB_SSH.captures(url)) {
            // bitbucket either through oauth or app password, with fallback to ssh
            let domain = c[2].to_string();
            let mut repo = c[3].to_string();
            if !repo.ends_with(".git") {
                repo.push_str(".git");
            }
            if !auth.has(&domain) {
                bitbucket_authorize_oauth(auth, &domain);
            }
            if let Some(mut a) = auth.get(&domain) {
                let pass = a.password.clone().unwrap_or_default();
                // Bitbucket API tokens authenticate git with a fixed username
                let user = if pass.starts_with("ATAT") { "x-bitbucket-api-token-auth".to_string() } else { a.username.clone() };
                let auth_url = format!("https://{}:{}@{domain}/{repo}", rawurlencode(&user), rawurlencode(&pass));
                if let Some(out) = try_auth_url(&auth_url, &user, &pass, &mut last) {
                    return Ok(out);
                }
                if a.username != "x-token-auth" {
                    let token = self.http.bitbucket_request_token(&domain, &a.username, &pass)?;
                    if !token.is_empty() {
                        auth.set(&domain, "x-token-auth", Some(&token));
                    }
                }
                if let Some(updated) = auth.get(&domain) {
                    a = updated;
                    let pass = a.password.unwrap_or_default();
                    let auth_url = format!("https://{}:{}@{domain}/{repo}", rawurlencode(&a.username), rawurlencode(&pass));
                    if let Some(out) = try_auth_url(&auth_url, &a.username, &pass, &mut last) {
                        return Ok(out);
                    }
                }
            }
            let ssh_url = format!("git@bitbucket.org:{repo}");
            io.write_error("    No bitbucket authentication configured. Falling back to ssh.");
            let attempt = Git::attempt(&commands, &ssh_url, cwd, initial_clone);
            if attempt.ok {
                return Ok(attempt.output);
            }
            last = attempt;
        } else if let Some(c) = gl_ssh.captures(url).or_else(|| gl_https.captures(url)) {
            let scheme = if &c[1] == "git" { "https".to_string() } else { c[1].to_string() };
            let (domain, path) = (c[2].to_string(), c[3].to_string());
            if !auth.has(&domain) {
                gitlab_authorize_oauth(auth, config, self.http.gitlab_domains(), &domain);
            }
            if let Some(a) = auth.get(&domain) {
                let pass = a.password.unwrap_or_default();
                let (user, pass) =
                    if matches!(pass.as_str(), "private-token" | "oauth2" | "gitlab-ci-token") { (pass, a.username) } else { (a.username, pass) };
                let auth_url = format!("{scheme}://{}:{}@{domain}/{path}", rawurlencode(&user), rawurlencode(&pass));
                if let Some(out) = try_auth_url(&auth_url, &user, &pass, &mut last) {
                    return Ok(out);
                }
            }
        } else if let Some((prefix, mut host, rest)) = auth_failure(url, &last.stderr) {
            // private non-github/gitlab/bitbucket repo that failed to authenticate
            if let Some((_, h)) = host.split_once('@') {
                host = h.to_string();
            }
            if let Some(a) = auth.get(&host) {
                let pass = a.password.unwrap_or_default();
                let auth_url = format!("{prefix}{}:{}@{host}{rest}", rawurlencode(&a.username), rawurlencode(&pass));
                if let Some(out) = try_auth_url(&auth_url, &a.username, &pass, &mut last) {
                    return Ok(out);
                }
            }
        }

        remove_clone_dir();
        let last_command = mask_credentials(&last.last_command, &credentials);
        let error = mask_credentials(&last.stderr, &credentials);
        Err(self.failure(&format!("Failed to execute {last_command}\n\n{error}"), url, &last.stderr))
    }

    /// `Git::throwException()`.
    fn failure(&self, message: &str, url: &str, stderr: &str) -> anyhow::Error {
        if version().is_none() {
            return anyhow::anyhow!(sanitize(&format!(
                "Failed to clone {url}, git was not found, check that it is installed and in your PATH env.\n\n{stderr}"
            )));
        }
        anyhow::anyhow!(sanitize(message))
    }

    fn is_mirror(dir: &Path) -> (bool, String) {
        if !dir.is_dir() {
            return (false, String::new());
        }
        let out = run(&["rev-parse", "--git-dir"], Some(dir));
        (out.ok() && out.stdout.trim() == ".", out.stderr)
    }

    /// `Git::syncMirror()`: clones or updates a bare mirror of `url` in `dir`.
    pub fn sync_mirror(&self, url: &str, dir: &Path) -> Result<bool> {
        let disabled = std::env::var("COMPOSER_DISABLE_NETWORK").ok().filter(|v| !v.is_empty() && v != "0");
        if disabled.is_some_and(|v| v != "prime") {
            self.http.io().warning(&format!("Aborting git mirror sync of {} as network is disabled", sanitize(url)));
            return Ok(false);
        }
        let (is_mirror, stderr) = Git::is_mirror(dir);
        if is_mirror {
            let updated = self.run_commands(
                &[&["remote", "set-url", "origin", "--", "%url%"], &["remote", "update", "--prune", "origin"], &["gc", "--auto"]],
                url,
                Some(dir),
                false,
            );
            let restored = self.run_commands(&[&["remote", "set-url", "origin", "--", "%sanitizedUrl%"]], url, Some(dir), false);
            if let Err(e) = updated.and(restored) {
                self.http.io().write_error_at(&format!("<error>Sync mirror failed: {e}</error>"), Verbosity::Debug);
                return Ok(false);
            }
            return Ok(true);
        }
        check_for_repo_ownership_error(&stderr, dir)?;
        // clean up the directory and do a fresh clone into it
        let _ = std::fs::remove_dir_all(dir);
        let dir_arg = dir.to_string_lossy();
        self.run_commands(&[&["clone", "--mirror", "--", "%url%", &dir_arg]], url, Some(dir), true)?;
        self.run_commands(&[&["remote", "set-url", "origin", "--", "%sanitizedUrl%"]], url, Some(dir), false)?;
        Ok(true)
    }

    /// `Git::fetchRefOrSyncMirror()`: makes sure `reference` is in the mirror.
    pub fn fetch_ref_or_sync_mirror(&self, url: &str, dir: &Path, reference: &str, pretty_version: Option<&str>) -> Result<bool> {
        static SHA: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-f0-9]{40}$").unwrap());
        static DEV: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)(?:^dev-|(?:\.x)?-dev$)").unwrap());
        if self.ref_in_mirror(dir, reference)? {
            if let Some(pretty) = pretty_version.filter(|_| SHA.is_match(reference)) {
                // a tag created after the ref was cached is only found by syncing the mirror
                let branch = regex::escape(&DEV.replace_all(pretty, ""));
                let (branches, tags) = (run(&["branch"], Some(dir)), run(&["tag"], Some(dir)));
                let in_branches = Regex::new(&format!(r"(?m)^[\s*]*v?{branch}$")).unwrap().is_match(&branches.stdout);
                let in_tags = Regex::new(&format!(r"(?m)^[\s*]*{branch}$")).unwrap().is_match(&tags.stdout);
                if branches.ok() && tags.ok() && !in_branches && !in_tags {
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

    fn ref_in_mirror(&self, dir: &Path, reference: &str) -> Result<bool> {
        let (is_mirror, stderr) = Git::is_mirror(dir);
        if is_mirror && run(&["rev-parse", "--quiet", "--verify", &format!("{reference}^{{commit}}")], Some(dir)).ok() {
            return Ok(true);
        }
        check_for_repo_ownership_error(&stderr, dir)?;
        Ok(false)
    }

    /// `Git::getMirrorDefaultBranch()`: the remote's HEAD branch.
    pub fn mirror_default_branch(&self, url: &str, dir: &Path, is_local: bool) -> Option<String> {
        static HEAD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*HEAD branch:\s(.+)\s*$").unwrap());
        if std::env::var("COMPOSER_DISABLE_NETWORK").is_ok_and(|v| !v.is_empty() && v != "0") {
            return None;
        }
        let output = if is_local {
            Ok(run(&["remote", "show", "origin"], Some(dir)).stdout)
        } else {
            let out = self.run_commands(&[&["remote", "set-url", "origin", "--", "%url%"], &["remote", "show", "origin"]], url, Some(dir), false);
            let _ = self.run_commands(&[&["remote", "set-url", "origin", "--", "%sanitizedUrl%"]], url, Some(dir), false);
            out
        };
        match output {
            Ok(output) => split_lines(&output).into_iter().find_map(|l| HEAD.captures(l).map(|c| c[1].trim_end().to_string())),
            Err(e) => {
                self.http.io().write_error_at(&format!("<error>Failed to fetch root identifier from remote: {e}</error>"), Verbosity::Debug);
                None
            }
        }
    }
}

/// `Git::getAuthenticationFailure()`: `(scheme://, host, rest)` when git reported an auth failure.
fn auth_failure(url: &str, stderr: &str) -> Option<(String, String, String)> {
    static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^(https?://)([^/]+)(.*)$").unwrap());
    const FAILURES: [&str; 5] = [
        "fatal: Authentication failed",
        "remote error: Invalid username or password.",
        "error: 401 Unauthorized",
        "fatal: unable to access",
        "fatal: could not read Username",
    ];
    let c = RE.captures(url)?;
    FAILURES.iter().any(|f| stderr.contains(f)).then(|| (c[1].to_string(), c[2].to_string(), c[3].to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{commit_file, git, git_repo, temp_dir, test_config};
    use komposer_package::io::Io;
    use std::sync::Arc;

    fn git_util() -> Git {
        Git::new(HttpClient::new(Arc::new(Io::null()), &test_config()).unwrap())
    }

    #[test]
    fn masks_credentials() {
        assert_eq!(
            mask_credentials("https://abcdefgh:x-oauth-basic@x", &["abcdefgh".into(), "x-oauth-basic".into()]),
            "https://abc...fgh:x-oauth-basic@x"
        );
        assert_eq!(mask_credentials("u:abcd@", &["abcd".into()]), "u:abc...@");
        assert_eq!(mask_credentials("u:abc@", &["abc".into()]), "u:XXX@");
    }

    #[test]
    fn syncs_mirrors_and_sanitizes_remote_url() {
        let src = git_repo("git-src");
        let sha = commit_file(&src, "composer.json", "{}", "init");
        let mirror = temp_dir("git-mirror").join("mirror");
        let git_util = git_util();
        let url = format!("file://{}", src.display());
        assert!(git_util.sync_mirror(&url, &mirror).unwrap());
        assert_eq!(git(&mirror, &["rev-parse", "--git-dir"]), ".");
        assert_eq!(git(&mirror, &["remote", "get-url", "origin"]), url);
        // update path
        let sha2 = commit_file(&src, "a.txt", "a", "second");
        assert!(git_util.sync_mirror(&url, &mirror).unwrap());
        assert!(git_util.fetch_ref_or_sync_mirror(&url, &mirror, &sha2, None).unwrap());
        assert!(git_util.fetch_ref_or_sync_mirror(&url, &mirror, &sha, Some("dev-main")).unwrap());
        assert!(!git_util.fetch_ref_or_sync_mirror(&url, &mirror, "0000000000000000000000000000000000000000", None).unwrap());
    }

    #[test]
    fn failure_messages_are_sanitized() {
        let dir = temp_dir("git-fail");
        let err = git_util()
            .run_commands(&[&["ls-remote", "--heads", "--", "%url%"]], "https://user:pass@127.0.0.1:1/nope.git", Some(&dir), false)
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.starts_with("Failed to execute git ls-remote --heads -- https://user:***@127.0.0.1:1/nope.git"), "{msg}");
        assert!(!msg.contains("pass@"));
    }
}
