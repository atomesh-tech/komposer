//! `VcsRepository` with git drivers: `GitDriver` (the `git` CLI on a mirror in `cache-vcs-dir`)
//! and, for GitHub URLs, `GitHubDriver`'s package data: the API's repository data, `support`
//! and `funding` information and zipball dists, with git (not the API) reading tags, branches
//! and composer.json files. GitLab, Bitbucket and Forgejo URLs are read as plain git
//! repositories (source only); hg, svn, fossil and perforce are not supported.

use crate::cache::Cache;
use crate::composer::cache_dir_name;
use crate::git::{self, Git};
use crate::http::{HttpClient, RequestOptions, error_status};
use crate::url::sanitize;
use crate::util::{is_local_path, platform_path, split_lines};
use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use komposer_package::PackageRef;
use komposer_package::io::{Io, Verbosity};
use komposer_package::json::{self, Flags};
use komposer_package::loader::{self, LoadOptions, php_empty};
use komposer_package::repository::{ArrayRepository, LoadRequest, LoadResult, RepoKind, Repository};
use komposer_semver::DEFAULT_BRANCH_ALIAS;
use regex::Regex;
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex};

static GITHUB_URL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:(?:https?|git)://([^/]+)/|git@([^:]+):/?)([^/]+)/([^/]+?)(?:\.git|/)?$").unwrap());

/// `GitHubDriver` state.
struct GitHub {
    origin: String,
    owner: String,
    repo: String,
    /// repository data came from the API (not `no-api`, no clone fallback)
    api: bool,
    private: bool,
    has_issues: bool,
    archived: bool,
    funding: Option<Option<Vec<Value>>>,
}

impl GitHub {
    fn api_url(&self) -> String {
        if self.origin == "github.com" { "https://api.github.com".into() } else { format!("https://{}/api/v3", self.origin) }
    }

    fn https_url(&self) -> String {
        format!("https://{}/{}/{}.git", self.origin, self.owner, self.repo)
    }

    /// `generateSshUrl()`.
    fn ssh_url(&self) -> String {
        if self.origin.contains(':') {
            format!("ssh://git@{}/{}/{}.git", self.origin, self.owner, self.repo)
        } else {
            format!("git@{}:{}/{}.git", self.origin, self.owner, self.repo)
        }
    }
}

/// `GitDriver` (+ `GitHubDriver`).
struct Driver {
    http: HttpClient,
    url: String,
    repo_dir: PathBuf,
    tags: Vec<(String, String)>,
    branches: Vec<(String, String)>,
    root_identifier: String,
    cache: Cache,
    info_cache: HashMap<String, Option<Map<String, Value>>>,
    github: Option<GitHub>,
}

impl Driver {
    fn new(http: &HttpClient, url: &str, kind: &str, repo_config: &Value) -> Result<Driver> {
        let url = if is_local_path(url) { platform_path(url) } else { url.to_string() };
        let github = if kind == "github" || (kind == "vcs" && github_supports(http, &url)) {
            let c = GITHUB_URL.captures(&url).ok_or_else(|| anyhow!("The GitHub repository URL {} is invalid.", sanitize(&url)))?;
            let origin = c.get(1).or(c.get(2)).map_or("", |m| m.as_str()).to_lowercase();
            Some(GitHub {
                origin: if origin == "www.github.com" { "github.com".into() } else { origin },
                owner: c[3].to_string(),
                repo: c[4].to_string(),
                api: false,
                private: false,
                has_issues: false,
                archived: false,
                funding: None,
            })
        } else {
            None
        };
        match github {
            Some(gh) => Driver::init_github(http, url, gh, repo_config),
            None => Driver::init_git(http, &url, None),
        }
    }

    /// `GitHubDriver::initialize()` + `fetchRootIdentifier()`.
    fn init_github(http: &HttpClient, url: String, mut gh: GitHub, repo_config: &Value) -> Result<Driver> {
        let allow_fallback = repo_config.get("allow-git-fallback") != Some(&Value::Bool(false));
        let use_api = http.config().get("use-github-api") != Some(Value::Bool(false)) && !repo_config.get("no-api").is_some_and(loader::php_truthy);
        if !use_api {
            if !allow_fallback {
                bail!("Fallback to git driver disabled");
            }
            return Driver::init_git(http, &url, Some(gh));
        }
        let options = RequestOptions::from_context(repo_config.get("options").unwrap_or(&Value::Null));
        let repo_data_url = format!("{}/repos/{}/{}", gh.api_url(), gh.owner, gh.repo);
        let mut response = http.get(&repo_data_url, &options);
        if let Err(e) = &response
            && matches!(error_status(e), Some(401 | 403 | 404))
            && !http.auth().has(&gh.origin)
            && crate::auth::github_authorize_oauth(http.auth(), http.github_domains(), &gh.origin)
        {
            response = http.get(&repo_data_url, &options);
        }
        let data = match response {
            Ok(r) => r.json()?,
            // private repositories look like 404s; without a way to ask for credentials, clone instead
            Err(e) if matches!(error_status(&e), Some(401 | 403 | 404 | 499)) => {
                if !allow_fallback {
                    return Err(e.context("Fallback to git driver disabled"));
                }
                gh.private = true;
                let ssh = gh.ssh_url();
                return Driver::init_git(http, &ssh, Some(gh)).inspect_err(|_| {
                    http.io().error(&format!(
                        "Failed to clone the {ssh} repository, try running in interactive mode so that you can enter your GitHub credentials"
                    ));
                });
            }
            Err(e) => return Err(e),
        };
        let s = |k: &str| data.get(k).and_then(Value::as_str).map(String::from);
        if let Some(owner) = data.get("owner").and_then(|o| o.get("login")).and_then(Value::as_str) {
            gh.owner = owner.into();
        }
        if let Some(name) = s("name") {
            gh.repo = name;
        }
        gh.api = true;
        gh.private = data.get("private").is_some_and(loader::php_truthy);
        gh.has_issues = data.get("has_issues").is_some_and(loader::php_truthy);
        gh.archived = data.get("archived").is_some_and(loader::php_truthy);
        let root = s("default_branch").or_else(|| s("master_branch")).unwrap_or_else(|| "master".into());
        let mut driver = Driver::init_git(http, &gh.https_url(), Some(gh))?;
        driver.root_identifier = root;
        Ok(driver)
    }

    /// `GitDriver::initialize()`.
    fn init_git(http: &HttpClient, url: &str, github: Option<GitHub>) -> Result<Driver> {
        static DOT_GIT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\\/]\.git/?$").unwrap());
        static SSH_PORT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^ssh://[^@]+@[^:]+:[^0-9]+").unwrap());
        let config = http.config();
        let local = is_local_path(url);
        let (url, repo_dir, cache_url) = if local {
            let url = DOT_GIT.replace(url, "").into_owned();
            if !std::path::Path::new(&url).is_dir() {
                bail!("Failed to read package information from {} as the path does not exist", sanitize(&url));
            }
            let real = std::fs::canonicalize(&url).map(|p| p.display().to_string()).unwrap_or_else(|_| url.clone());
            (url.clone(), PathBuf::from(&url), real)
        } else {
            let vcs_dir = config.get_string("cache-vcs-dir").unwrap_or_default();
            if !Cache::is_usable(&vcs_dir) {
                bail!("GitDriver requires a usable cache directory, and it looks like you set it to be disabled");
            }
            let repo_dir = PathBuf::from(format!("{vcs_dir}/{}/", cache_dir_name(url)));
            if let Some(parent) = repo_dir.parent() {
                std::fs::create_dir_all(parent).with_context(|| {
                    format!(
                        "Can not clone {} to access package information. The \"{}\" directory is not writable by the current user.",
                        sanitize(url),
                        parent.display()
                    )
                })?;
            }
            if SSH_PORT.is_match(url) {
                bail!(
                    "The source URL {} is invalid, ssh URLs should have a port number after \":\".\nUse ssh://git@example.com:22/path or just git@example.com:path if you do not want to provide a password or custom port.",
                    sanitize(url)
                );
            }
            if !Git::new(http.clone()).sync_mirror(url, &repo_dir)? {
                if !repo_dir.is_dir() {
                    bail!("Failed to clone {} to read package information from it", sanitize(url));
                }
                http.io().error(&format!("Failed to update {}, package information from this repository may be outdated", sanitize(url)));
            }
            (url.to_string(), repo_dir, url.to_string())
        };
        let repo_cache_dir = config.get_string("cache-repo-dir").unwrap_or_default();
        let cache_dir = match &github {
            Some(gh) if gh.api => format!("{repo_cache_dir}/{}/{}/{}", gh.origin, gh.owner, gh.repo),
            _ => format!("{repo_cache_dir}/{}", cache_dir_name(&cache_url)),
        };
        let mut driver = Driver {
            cache: Cache::new(http.io().clone(), cache_dir, "a-z0-9._").read_only(config.get_bool("cache-read-only")),
            http: http.clone(),
            url,
            repo_dir,
            tags: vec![],
            branches: vec![],
            root_identifier: String::new(),
            info_cache: HashMap::new(),
            github,
        };
        driver.tags = driver.read_tags();
        driver.branches = driver.read_branches();
        driver.root_identifier = driver.read_root_identifier(local);
        Ok(driver)
    }

    fn git(&self, args: &[&str]) -> crate::ProcessOutput {
        git::run(args, Some(&self.repo_dir))
    }

    /// `GitDriver::getRootIdentifier()`.
    fn read_root_identifier(&self, local: bool) -> String {
        static CURRENT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\* +(\S+)").unwrap());
        if !local && let Some(branch) = Git::new(self.http.clone()).mirror_default_branch(&self.url, &self.repo_dir, false) {
            return branch;
        }
        // the currently checked out branch
        let out = self.git(&["branch", "--no-color"]);
        let lines = split_lines(&out.stdout);
        if lines.contains(&"* master") {
            return "master".into();
        }
        lines.iter().find_map(|l| CURRENT.captures(l).map(|c| c[1].to_string())).unwrap_or_else(|| "master".into())
    }

    /// `GitDriver::getTags()`.
    fn read_tags(&self) -> Vec<(String, String)> {
        static TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^([a-f0-9]{40}) refs/tags/(\S+?)(\^\{\})?$").unwrap());
        let mut tags: Vec<(String, String)> = vec![];
        for line in split_lines(&self.git(&["show-ref", "--tags", "--dereference"]).stdout) {
            if let Some(c) = TAG.captures(line) {
                upsert(&mut tags, &c[2], &c[1]);
            }
        }
        tags
    }

    /// `GitDriver::getBranches()` (GitHub's API never lists `gh-pages`).
    fn read_branches(&self) -> Vec<(String, String)> {
        static REMOTE_HEAD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^ *[^/]+/HEAD ").unwrap());
        static BRANCH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(?:\* )? *(\S+) *([a-f0-9]+)(?: .*)?$").unwrap());
        let mut branches = vec![];
        for line in split_lines(&self.git(&["branch", "--no-color", "--no-abbrev", "-v"]).stdout) {
            if REMOTE_HEAD.is_match(line) {
                continue;
            }
            if let Some(c) = BRANCH.captures(line).filter(|c| !c[1].starts_with('-')) {
                if self.github.as_ref().is_some_and(|g| g.api) && &c[1] == "gh-pages" {
                    continue;
                }
                upsert(&mut branches, &c[1], &c[2]);
            }
        }
        branches
    }

    fn url(&self) -> String {
        match &self.github {
            Some(gh) if gh.api => gh.https_url(),
            _ => self.url.clone(),
        }
    }

    fn source(&self, identifier: &str) -> Value {
        let url = match &self.github {
            Some(gh) if gh.api && gh.private => gh.ssh_url(),
            _ => self.url(),
        };
        json!({"type": "git", "url": url, "reference": identifier})
    }

    fn dist(&self, identifier: &str) -> Value {
        match &self.github {
            Some(gh) => {
                json!({"type": "zip", "url": format!("{}/repos/{}/{}/zipball/{identifier}", gh.api_url(), gh.owner, gh.repo), "reference": identifier, "shasum": ""})
            }
            None => Value::Null,
        }
    }

    /// `GitDriver::getFileContent()`.
    fn file_content(&self, file: &str, identifier: &str) -> Result<Option<String>> {
        if identifier.starts_with('-') {
            bail!("Invalid git identifier detected. Identifier must not start with a -, given: {identifier}");
        }
        let out = self.git(&["show", &format!("{identifier}:{file}")]);
        Ok(Some(out.stdout).filter(|c| !c.trim().is_empty()))
    }

    /// `GitDriver::getChangeDate()` (`GitHubDriver` reports the committer date).
    fn change_date(&self, identifier: &str) -> Result<Option<String>> {
        if identifier.starts_with('-') {
            bail!("Invalid git identifier detected. Identifier must not start with a -, given: {identifier}");
        }
        let format = if self.github.as_ref().is_some_and(|g| g.api) { "--format=%ct" } else { "--format=%at" };
        let out = self.git(&git::rev_list_command(&["-n1", format, identifier]));
        Ok(loader::format_date(git::parse_rev_list_output(&out.stdout).trim()))
    }

    /// `VcsDriver::getComposerInformation()` (+ GitHub's `support`/`funding` additions).
    fn composer_information(&mut self, identifier: &str) -> Result<Option<Map<String, Value>>> {
        static SHA: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^[a-f0-9]{40}$").unwrap());
        if let Some(info) = self.info_cache.get(identifier) {
            return Ok(info.clone());
        }
        let cacheable = SHA.is_match(identifier);
        let cached = cacheable.then(|| self.cache.read(identifier)).flatten().filter(|c| !c.is_empty());
        let mut composer = match cached {
            Some(c) => json::parse(&c)?.as_object().cloned(),
            None => {
                let composer = self.base_composer_information(identifier)?;
                if cacheable {
                    let encoded = json::encode(
                        &composer.clone().map_or(Value::Null, Value::Object),
                        Flags { pretty: false, unescaped_slashes: true, unescaped_unicode: true },
                    );
                    self.cache.write(identifier, encoded.as_bytes());
                }
                composer
            }
        };
        if let Some(c) = composer.as_mut() {
            self.github_specials(c, identifier);
        }
        self.info_cache.insert(identifier.into(), composer.clone());
        Ok(composer)
    }

    /// `VcsDriver::getBaseComposerInformation()`.
    fn base_composer_information(&self, identifier: &str) -> Result<Option<Map<String, Value>>> {
        let Some(content) = self.file_content("composer.json", identifier)? else { return Ok(None) };
        let parsed = json::parse(&content).map_err(|_| anyhow!("\"{identifier}:composer.json\" does not contain valid JSON"))?;
        let Some(mut composer) = parsed.as_object().cloned().filter(|c| !c.is_empty()) else { return Ok(None) };
        if composer.get("time").is_none_or(php_empty)
            && let Some(date) = self.change_date(identifier)?
        {
            composer.insert("time".into(), date.into());
        }
        Ok(Some(composer))
    }

    fn github_specials(&mut self, composer: &mut Map<String, Value>, identifier: &str) {
        let Some(gh) = self.github.as_ref().filter(|g| g.api) else { return };
        let (origin, owner, repo, has_issues, archived) = (gh.origin.clone(), gh.owner.clone(), gh.repo.clone(), gh.has_issues, gh.archived);
        let support = composer.entry("support").or_insert_with(|| json!({}));
        if !support.is_object() {
            *support = json!({});
        }
        let support = support.as_object_mut().expect("support is a map");
        if !support.contains_key("source") {
            let label = [&self.tags, &self.branches]
                .iter()
                .find_map(|refs| refs.iter().find(|(_, sha)| sha == identifier).map(|(n, _)| n.clone()))
                .unwrap_or_else(|| identifier.to_string());
            support.insert("source".into(), format!("https://{origin}/{owner}/{repo}/tree/{label}").into());
        }
        if !support.contains_key("issues") && has_issues {
            support.insert("issues".into(), format!("https://{origin}/{owner}/{repo}/issues").into());
        }
        if !composer.contains_key("abandoned") && archived {
            composer.insert("abandoned".into(), true.into());
        }
        if !composer.contains_key("funding")
            && let Some(funding) = self.funding_info()
        {
            composer.insert("funding".into(), Value::Array(funding));
        }
    }

    /// `GitHubDriver::getFundingInfo()`: `.github/FUNDING.yml` of the repository or its owner.
    fn funding_info(&mut self) -> Option<Vec<Value>> {
        let gh = self.github.as_mut()?;
        if let Some(funding) = &gh.funding {
            return funding.clone();
        }
        if gh.origin != "github.com" {
            gh.funding = Some(None);
            return None;
        }
        let api = gh.api_url();
        let files = [
            format!("{api}/repos/{}/{}/contents/.github/FUNDING.yml", gh.owner, gh.repo),
            format!("{api}/repos/{}/.github/contents/FUNDING.yml", gh.owner),
        ];
        let opts = RequestOptions { retry_auth_failure: false, ..Default::default() };
        let yml = files.iter().find_map(|file| {
            let data = self.http.get(file, &opts).ok()?.json().ok()?;
            let content = data.get("content").and_then(Value::as_str).filter(|c| !c.is_empty())?;
            if data.get("encoding").and_then(Value::as_str) != Some("base64") {
                return None;
            }
            let clean: String = content.chars().filter(|c| !c.is_whitespace()).collect();
            base64::engine::general_purpose::STANDARD.decode(clean).ok().filter(|b| !b.is_empty()).map(|b| String::from_utf8_lossy(&b).into_owned())
        });
        let funding = yml.map(|y| parse_funding(&y, self.http.io())).filter(|f| !f.is_empty());
        let gh = self.github.as_mut()?;
        gh.funding = Some(funding.clone());
        funding
    }
}

fn upsert(list: &mut Vec<(String, String)>, key: &str, value: &str) {
    match list.iter_mut().find(|(k, _)| k == key) {
        Some(entry) => entry.1 = value.to_string(),
        None => list.push((key.to_string(), value.to_string())),
    }
}

/// `GitHubDriver::supports()`.
fn github_supports(http: &HttpClient, url: &str) -> bool {
    let Some(c) = GITHUB_URL.captures(url) else { return false };
    let origin = c.get(1).or(c.get(2)).map_or("", |m| m.as_str()).to_lowercase();
    let origin = origin.strip_prefix("www.").unwrap_or(&origin);
    http.github_domains().iter().any(|d| d == origin)
}

/// `GitDriver::supports()`: shallow URL/path checks, then `git ls-remote` when `deep`.
fn git_supports(http: &HttpClient, url: &str, deep: bool) -> bool {
    static GITISH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)(^git://|\.git/?$|git(?:olite)?@|//git\.|//github.com/)").unwrap());
    if GITISH.is_match(url) {
        return true;
    }
    if is_local_path(url) {
        let path = platform_path(url);
        if !std::path::Path::new(&path).is_dir() {
            return false;
        }
        if git::run(&["tag"], Some(std::path::Path::new(&path))).ok() {
            return true;
        }
    }
    deep && Git::new(http.clone()).run_commands(&[&["ls-remote", "--heads", "--", "%url%"]], url, Some(&std::env::temp_dir()), false).is_ok()
}

/// Parses FUNDING.yml the way `GitHubDriver::getFundingInfo()` does.
fn parse_funding(yml: &str, io: &Io) -> Vec<Value> {
    static KEY_VALUE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(\w+)\s*:\s*(.+)$").unwrap());
    static INLINE_LIST: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\[(.*?)\](?:\s*#.*)?$").unwrap());
    static LIST_SPLIT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"['"]?\s*,\s*['"]?"#).unwrap());
    static SCALAR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^([^#].*?)(?:\s+#.*)?$").unwrap());
    static KEY_ONLY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(\w+)\s*:\s*#\s*$").unwrap());
    static DASH_ITEM: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^-\s*(.+)(?:\s+#.*)?$").unwrap());
    static COMMA_ITEM: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(.+),(?:\s*#.*)?$").unwrap());
    static DOMAIN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-z0-9-]+\.[a-z]{2,3}$").unwrap());
    let quotes: &[char] = &['"', '\'', ' '];
    let mut result: Vec<(String, String)> = vec![];
    let mut key: Option<String> = None;
    for line in yml.split('\n').map(|l| l.trim()) {
        if let Some(c) = KEY_VALUE.captures(line) {
            if &c[2] == "[" {
                key = Some(c[1].to_string());
                continue;
            }
            if let Some(list) = INLINE_LIST.captures(&c[2]) {
                for item in LIST_SPLIT.split(&list[1]) {
                    result.push((c[1].to_string(), item.trim().trim_matches(quotes).to_string()));
                }
            } else if let Some(s) = SCALAR.captures(&c[2]) {
                result.push((c[1].to_string(), s[1].trim_matches(quotes).to_string()));
            }
            key = None;
        } else if let Some(c) = KEY_ONLY.captures(line) {
            key = Some(c[1].to_string());
        } else if let Some(k) = &key {
            if let Some(c) = DASH_ITEM.captures(line).or_else(|| COMMA_ITEM.captures(line)) {
                result.push((k.clone(), c[1].trim_matches(quotes).to_string()));
            } else if line == "]" {
                key = None;
            }
        }
    }
    let basename = |u: &str| u.trim_end_matches('/').rsplit('/').next().unwrap_or("").to_string();
    let mut out = vec![];
    for (kind, url) in result {
        let url = match kind.as_str() {
            "community_bridge" => format!("https://funding.communitybridge.org/projects/{}", basename(&url)),
            "github" => format!("https://github.com/{}", basename(&url)),
            "issuehunt" => format!("https://issuehunt.io/r/{url}"),
            "ko_fi" => format!("https://ko-fi.com/{}", basename(&url)),
            "liberapay" => format!("https://liberapay.com/{}", basename(&url)),
            "open_collective" => format!("https://opencollective.com/{}", basename(&url)),
            "patreon" => format!("https://www.patreon.com/{}", basename(&url)),
            "tidelift" => format!("https://tidelift.com/funding/github/{url}"),
            "polar" => format!("https://polar.sh/{}", basename(&url)),
            "buy_me_a_coffee" => format!("https://www.buymeacoffee.com/{}", basename(&url)),
            "thanks_dev" => format!("https://thanks.dev/{url}"),
            "otechie" => format!("https://otechie.com/{}", basename(&url)),
            "custom" => {
                let parts = crate::url::parse(&url);
                if parts.scheme.is_none() && parts.host.is_none() {
                    if DOMAIN.is_match(&url) {
                        format!("https://{url}")
                    } else {
                        io.warning(&format!("Funding URL {url} not in a supported format."));
                        continue;
                    }
                } else {
                    url
                }
            }
            _ => url,
        };
        out.push(json!({"type": kind, "url": url}));
    }
    out
}

/// `VcsRepository`.
pub struct VcsRepository {
    http: HttpClient,
    url: String,
    kind: String,
    repo_config: Value,
    loaded: Mutex<Option<Arc<ArrayRepository>>>,
}

impl VcsRepository {
    pub fn new(repo_config: &Value, http: HttpClient) -> Result<VcsRepository> {
        let url = komposer_package::config::expand_path(repo_config.get("url").and_then(Value::as_str).unwrap_or(""));
        let kind = repo_config.get("type").and_then(Value::as_str).unwrap_or("vcs").to_string();
        if matches!(kind.as_str(), "hg" | "svn" | "fossil" | "perforce") {
            bail!("Repositories of type \"{kind}\" are not supported by komposer ({})", sanitize(&url));
        }
        Ok(VcsRepository { http, url, kind, repo_config: repo_config.clone(), loaded: Mutex::new(None) })
    }

    pub fn repo_config(&self) -> &Value {
        &self.repo_config
    }

    fn repository(&self) -> Result<Arc<ArrayRepository>> {
        crate::util::lazy_repository(&self.loaded, || self.initialize())
    }

    /// `VcsRepository::initialize()`: one package per valid tag and branch.
    fn initialize(&self) -> Result<ArrayRepository> {
        static DEV_SUFFIX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)[.-]?dev$").unwrap());
        static DEV_AFFIX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)(^dev-|[.-]?dev$)").unwrap());
        static NINES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(\.9{7})+").unwrap());
        let io = self.http.io().clone();
        let very_verbose = io.is_very_verbose();
        let log = |msg: &str| {
            if very_verbose {
                io.write_error(msg)
            }
        };
        if !matches!(self.kind.as_str(), "github" | "git" | "gitlab" | "bitbucket" | "git-bitbucket" | "forgejo")
            && !github_supports(&self.http, &self.url)
            && !git_supports(&self.http, &self.url, false)
            && !git_supports(&self.http, &self.url, true)
        {
            bail!("No driver found to handle VCS repository {}", sanitize(&self.url));
        }
        let mut driver = Driver::new(&self.http, &self.url, &self.kind, &self.repo_config)?;
        let root = driver.root_identifier.clone();
        let mut repo = ArrayRepository::new(self.repo_name(), RepoKind::Remote);
        let mut package_name: Option<String> = None;

        let has_root_composer = match driver.composer_information(&root) {
            Ok(data) => {
                package_name = data.as_ref().and_then(|d| d.get("name")).and_then(Value::as_str).filter(|n| !n.is_empty()).map(String::from);
                data.is_some()
            }
            Err(e) => {
                if should_rethrow(&e) {
                    return Err(e);
                }
                log(&format!("<error>Skipped parsing {root}, {e}</error>"));
                false
            }
        };

        for (tag, identifier) in driver.tags.clone() {
            // strip the release- prefix from tags if present
            let tag = tag.replace("release-", "");
            let Ok(parsed_tag) = komposer_semver::normalize(&tag) else {
                log(&format!("<warning>Skipped tag {tag}, invalid tag name</warning>"));
                continue;
            };
            log(&format!(
                "Reading composer.json of <info>{}</info> (<comment>{tag}</comment>)",
                package_name.clone().unwrap_or_else(|| sanitize(&self.url))
            ));
            let result = (|| -> Result<Option<String>> {
                let Some(mut data) = driver.composer_information(&identifier)? else {
                    return Ok(Some(format!("Skipped tag {tag}, no composer file")));
                };
                let normalized = match data.get("version").and_then(Value::as_str) {
                    // manually versioned package
                    Some(v) => komposer_semver::normalize(v).map_err(|e| anyhow!("{e}"))?,
                    None => {
                        data.insert("version".into(), tag.clone().into());
                        parsed_tag.clone()
                    }
                };
                // tag packages have no -dev flag nor the default-branch marker
                let version = DEV_SUFFIX.replace(data["version"].as_str().unwrap_or(""), "").into_owned();
                let normalized = DEV_AFFIX.replace(&normalized, "").into_owned();
                data.insert("version".into(), version.into());
                data.insert("version_normalized".into(), normalized.clone().into());
                data.shift_remove("default-branch");
                if normalized != parsed_tag {
                    return Ok(Some(if DEV_AFFIX.is_match(&parsed_tag) {
                        format!("Skipped tag {tag}, invalid tag name, tags can not use dev prefixes or suffixes")
                    } else {
                        format!("Skipped tag {tag}, tag ({parsed_tag}) does not match version ({normalized}) in composer.json")
                    }));
                }
                let name =
                    package_name.clone().or_else(|| data.get("name").and_then(Value::as_str).map(String::from)).unwrap_or_default().to_lowercase();
                if let Some(existing) = repo.all().iter().find(|p| p.name == name && p.version == normalized) {
                    return Ok(Some(format!(
                        "Skipped tag {tag}, it conflicts with an another tag ({}) as both resolve to {normalized} internally",
                        existing.pretty_version
                    )));
                }
                log(&format!("Importing tag {tag} ({normalized})"));
                for p in loader::load(&self.pre_process(&driver, data, &identifier, package_name.as_deref()), LoadOptions::default())? {
                    repo.add(p);
                }
                Ok(None)
            })();
            match result {
                Ok(None) => {}
                Ok(Some(skipped)) => log(&format!("<warning>{skipped}</warning>")),
                Err(e) if should_rethrow(&e) => return Err(e),
                Err(e) => log(&format!("<warning>Skipped tag {tag}, {e}</warning>")),
            }
        }

        let mut branches = driver.branches.clone();
        // make sure the root identifier branch gets loaded first
        if has_root_composer && let Some(pos) = branches.iter().position(|(b, _)| *b == root) {
            let entry = branches.remove(pos);
            branches.insert(0, entry);
        }
        for (branch, identifier) in branches {
            log(&format!(
                "Reading composer.json of <info>{}</info> (<comment>{branch}</comment>)",
                package_name.clone().unwrap_or_else(|| sanitize(&self.url))
            ));
            let normalized = komposer_semver::normalize_branch(&branch);
            if komposer_semver::parse_constraints(&normalized).is_err() {
                log(&format!("<warning>Skipped branch {branch}, invalid name</warning>"));
                continue;
            }
            // branch packages have a dev flag
            let (version, normalized) = if normalized.starts_with("dev-") || normalized == DEFAULT_BRANCH_ALIAS {
                (format!("dev-{}", branch.replace('#', "+")), normalized.replace('#', "+"))
            } else {
                let prefix = if branch.starts_with('v') { "v" } else { "" };
                (format!("{prefix}{}", NINES.replace_all(&normalized, ".x")), normalized)
            };
            let result = (|| -> Result<bool> {
                let Some(mut data) = driver.composer_information(&identifier)? else { return Ok(false) };
                // branches are always auto-versioned, read value from branch name
                data.insert("version".into(), version.clone().into());
                data.insert("version_normalized".into(), normalized.clone().into());
                data.shift_remove("default-branch");
                if branch == root {
                    data.insert("default-branch".into(), true.into());
                }
                log(&format!("Importing branch {branch} ({version})"));
                for p in loader::load(&self.pre_process(&driver, data, &identifier, package_name.as_deref()), LoadOptions::default())? {
                    repo.add(p);
                }
                Ok(true)
            })();
            match result {
                Ok(true) => {}
                Ok(false) => log(&format!("<warning>Skipped branch {branch}, no composer file</warning>")),
                Err(e) if should_rethrow(&e) => return Err(e),
                Err(e) if error_status(&e).is_some() => log(&format!(
                    "<warning>Skipped branch {branch}, no composer file was found ({} HTTP status code)</warning>",
                    error_status(&e).unwrap_or(0)
                )),
                Err(e) => {
                    if !very_verbose {
                        io.write_error("");
                    }
                    io.error(&format!("Skipped branch {branch}, {e}"));
                    io.write_error("");
                }
            }
        }
        if repo.is_empty() {
            bail!("No valid composer.json was found in any branch or tag of {}, could not load a package from it.", sanitize(&self.url));
        }
        io.write_error_at(&format!("Loaded {} packages from {}", repo.len(), self.repo_name()), Verbosity::Debug);
        Ok(repo)
    }

    /// `preProcess()`: name, source and dist from the driver.
    fn pre_process(&self, driver: &Driver, mut data: Map<String, Value>, identifier: &str, package_name: Option<&str>) -> Map<String, Value> {
        // keep the name of the main identifier for all packages, so a rename applies to old tags too
        let name = package_name.map(Value::from).or_else(|| data.get("name").cloned()).unwrap_or(Value::Null);
        data.insert("name".into(), name);
        if data.get("dist").is_none_or(Value::is_null) {
            data.insert("dist".into(), driver.dist(identifier));
        }
        if data.get("source").is_none_or(Value::is_null) {
            data.insert("source".into(), driver.source(identifier));
        }
        // custom dist info without a reference gets the source reference
        let source_ref = data.get("source").and_then(|s| s.get("reference")).cloned();
        if let (Some(Value::Object(dist)), Some(reference)) = (data.get_mut("dist"), source_ref)
            && dist.get("reference").is_none_or(Value::is_null)
        {
            dist.insert("reference".into(), reference);
        }
        data
    }
}

/// `shouldRethrowTransportException()`.
fn should_rethrow(e: &anyhow::Error) -> bool {
    error_status(e).is_some_and(|s| matches!(s, 401 | 403 | 429) || s >= 500)
}

impl Repository for VcsRepository {
    fn repo_name(&self) -> String {
        let driver = if self.kind == "github" || (self.kind == "vcs" && github_supports(&self.http, &self.url)) { "github" } else { "git" };
        format!("vcs repo ({driver} {})", sanitize(&self.url))
    }

    fn load_packages(&self, req: &LoadRequest) -> Result<LoadResult> {
        self.repository()?.load_packages(req)
    }

    fn packages(&self) -> Result<Vec<PackageRef>> {
        self.repository()?.packages()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{commit_file, git, git_repo, test_config};
    use komposer_semver::Stability;
    use std::collections::{HashMap, HashSet};

    fn http() -> HttpClient {
        HttpClient::new(Arc::new(Io::null()), &test_config()).unwrap()
    }

    fn versions(repo: &VcsRepository) -> Vec<String> {
        let mut v: Vec<String> = repo.packages().unwrap().iter().map(|p| p.pretty_version.clone()).collect();
        v.sort();
        v
    }

    /// `VcsRepositoryTest::testLoadVersions()`.
    #[test]
    fn loads_versions_from_tags_and_branches() {
        let src = git_repo("vcs-src");
        let composer = |v: Option<&str>| match v {
            Some(v) => format!(r#"{{"name": "a/b", "version": "{v}"}}"#),
            None => r#"{"name": "a/b"}"#.to_string(),
        };
        commit_file(&src, "composer.json", &composer(None), "init");
        git(&src, &["tag", "0.6.0"]);
        commit_file(&src, "composer.json", &composer(Some("1.0.0")), "versioned");
        git(&src, &["tag", "1.0.0"]);
        git(&src, &["tag", "1.0.x"]); // invalid tag name
        commit_file(&src, "composer.json", &composer(Some("1.1.0")), "mismatch");
        git(&src, &["tag", "1.0.1"]); // version mismatch, skipped
        commit_file(&src, "composer.json", &composer(None), "unversioned");
        git(&src, &["tag", "release-1.2.0"]);
        git(&src, &["tag", "1.2.1-dev"]); // dev suffix, skipped
        git(&src, &["branch", "1.0"]);
        git(&src, &["branch", "feature/foo"]);
        git(&src, &["checkout", "-q", "-b", "empty"]);
        git(&src, &["rm", "-q", "composer.json"]);
        git(&src, &["commit", "-q", "-m", "no composer"]);
        git(&src, &["checkout", "-q", "main"]);

        let repo = VcsRepository::new(&json!({"type": "vcs", "url": src.display().to_string()}), http()).unwrap();
        assert_eq!(versions(&repo), ["0.6.0", "1.0.0", "1.0.x-dev", "1.2.0", "9999999-dev", "dev-feature/foo", "dev-main"]);
        let main = repo.packages().unwrap().into_iter().find(|p| p.pretty_version == "dev-main" && !p.is_alias()).unwrap();
        assert!(main.default_branch);
        assert_eq!(main.source.as_ref().unwrap().kind, "git");
        assert!(main.dist.is_none());
        assert!(main.time.as_deref().is_some_and(|t| t.ends_with("+00:00")));
        // default branch without branch-alias gets the 9999999-dev alias
        assert!(repo.packages().unwrap().iter().any(|p| p.is_alias() && p.version == DEFAULT_BRANCH_ALIAS));

        let names = vec![("a/b".to_string(), None)];
        let acceptable: HashSet<Stability> = [Stability::Stable].into();
        let req =
            LoadRequest { names: &names, acceptable_stabilities: &acceptable, stability_flags: &HashMap::new(), already_loaded: &HashMap::new() };
        let loaded = repo.load_packages(&req).unwrap();
        assert_eq!(loaded.names_found, ["a/b"]);
        assert_eq!(loaded.packages.len(), 3);
    }

    #[test]
    fn file_urls_are_local_and_composer_json_is_cached() {
        let src = git_repo("vcs-remote");
        let sha = commit_file(&src, "composer.json", r#"{"name": "a/remote", "extra": {"branch-alias": {"dev-main": "2.x-dev"}}}"#, "init");
        let http = http();
        let url = format!("file://{}/.git", src.display());
        let repo = VcsRepository::new(&json!({"type": "git", "url": url}), http.clone()).unwrap();
        let mut v = versions(&repo);
        v.sort();
        assert_eq!(v, ["2.x-dev", "dev-main"]);
        assert_eq!(repo.repo_name(), format!("vcs repo (git {url})"));
        // local paths are read in place, composer.json files are cached by commit
        let repo_dir = http.config().get_string("cache-repo-dir").unwrap();
        assert!(PathBuf::from(repo_dir).join(cache_dir_name(&src.display().to_string())).join(&sha).exists());
        assert!(!PathBuf::from(http.config().get_string("cache-vcs-dir").unwrap()).exists());
    }

    #[test]
    fn git_driver_rejects_option_like_identifiers() {
        let src = git_repo("vcs-ident");
        commit_file(&src, "composer.json", "{}", "init");
        let driver = Driver::new(&http(), &src.display().to_string(), "git", &json!({})).unwrap();
        assert!(driver.branches.iter().all(|(b, _)| !b.starts_with('-')));
        assert_eq!(driver.root_identifier, "main");
        assert!(driver.file_content("composer.json", "-n").unwrap_err().to_string().contains("must not start with a -"));
        assert!(driver.change_date("-n").is_err());
    }

    #[test]
    fn github_urls_get_zipball_dists() {
        let gh = GitHub {
            origin: "github.com".into(),
            owner: "acme".into(),
            repo: "lib".into(),
            api: false,
            private: false,
            has_issues: false,
            archived: false,
            funding: None,
        };
        assert_eq!(gh.ssh_url(), "git@github.com:acme/lib.git");
        assert_eq!(gh.https_url(), "https://github.com/acme/lib.git");
        assert!(github_supports(&http(), "https://github.com/acme/lib.git"));
        assert!(github_supports(&http(), "git@github.com:acme/lib.git"));
        assert!(!github_supports(&http(), "https://gitlab.com/acme/lib.git"));
        let src = git_repo("vcs-gh");
        commit_file(&src, "composer.json", "{}", "init");
        let mut driver = Driver::init_git(&http(), &src.display().to_string(), Some(gh)).unwrap();
        driver.github.as_mut().unwrap().owner = "acme".into();
        assert_eq!(driver.dist("abc")["url"], "https://api.github.com/repos/acme/lib/zipball/abc");
    }

    #[test]
    fn funding_yml() {
        let yml = "github: [foo, 'bar']\npatreon: baz # comment\ncustom: example.com\nko_fi:\n  - https://ko-fi.com/qux\ncustom: https://example.org/donate\ncustom: not a url\n";
        let parsed = parse_funding(yml, &Io::null());
        let urls: Vec<&str> = parsed.iter().map(|f| f["url"].as_str().unwrap()).collect();
        assert_eq!(
            urls,
            ["https://github.com/foo", "https://github.com/bar", "https://www.patreon.com/baz", "https://example.com", "https://example.org/donate"]
        );
    }

    #[test]
    fn unsupported_types() {
        assert!(VcsRepository::new(&json!({"type": "svn", "url": "https://svn.example.org"}), http()).is_err());
    }
}
