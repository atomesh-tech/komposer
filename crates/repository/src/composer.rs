//! `ComposerRepository`: a `packages.json` repository such as Packagist, with Composer 2
//! `metadata-url` lazy loading, inline packages, `includes`, security advisories and filter lists.
//! Also `MetadataMinifier::expand()`, `PartialSecurityAdvisory::create()` and
//! `ComposerRepositoryFilterInformation`.

use crate::cache::Cache;
use crate::http::{HttpClient, RequestOptions, Response, error_status, output_warnings};
use crate::url::sanitize;
use crate::util::{http_build_query, package_name_regex, urlencode};
use anyhow::{Context, Result, anyhow, bail};
use komposer_package::io::Io;
use komposer_package::json::{self, Flags};
use komposer_package::loader::{self, LoadOptions, php_empty, php_truthy};
use komposer_package::repository::{
    AdvisoryResult, ArrayRepository, FilterEntry, LoadRequest, LoadResult, RepoKind, Repository, SearchMode, SecurityAdvisory, array_search,
    is_package_acceptable,
};
use komposer_package::{PackageRef, ROOT_NAME, is_platform_package};
use komposer_semver::{Constraint, DEFAULT_BRANCH_ALIAS, Op, Stability};
use regex::Regex;
use serde_json::{Map, Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock, Mutex};

/// Package names per security-advisories API request (PHP's `max_input_vars` would truncate more).
const ADVISORY_API_BATCH_SIZE: usize = 500;
const PACKAGES_JSON: &str = "packages.json";

/// `MetadataMinifier::expand()`: undoes `composer/2.0` minification of a version list.
pub fn expand(versions: &[Value]) -> Vec<Value> {
    let mut expanded = Vec::with_capacity(versions.len());
    let _ = expand_each(versions.iter(), |v| {
        expanded.push(Value::Object(v.clone()));
        Ok(())
    });
    expanded
}

/// `expand()` without materializing every version: `visit` sees each expanded version in turn, so
/// callers only copy the ones they keep (PHP gets this for free from copy-on-write arrays).
fn expand_each<'a>(versions: impl Iterator<Item = &'a Value>, mut visit: impl FnMut(&Map<String, Value>) -> Result<()>) -> Result<()> {
    let empty = Map::new();
    let mut current: Map<String, Value> = Map::new();
    for version in versions {
        let data = version.as_object().unwrap_or(&empty);
        if current.is_empty() {
            current = data.clone();
        } else {
            for (key, val) in data {
                if val == "__unset" {
                    current.shift_remove(key);
                } else {
                    current.insert(key.clone(), val.clone());
                }
            }
        }
        visit(&current)?;
    }
    Ok(())
}

/// `PartialSecurityAdvisory::create()`: full advisories carry title, sources and reportedAt.
pub fn create_advisory(package: &str, data: &Value) -> SecurityAdvisory {
    static ESSENTIAL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(^[>=<^~]*[\d.]+).*").unwrap());
    let s = |k: &str| data.get(k).and_then(Value::as_str).map(String::from);
    let pretty = s("affectedVersions").unwrap_or_default();
    // keep the essential part of invalid constraints like <=3.20-test2
    let constraint = komposer_semver::parse_constraints(&pretty)
        .or_else(|_| komposer_semver::parse_constraints(&ESSENTIAL.replace(&pretty, "$1")))
        .unwrap_or_else(|_| Constraint::single(Op::Eq, "0.0.0-invalid-version"));
    let full = ["title", "sources", "reportedAt"].iter().all(|k| data.get(*k).is_some_and(|v| !v.is_null()));
    SecurityAdvisory {
        package_name: package.into(),
        advisory_id: data.get("advisoryId").map(scalar).unwrap_or_default(),
        affected_versions: constraint,
        affected_versions_pretty: pretty,
        title: if full { s("title") } else { None },
        cve: s("cve"),
        link: s("link"),
        severity: s("severity"),
        reported_at: s("reportedAt").filter(|_| full).map(|r| loader::format_date(&r).unwrap_or(r)),
        sources: data.get("sources").and_then(Value::as_array).cloned().unwrap_or_default(),
    }
}

fn scalar(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        v => v.to_string(),
    }
}

/// `ComposerRepositoryFilterInformation`.
#[derive(Clone, Debug, Default)]
struct FilterInfo {
    metadata: bool,
    lists: Vec<String>,
    summary_url: Option<String>,
    api_url: Option<String>,
}

/// What `loadRootServerFile()` learnt from packages.json.
#[derive(Default)]
struct Root {
    data: Value,
    notify_url: Option<String>,
    search_url: Option<String>,
    list_url: Option<String>,
    providers_api_url: Option<String>,
    /// `metadata-url`
    lazy_url: Option<String>,
    /// inline packages of a v2 repository, by name
    partial: Option<HashMap<String, Vec<Value>>>,
    available: Option<HashSet<String>>,
    patterns: Option<Vec<Regex>>,
    /// (`metadata`, `api-url`)
    advisories: Option<(bool, Option<String>)>,
    filter: Option<FilterInfo>,
    source_mirrors: HashMap<String, Vec<Value>>,
    dist_mirrors: Option<Vec<Value>>,
}

impl Root {
    fn has_available_list(&self) -> bool {
        self.available.is_some() || self.patterns.is_some()
    }

    /// `lazyProvidersRepoContains()`.
    fn contains(&self, name: &str) -> bool {
        self.available.as_ref().is_some_and(|a| a.contains(name)) || self.patterns.iter().flatten().any(|p| p.is_match(name))
    }
}

#[derive(Default)]
struct State {
    root: Option<Arc<Root>>,
    /// all packages of a repository without `metadata-url` (`ArrayRepository::initialize()`)
    all: Option<Arc<ArrayRepository>>,
    /// packages loaded lazily so far
    loaded: Vec<PackageRef>,
    fresh_metadata_urls: HashSet<String>,
    /// file name => `security-advisories`/`filter` of p2 files loaded so far (`None`: nothing relevant)
    sections: HashMap<String, Option<Value>>,
    /// file name => p2 files fetched ahead of `loadPackages()` by [`Repository::prefetch`]
    prefetched: HashMap<String, (Option<Value>, String)>,
    packages_not_found: HashSet<String>,
    degraded_mode: bool,
    warned_index_mismatch: bool,
}

/// Outcome of `asyncFetchFile()`.
enum Fetched {
    Data(Value),
    /// 304, the cached copy is current
    Fresh,
    /// 404 (or `false`): no such file
    Missing,
}

pub struct ComposerRepository {
    http: HttpClient,
    io: Arc<Io>,
    url: String,
    base_url: String,
    repo_config: Value,
    raw_options: Value,
    options: RequestOptions,
    cache: Cache,
    /// `None`: `filter: false`; otherwise lists the user opted out of
    user_filter: Option<HashSet<String>>,
    state: Mutex<State>,
}

impl ComposerRepository {
    /// `new ComposerRepository($repoConfig, ...)`.
    pub fn new(repo_config: &Value, http: HttpClient) -> Result<ComposerRepository> {
        static HAS_SCHEME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[\w.]+\??://").unwrap());
        static PACKAGIST: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^(https?)://packagist\.org/?$").unwrap());
        static JSON_FILE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?:/[^/\\]+\.json)?(?:[?#].*)?$").unwrap());
        let mut url = repo_config.get("url").and_then(Value::as_str).unwrap_or("").to_string();
        if !HAS_SCHEME.is_match(&url) {
            url = match std::fs::canonicalize(&url) {
                Ok(path) if !url.is_empty() => format!("file://{}", path.display()),
                _ => format!("http://{url}"),
            };
        }
        let mut url = url.trim_end_matches('/').to_string();
        if url.is_empty() {
            bail!("The repository url must not be an empty string");
        }
        if let Some(rest) = url.strip_prefix("https?") {
            url = format!("https{rest}");
        }
        if crate::url::parse(&url.replace('\\', "/")).scheme.is_none_or(str::is_empty) {
            bail!("Invalid url given for Composer repository: {}", sanitize(&url));
        }
        if let Some(c) = PACKAGIST.captures(&url) {
            url = format!("{}://repo.packagist.org", &c[1]);
        }
        let base_url = JSON_FILE.replace(&url, "").trim_end_matches('/').to_string();
        let config = http.config();
        let cache_dir = format!("{}/{}", config.get_string("cache-repo-dir").unwrap_or_default(), cache_dir_name(&url));
        let cache = Cache::new(http.io().clone(), cache_dir, "a-z0-9.$~_").read_only(config.get_bool("cache-read-only"));
        let raw_options = repo_config.get("options").cloned().unwrap_or(json!([]));
        Ok(ComposerRepository {
            io: http.io().clone(),
            options: RequestOptions::from_context(&raw_options),
            raw_options,
            user_filter: parse_user_filter(repo_config.get("filter"))?,
            http,
            url,
            base_url,
            repo_config: repo_config.clone(),
            cache,
            state: Mutex::default(),
        })
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn repo_config(&self) -> &Value {
        &self.repo_config
    }

    /// The `notify-batch` URL, if the repository wants install notifications.
    pub fn notify_url(&self) -> Result<Option<String>> {
        Ok(self.root()?.notify_url.clone())
    }

    fn root(&self) -> Result<Arc<Root>> {
        self.root_with_max_age(None)
    }

    fn root_with_max_age(&self, max_age: Option<u64>) -> Result<Arc<Root>> {
        let mut st = self.state.lock().unwrap();
        self.load_root(&mut st, max_age)
    }

    fn packages_json_url(&self) -> String {
        if crate::url::parse(&self.url.replace('\\', "/")).path.contains(".json") { self.url.clone() } else { format!("{}/packages.json", self.url) }
    }

    /// `canonicalizeUrl()`: resolves root-relative URLs against the repository URL.
    fn canonicalize_url(&self, url: &str) -> String {
        static ORIGIN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[^:]+://[^/]*").unwrap());
        if url.starts_with('/') {
            return match ORIGIN.find(&self.url) {
                Some(m) => format!("{}{url}", m.as_str()),
                None => self.url.clone(),
            };
        }
        url.to_string()
    }

    /// `loadRootServerFile()`.
    fn load_root(&self, st: &mut State, max_age: Option<u64>) -> Result<Arc<Root>> {
        if let Some(root) = &st.root {
            return Ok(root.clone());
        }
        let url = self.packages_json_url();
        let mut data = None;
        if let Some(cached) = self.cache.read(PACKAGES_JSON).filter(|c| !c.is_empty()).and_then(|c| json::parse(&c).ok()) {
            if max_age.is_some_and(|max| self.cache.age(PACKAGES_JSON).is_some_and(|age| age <= max)) {
                data = Some(cached);
            } else if let Some(last_modified) = cached.get("last-modified").and_then(Value::as_str).map(String::from) {
                data = Some(self.fetch_file_if_last_modified(st, &url, PACKAGES_JSON, &last_modified)?.unwrap_or(cached));
            }
        }
        let data = match data {
            Some(d) => d,
            None => self.fetch_file(st, &url, PACKAGES_JSON, true)?,
        };
        let root = Arc::new(self.parse_root(st, data)?);
        st.root = Some(root.clone());
        Ok(root)
    }

    fn parse_root(&self, st: &mut State, data: Value) -> Result<Root> {
        let field = |k: &str| data.get(k).filter(|v| !php_empty(v));
        let url_field = |k: &str| field(k).and_then(Value::as_str).map(|u| self.canonicalize_url(u));
        let mut root = Root {
            notify_url: url_field("notify-batch").or_else(|| url_field("notify")),
            search_url: url_field("search"),
            list_url: url_field("list"),
            providers_api_url: url_field("providers-api"),
            ..Default::default()
        };
        for mirror in field("mirrors").and_then(Value::as_array).into_iter().flatten() {
            let preferred = mirror.get("preferred").is_some_and(php_truthy);
            for kind in ["git", "hg"] {
                if let Some(u) = mirror.get(format!("{kind}-url")).filter(|v| !php_empty(v)) {
                    root.source_mirrors.entry(kind.into()).or_default().push(json!({"url": u, "preferred": preferred}));
                }
            }
            if let Some(u) = mirror.get("dist-url").and_then(Value::as_str).filter(|u| !u.is_empty()) {
                root.dist_mirrors.get_or_insert_default().push(json!({"url": self.canonicalize_url(u), "preferred": preferred}));
            }
        }

        // metadata-url indicates the v2 protocol, which takes over from all the v1 types
        if let Some(lazy) = url_field("metadata-url") {
            root.lazy_url = Some(lazy);
            if let Some(names) = field("available-packages").and_then(Value::as_array) {
                root.available = Some(names.iter().filter_map(Value::as_str).map(str::to_lowercase).collect());
            }
            if let Some(patterns) = field("available-package-patterns").and_then(Value::as_array) {
                root.patterns = Some(patterns.iter().filter_map(Value::as_str).map(package_name_regex).collect());
            }
            if let Some(adv) = data.get("security-advisories").filter(|v| v.is_object() || v.is_array()) {
                let api_url = adv.get("api-url").and_then(Value::as_str).map(|u| self.canonicalize_url(u));
                if api_url.is_none() && !root.has_available_list() {
                    bail!(
                        "Invalid security advisory configuration on {}: If the repository does not provide a security-advisories.api-url then available-packages or available-package-patterns are required to be provided for performance reason.",
                        self.repo_name()
                    );
                }
                root.advisories = Some((adv.get("metadata").is_some_and(php_truthy), api_url));
            }
            if let Some(filter) = data.get("filter").filter(|v| v.is_object() || v.is_array()) {
                root.filter = Some(self.filter_info(filter));
            }
            if let Some(packages) = field("packages").filter(|p| p.is_object() || p.is_array()) {
                let mut partial: HashMap<String, Vec<Value>> = HashMap::new();
                for (key, versions) in entries(packages) {
                    for version in values(versions) {
                        let name = version.get("name").and_then(Value::as_str).unwrap_or("").to_lowercase();
                        self.warn_index_mismatch(st, &key, &name, version);
                        partial.entry(name).or_default().push(version.clone());
                    }
                }
                root.partial = Some(partial);
            }
        } else if ["providers-url", "providers", "provider-includes", "providers-includes", "providers-lazy-url"].iter().any(|k| field(k).is_some()) {
            bail!(
                "{} only serves Composer 1 provider metadata, which komposer does not support. The repository needs to provide a metadata-url (Composer 2 metadata).",
                self.repo_name()
            );
        }
        root.data = data;
        Ok(root)
    }

    fn warn_index_mismatch(&self, st: &mut State, key: &str, name: &str, version: &Value) {
        if !st.warned_index_mismatch && key.to_lowercase() != name {
            st.warned_index_mismatch = true;
            let meta_name = version.get("name").and_then(Value::as_str).unwrap_or("");
            self.io.warning(&format!(
                "Warning: the packages key '{key}' doesn't match the name defined in the package metadata '{meta_name}' in repository {}",
                self.base_url
            ));
        }
    }

    /// `ComposerRepositoryFilterInformation::fromData()`.
    fn filter_info(&self, data: &Value) -> FilterInfo {
        const RESERVED: [&str; 2] = ["advisories", "abandoned"];
        const FUTURE_RESERVED: [&str; 10] =
            ["package", "packages", "license", "licence", "licenses", "licences", "support", "maintenance", "security", "minimum-release-age"];
        let lists = match data.get("lists") {
            Some(Value::Object(lists)) => lists
                .iter()
                .filter(|(name, cfg)| cfg.get("enabled").is_some_and(php_truthy) && !RESERVED.contains(&name.as_str()))
                .filter(|(name, _)| !FUTURE_RESERVED.contains(&name.as_str()) && !name.starts_with("ignore"))
                .map(|(name, _)| name.clone())
                .collect(),
            _ => vec![],
        };
        let url = |k: &str| data.get(k).and_then(Value::as_str).filter(|u| !u.is_empty()).map(|u| self.canonicalize_url(u));
        FilterInfo { metadata: data.get("metadata").is_some_and(php_truthy), lists, summary_url: url("summary-url"), api_url: url("api-url") }
    }

    /// `fetchFile()`: downloads and caches a file, falling back to the cache when the network fails.
    fn fetch_file(&self, st: &mut State, url: &str, cache_key: &str, store_last_modified: bool) -> Result<Value> {
        // url-encode $ signs in URLs as bad proxies choke on them
        let mut url = url.to_string();
        if let Some(pos) = url.find('$').filter(|p| *p > 0)
            && (url.starts_with("http://") || url.starts_with("https://"))
        {
            url.replace_range(pos..=pos, "%24");
        }
        let fetched =
            self.http.get(&url, &self.options).and_then(|r| self.store_response(&r, cache_key, store_last_modified.then_some(Flags::PLAIN)));
        match fetched {
            Ok(data) => Ok(data),
            Err(e) if error_status(&e) == Some(404) => Err(e),
            Err(e) => match self.cache.read(cache_key).filter(|c| !c.is_empty()) {
                Some(contents) => {
                    self.warn_degraded(st, &e);
                    json::parse(&contents).with_context(|| format!("\"{}\" does not contain valid JSON", self.cache.path(cache_key).display()))
                }
                None => Err(e),
            },
        }
    }

    /// `fetchFileIfLastModified()`: `None` when the cached copy is still current.
    fn fetch_file_if_last_modified(&self, st: &mut State, url: &str, cache_key: &str, last_modified: &str) -> Result<Option<Value>> {
        let opts = self.options.clone().header(format!("If-Modified-Since: {last_modified}"));
        let fetched = self.http.get(url, &opts).and_then(|response| {
            if response.body.is_empty() && response.status == 304 {
                return Ok(None);
            }
            Ok(Some(self.store_response(&response, cache_key, Some(Flags::PLAIN))?))
        });
        match fetched {
            Ok(data) => Ok(data),
            Err(e) if error_status(&e) == Some(404) => Err(e),
            Err(e) => {
                self.warn_degraded(st, &e);
                Ok(None)
            }
        }
    }

    /// Decodes a fresh response and caches it; with `flags`, stored with its `last-modified` date.
    fn store_response(&self, response: &Response, cache_key: &str, flags: Option<Flags>) -> Result<Value> {
        let mut data = response.json()?;
        output_warnings(&self.io, &self.url, &data);
        let mut contents = response.body.clone();
        if let (Some(flags), Some(last_modified)) = (flags, response.header("last-modified")) {
            contents = match with_last_modified(&contents, &data, last_modified) {
                Some(c) => c,
                None => {
                    set_key(&mut data, "last-modified", last_modified.into());
                    json::encode(&data, flags).into_bytes()
                }
            };
            set_key(&mut data, "last-modified", last_modified.into());
        }
        if !self.cache.is_read_only() {
            self.cache.write(cache_key, &contents);
        }
        Ok(data)
    }

    fn warn_degraded(&self, st: &mut State, e: &anyhow::Error) {
        if !st.degraded_mode {
            self.io.warning(&format!(
                "{} could not be fully loaded ({e}), package information was loaded from the local cache and may be out of date",
                sanitize(&self.url)
            ));
        }
        st.degraded_mode = true;
    }

    /// Several `asyncFetchFile()` calls, downloaded concurrently. Requests are `(url, cache key,
    /// last-modified of the cached copy)`.
    fn async_fetch_files(&self, st: &mut State, requests: &[(String, String, Option<String>)]) -> Result<Vec<Fetched>> {
        let mut out: Vec<Option<Fetched>> = Vec::with_capacity(requests.len());
        let mut pending = vec![];
        for (i, (url, _, last_modified)) in requests.iter().enumerate() {
            if st.packages_not_found.contains(url) {
                out.push(Some(Fetched::Missing));
            } else if st.fresh_metadata_urls.contains(url) && last_modified.is_some() {
                // make it look like we got a 304 response
                out.push(Some(Fetched::Fresh));
            } else {
                let mut opts = self.options.clone();
                if let Some(lm) = last_modified {
                    opts.headers.push(format!("If-Modified-Since: {lm}"));
                }
                out.push(None);
                pending.push((i, url.clone(), opts));
            }
        }
        let indexes: Vec<usize> = pending.iter().map(|(i, _, _)| *i).collect();
        let responses = self.http.get_many(pending.into_iter().map(|(_, url, opts)| (url, opts)).collect());
        // decoding and caching the new files is the heavy part: do it on all cores
        let flags = Flags { pretty: false, unescaped_slashes: true, unescaped_unicode: true };
        let jobs: Vec<(usize, &Response)> = indexes.iter().zip(&responses).filter_map(|(i, r)| Some((*i, r.as_ref().ok()?))).collect();
        let mut stored: HashMap<usize, Result<Value>> = par_map(jobs, |(i, response)| {
            let fresh = response.status == 404 || (response.body.is_empty() && response.status == 304);
            (!fresh).then(|| (i, self.store_response(response, &requests[i].1, Some(flags))))
        })
        .into_iter()
        .flatten()
        .collect();
        for (i, response) in indexes.into_iter().zip(responses) {
            let (url, _, last_modified) = &requests[i];
            let fetched = match response {
                Ok(response) if response.status == 404 => {
                    // package not found is acceptable for a v2 protocol repository
                    st.packages_not_found.insert(url.clone());
                    Fetched::Missing
                }
                Ok(response) if response.body.is_empty() && response.status == 304 => {
                    st.fresh_metadata_urls.insert(url.clone());
                    Fetched::Fresh
                }
                Ok(_) => {
                    let data = stored.remove(&i).expect("decoded above")?;
                    st.fresh_metadata_urls.insert(url.clone());
                    Fetched::Data(data)
                }
                Err(e) if error_status(&e) == Some(404) => {
                    st.packages_not_found.insert(url.clone());
                    Fetched::Missing
                }
                Err(e) => {
                    self.warn_degraded(st, &e);
                    // with a cached copy, pretend we got a 304 so the process can continue
                    if last_modified.is_some() {
                        st.fresh_metadata_urls.insert(url.clone());
                        Fetched::Fresh
                    } else if error_status(&e) == Some(499) {
                        st.packages_not_found.insert(url.clone());
                        Fetched::Missing
                    } else {
                        return Err(e);
                    }
                }
            };
            out[i] = Some(fetched);
        }
        Ok(out.into_iter().map(|f| f.unwrap_or(Fetched::Missing)).collect())
    }

    /// Several `startCachedAsyncDownload($fileName, $packageName)`: the p2 file of each name,
    /// `None` where it has nothing for the package.
    fn cached_async_downloads(&self, st: &mut State, root: &Root, files: &[(String, String)]) -> Result<Vec<(Option<Value>, String)>> {
        let lazy_url = root.lazy_url.as_deref().context("startCachedAsyncDownload only supports v2 protocol composer repos with a metadata-url")?;
        let cache_keys: Vec<String> = files.iter().map(|(file, _)| format!("provider-{}.json", file.to_lowercase().replace('/', "~"))).collect();
        let cached: Vec<Option<String>> = par_map(cache_keys.iter().collect(), |key| self.cache.read(key).filter(|c| !c.is_empty()));
        let requests: Vec<(String, String, Option<String>)> = files
            .iter()
            .zip(cache_keys)
            .zip(&cached)
            .map(|(((file, _), cache_key), contents)| {
                let url = lazy_url.replace("%package%", &file.to_lowercase());
                (url, cache_key, contents.as_deref().and_then(last_modified_of).map(String::from))
            })
            .collect();
        let mut fetched = self.async_fetch_files(st, &requests)?;

        // a cached copy that turns out to be unreadable is downloaded again, unconditionally
        let fresh: Vec<(usize, &str)> =
            fetched.iter().enumerate().filter(|(_, f)| matches!(f, Fetched::Fresh)).filter_map(|(i, _)| Some((i, cached[i].as_deref()?))).collect();
        let mut parsed: HashMap<usize, Value> = par_map(fresh, |(i, text)| json::parse(text).ok().map(|v| (i, v))).into_iter().flatten().collect();
        let broken: Vec<usize> =
            fetched.iter().enumerate().filter(|(i, f)| matches!(f, Fetched::Fresh) && !parsed.contains_key(i)).map(|(i, _)| i).collect();
        if !broken.is_empty() {
            let retry: Vec<(String, String, Option<String>)> = broken.iter().map(|&i| (requests[i].0.clone(), requests[i].1.clone(), None)).collect();
            for (i, f) in broken.into_iter().zip(self.async_fetch_files(st, &retry)?) {
                fetched[i] = f;
            }
        }

        let mut out = vec![];
        for (i, (fetched, (file, package))) in fetched.into_iter().zip(files).enumerate() {
            let (url, cache_key, _) = &requests[i];
            let (data, source) = match fetched {
                Fetched::Fresh => (parsed.remove(&i), format!("cached file ({cache_key} originating from {})", sanitize(url))),
                Fetched::Data(d) => (Some(d), format!("downloaded file ({})", sanitize(url))),
                Fetched::Missing => (None, format!("downloaded file ({})", sanitize(url))),
            };
            let relevant = data.filter(|d| {
                d.get("packages").and_then(|p| p.get(package.as_str())).is_some_and(|v| !v.is_null())
                    || d.get("security-advisories").is_some_and(|v| !v.is_null())
                    || d.get("filter").is_some_and(|v| !v.is_null())
            });
            st.sections.insert(file.to_lowercase(), relevant.as_ref().map(policy_sections));
            out.push((relevant, source));
        }
        Ok(out)
    }

    /// The `security-advisories` and `filter` parts of p2 files, kept after their first load so the
    /// pool filters and the audit do not read and decode every file again.
    fn cached_sections(&self, st: &mut State, root: &Root, files: &[(String, String)]) -> Result<Vec<Option<Value>>> {
        let missing: Vec<(String, String)> = files.iter().filter(|(f, _)| !st.sections.contains_key(&f.to_lowercase())).cloned().collect();
        if !missing.is_empty() {
            self.cached_async_downloads(st, root, &missing)?;
        }
        Ok(files.iter().map(|(f, _)| st.sections.get(&f.to_lowercase()).cloned().flatten()).collect())
    }

    /// `loadAsyncPackages()`: fetches the p2 files (and `~dev` files) of all names at once.
    fn load_async(
        &self,
        st: &mut State,
        root: &Root,
        names: &[(String, Option<Constraint>)],
        stability: Option<(&HashSet<Stability>, &HashMap<String, Stability>)>,
        already_loaded: &HashMap<String, HashSet<String>>,
    ) -> Result<(Vec<String>, Vec<PackageRef>)> {
        let mut files: Vec<(String, String, Option<Constraint>)> = vec![];
        let only_dev = stability.is_some_and(|(a, f)| a.len() == 1 && a.contains(&Stability::Dev) && f.is_empty());
        let mut dev_files = vec![];
        for (name, constraint) in names {
            let name = name.to_lowercase();
            if is_platform_package(&name) || name == ROOT_NAME {
                continue;
            }
            // if only dev stability is requested, the non-dev file is skipped
            if !only_dev {
                files.push((name.clone(), name.clone(), constraint.clone()));
            }
            if stability.is_none_or(|(a, f)| is_package_acceptable(a, f, std::slice::from_ref(&name), Stability::Dev)) {
                dev_files.push((format!("{name}~dev"), name, constraint.clone()));
            }
        }
        files.extend(dev_files);

        // files prefetched earlier are used once; the rest is fetched now
        let mut downloads: Vec<Option<(Option<Value>, String)>> = files.iter().map(|(f, _, _)| st.prefetched.remove(f)).collect();
        let missing: Vec<(String, String)> =
            files.iter().zip(&downloads).filter(|(_, d)| d.is_none()).map(|((f, n, _), _)| (f.clone(), n.clone())).collect();
        if !missing.is_empty() {
            let mut fetched = self.cached_async_downloads(st, root, &missing)?.into_iter();
            for d in downloads.iter_mut().filter(|d| d.is_none()) {
                *d = fetched.next();
            }
        }
        // expanding and loading the versions is independent per file: do it on all cores
        let jobs: Vec<_> = files.iter().zip(downloads.into_iter().map(|d| d.expect("filled above"))).collect();
        let loaded = par_map(jobs, |((_, real_name, constraint), (response, source))| -> Result<Option<Vec<PackageRef>>> {
            let Some(versions) = response.as_ref().and_then(|r| r.get("packages")).and_then(|p| p.get(real_name.as_str())) else {
                return Ok(None);
            };
            let mut to_load = vec![];
            let mut consider = |data: &Map<String, Value>| -> Result<()> {
                let normalized = normalized_version(data)?;
                if already_loaded.get(real_name).is_some_and(|v| v.contains(&normalized))
                    || !is_version_acceptable(constraint.as_ref(), real_name, data, &normalized, stability)
                {
                    return Ok(());
                }
                let mut version = data.clone();
                version.insert("version_normalized".into(), normalized.into());
                to_load.push(Value::Object(version));
                Ok(())
            };
            if response.as_ref().and_then(|r| r.get("minified")).and_then(Value::as_str) == Some("composer/2.0") {
                expand_each(values(versions), consider)?;
            } else {
                for version in values(versions).filter_map(Value::as_object) {
                    consider(version)?;
                }
            }
            self.create_packages(root, to_load, &source).map(Some)
        });
        let mut names_found: Vec<String> = vec![];
        let mut packages: Vec<PackageRef> = vec![];
        let mut seen = HashSet::new();
        for ((_, real_name, _), loaded) in files.iter().zip(loaded) {
            let Some(loaded) = loaded? else { continue };
            if !names_found.contains(real_name) {
                names_found.push(real_name.clone());
            }
            for package in &loaded {
                for p in std::iter::once(package).chain(&package.alias_of) {
                    if seen.insert(Arc::as_ptr(p)) {
                        packages.push(p.clone());
                    }
                }
            }
        }
        // all of them were just created, so none is in there yet
        st.loaded.extend(packages.iter().cloned());
        Ok((names_found, packages))
    }

    /// `whatProvides()` for packages inlined in packages.json next to a `metadata-url`.
    fn what_provides_partial(
        &self,
        root: &Root,
        name: &str,
        stability: Option<(&HashSet<Stability>, &HashMap<String, Stability>)>,
        already_loaded: &HashMap<String, HashSet<String>>,
    ) -> Result<Vec<PackageRef>> {
        let mut uids = HashSet::new();
        let mut to_load = vec![];
        for (i, version) in root.partial.as_ref().and_then(|p| p.get(name)).into_iter().flatten().enumerate() {
            if version.get("name").and_then(Value::as_str).map(str::to_lowercase).as_deref() != Some(name) {
                continue;
            }
            let uid = version.get("uid").map(scalar).unwrap_or_else(|| i.to_string());
            if !uids.insert(uid) {
                continue;
            }
            let Some(data) = version.as_object() else { continue };
            let normalized = normalized_version(data)?;
            if already_loaded.get(name).is_some_and(|v| v.contains(&normalized)) {
                continue;
            }
            if is_version_acceptable(None, name, data, &normalized, stability) {
                let mut data = data.clone();
                data.insert("version_normalized".into(), normalized.into());
                to_load.push(Value::Object(data));
            }
        }
        let source = format!("root file ({})", sanitize(&self.packages_json_url()));
        self.create_packages(root, to_load, &source)
    }

    /// `createPackages()`: loads version data, adding the notify URL, mirrors and transport options.
    fn create_packages(&self, root: &Root, versions: Vec<Value>, source: &str) -> Result<Vec<PackageRef>> {
        let mut out = vec![];
        for mut version in versions {
            let Some(data) = version.as_object_mut() else { continue };
            if data.get("notification-url").is_none_or(Value::is_null)
                && let Some(notify) = &root.notify_url
            {
                data.insert("notification-url".into(), notify.clone().into());
            }
            for (key, mirrors) in [("source", None), ("dist", root.dist_mirrors.as_ref())] {
                let Some(Value::Object(loc)) = data.get_mut(key) else { continue };
                let mirrors = match key {
                    "source" => loc.get("type").and_then(Value::as_str).and_then(|t| root.source_mirrors.get(t)),
                    _ => mirrors,
                };
                if let Some(m) = mirrors {
                    loc.insert("mirrors".into(), Value::Array(m.clone()));
                }
            }
            // `configurePackageTransportOptions()`: only dists served by this repository get its options
            data.remove("transport-options");
            let dist_urls: Vec<&str> = data.get("dist").and_then(|d| d.get("url")).and_then(Value::as_str).into_iter().collect();
            if dist_urls.iter().any(|u| u.starts_with(&self.base_url))
                || root.dist_mirrors.iter().flatten().any(|m| m["url"].as_str().is_some_and(|u| u.starts_with(&self.base_url)))
            {
                data.insert("transport-options".into(), self.raw_options.clone());
            }
            let opts = LoadOptions { transport_options: true, skip_self_links: true };
            let loaded = loader::load(data, opts)
                .map_err(|e| anyhow!("Could not load packages in {} from {source}: [UnexpectedValueException] {e}", self.repo_name()))?;
            out.extend(loaded);
        }
        Ok(out)
    }

    /// `loadDataFromServer()` + `loadIncludes()`: every package of a non-lazy repository.
    fn all_packages(&self, st: &mut State, root: &Root) -> Result<Arc<ArrayRepository>> {
        if let Some(all) = &st.all {
            return Ok(all.clone());
        }
        let versions = self.load_includes(st, &root.data)?;
        let source = format!("root file ({})", sanitize(&self.packages_json_url()));
        let mut repo = ArrayRepository::new(self.repo_name(), RepoKind::Remote);
        for package in self.create_packages(root, versions, &source)? {
            repo.add(package);
        }
        let repo = Arc::new(repo);
        st.all = Some(repo.clone());
        Ok(repo)
    }

    fn load_includes(&self, st: &mut State, data: &Value) -> Result<Vec<Value>> {
        let mut packages = vec![];
        // legacy repository format
        if data.get("packages").is_none() && data.get("includes").is_none() {
            for (_, package) in entries(data) {
                if let Some(versions) = package.get("versions").filter(|v| v.is_object() || v.is_array()) {
                    packages.extend(values(versions).cloned());
                }
            }
            return Ok(packages);
        }
        if let Some(by_name) = data.get("packages") {
            for (key, versions) in entries(by_name) {
                for metadata in values(versions) {
                    let name = metadata.get("name").and_then(Value::as_str).unwrap_or("").to_lowercase();
                    self.warn_index_mismatch(st, &key, &name, metadata);
                    packages.push(metadata.clone());
                }
            }
        }
        if let Some(includes) = data.get("includes") {
            for (include, metadata) in entries(includes) {
                let cached = metadata
                    .get("sha1")
                    .and_then(Value::as_str)
                    .filter(|sha1| self.cache.sha1(&include).as_deref() == Some(*sha1))
                    .and_then(|_| self.cache.read(&include))
                    .and_then(|c| json::parse(&c).ok());
                let included = match cached {
                    Some(c) => c,
                    None => self.fetch_file(st, &format!("{}/{include}", self.base_url), &include, false)?,
                };
                packages.extend(self.load_includes(st, &included)?);
            }
        }
        Ok(packages)
    }

    /// `search()`.
    fn search_impl(&self, query: &str, mode: SearchMode, kind: Option<&str>) -> Result<Vec<Value>> {
        static VENDOR_QUERY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^\^(([a-z0-9_.-]+)/[a-z0-9_.-]*)\*?$").unwrap());
        let root = self.root_with_max_age(Some(600))?;
        if let (Some(search_url), SearchMode::Fulltext) = (&root.search_url, mode) {
            let url = search_url.replace("%query%", &urlencode(query)).replace("%type%", kind.unwrap_or(""));
            let search = self.http.get(&url, &self.options)?.json()?;
            output_warnings(&self.io, &self.url, &search);
            let results = search.get("results").and_then(Value::as_array).cloned().unwrap_or_default();
            // virtual packages are not directly useful from a composer perspective
            return Ok(results.into_iter().filter(|r| !r.get("virtual").is_some_and(php_truthy)).collect());
        }
        let regex = query_regex(query, false)?;
        let names_to_results =
            |names: Vec<String>| names.into_iter().filter(|n| regex.is_match(n)).map(|n| json!({"name": n, "description": ""})).collect();
        if mode == SearchMode::Vendor {
            return Ok(names_to_results(self.vendor_names()?));
        }
        if root.lazy_url.is_some() {
            // "^foo/bar" queries are answered by the list API directly
            if let (Some(c), Some(list_url)) = (VENDOR_QUERY.captures(query), &root.list_url) {
                let url = format!("{list_url}?vendor={}&filter={}", urlencode(&c[2]), urlencode(&format!("{}*", &c[1])));
                let result = self.http.get(&url, &self.options)?.json()?;
                output_warnings(&self.io, &self.url, &result);
                return Ok(string_values(result.get("packageNames")).into_iter().map(|n| json!({"name": n, "description": ""})).collect());
            }
            return Ok(names_to_results(self.package_names(None)?));
        }
        Ok(array_search(&self.packages()?, query, mode, kind))
    }

    fn vendor_names(&self) -> Result<Vec<String>> {
        const KEY: &str = "vendor-list.txt";
        if self.cache.age(KEY).is_some_and(|a| a < 600)
            && let Some(cached) = self.cache.read(KEY)
        {
            return Ok(cached.split('\n').map(String::from).collect());
        }
        let mut vendors: Vec<String> = vec![];
        for name in self.package_names(None)? {
            let vendor = name.split('/').next().unwrap_or("").to_string();
            if !vendors.contains(&vendor) {
                vendors.push(vendor);
            }
        }
        if !self.cache.is_read_only() {
            self.cache.write(KEY, vendors.join("\n").as_bytes());
        }
        Ok(vendors)
    }

    /// `getPackageNames($filter)`, `*` wildcards allowed in the filter.
    pub fn package_names(&self, filter: Option<&str>) -> Result<Vec<String>> {
        let root = self.root()?;
        let filter_regex = filter.filter(|f| !f.is_empty()).map(package_name_regex);
        let filtered = |names: Vec<String>| names.into_iter().filter(|n| filter_regex.as_ref().is_none_or(|r| r.is_match(n))).collect::<Vec<_>>();
        if root.lazy_url.is_none() {
            return Ok(filtered(self.packages()?.iter().map(|p| p.pretty_name.clone()).collect()));
        }
        if let Some(available) = &root.available {
            let mut names: Vec<String> = available.iter().cloned().collect();
            names.sort();
            return Ok(filtered(names));
        }
        if let Some(list_url) = &root.list_url {
            return self.load_package_list(list_url, filter);
        }
        Ok(filtered(root.partial.as_ref().map(|p| p.keys().cloned().collect()).unwrap_or_default()))
    }

    /// `loadPackageList()`.
    fn load_package_list(&self, list_url: &str, filter: Option<&str>) -> Result<Vec<String>> {
        const KEY: &str = "package-list.txt";
        if let Some(filter) = filter.filter(|f| !f.is_empty()) {
            let result = self.http.get(&format!("{list_url}?filter={}", urlencode(filter)), &self.options)?.json()?;
            output_warnings(&self.io, &self.url, &result);
            return Ok(string_values(result.get("packageNames")));
        }
        if self.cache.age(KEY).is_some_and(|a| a < 600)
            && let Some(cached) = self.cache.read(KEY)
        {
            return Ok(cached.split('\n').map(String::from).collect());
        }
        let result = self.http.get(list_url, &self.options)?.json()?;
        output_warnings(&self.io, &self.url, &result);
        let names = string_values(result.get("packageNames"));
        if !self.cache.is_read_only() {
            self.cache.write(KEY, names.join("\n").as_bytes());
        }
        Ok(names)
    }

    /// `getFilterLists()`: the lists this repository serves that the user did not opt out of.
    pub fn filter_lists(&self) -> Result<Vec<String>> {
        let Some(skipped) = &self.user_filter else { return Ok(vec![]) };
        let root = self.root_with_max_age(Some(600))?;
        Ok(root.filter.iter().flat_map(|f| f.lists.iter()).filter(|l| !skipped.contains(*l)).cloned().collect())
    }

    /// `hasFilter()`.
    pub fn has_filter(&self) -> Result<bool> {
        if self.user_filter.is_none() {
            return Ok(false);
        }
        Ok(self.root_with_max_age(Some(600))?.filter.as_ref().is_some_and(|f| f.metadata))
    }

    /// `getFilter()`: filter entries of the given lists matching the constraints, by list.
    pub fn filter(&self, constraints: &[(String, Constraint)], lists: &[String]) -> Result<Vec<(String, Vec<FilterEntry>)>> {
        let root = self.root_with_max_age(Some(600))?;
        let mut st = self.state.lock().unwrap();
        let mut map: Vec<(String, Constraint)> =
            constraints.iter().filter(|(n, _)| !root.has_available_list() || root.contains(&n.to_lowercase())).cloned().collect();
        let info = root.filter.clone().unwrap_or_default();
        // skipped when package metadata was already pulled in this run: the cached files answer it
        if st.fresh_metadata_urls.is_empty() {
            if let Some(api_url) = &info.api_url {
                let purls: Vec<String> = map.iter().map(|(n, _)| format!("pkg://composer/{n}")).collect();
                let body = json::encode(&json!({"packages": purls, "lists": lists}), Flags::PLAIN);
                let opts = RequestOptions {
                    method: Some("POST".into()),
                    body: Some(body.into_bytes()),
                    timeout: Some(std::time::Duration::from_secs(10)),
                    ..self.options.clone()
                }
                .header("Content-type: application/json");
                let decoded = self.http.get(api_url, &opts)?.json()?;
                output_warnings(&self.io, &self.url, &decoded);
                let Some(filter) = decoded.get("filter").filter(|f| f.is_object() || f.is_array()) else {
                    bail!(crate::http::TransportError::new(
                        format!("Filter api-url {api_url} returned an unexpected response for {}", self.repo_name()),
                        None
                    ));
                };
                return Ok(build_filter_entries(filter, &map, None));
            }
            if info.summary_url.is_some() {
                let summary = self.load_filter_summary(&mut st, &info)?;
                let mut candidates = HashSet::new();
                for list in lists {
                    for (package, summary_constraint) in summary.get(list).into_iter().flatten() {
                        let Some((_, c)) = map.iter().find(|(n, _)| n == package) else { continue };
                        if *c != Constraint::Any && !komposer_semver::parse_constraints(summary_constraint).is_ok_and(|s| s.matches(c)) {
                            continue;
                        }
                        candidates.insert(package.clone());
                    }
                }
                map.retain(|(n, _)| candidates.contains(n));
            }
        }
        let wanted: Vec<(String, String)> =
            map.iter().map(|(n, _)| n.to_lowercase()).filter(|n| !is_platform_package(n) && n != ROOT_NAME).map(|n| (n.clone(), n)).collect();
        let mut out: Vec<(String, Vec<FilterEntry>)> = vec![];
        for ((name, _), response) in wanted.iter().zip(self.cached_sections(&mut st, &root, &wanted)?) {
            let Some(filter) = response.as_ref().and_then(|r| r.get("filter")).filter(|f| f.is_object() || f.is_array()) else { continue };
            for (list, entries) in build_filter_entries(filter, &map, Some(name)) {
                match out.iter_mut().find(|(l, _)| *l == list) {
                    Some((_, existing)) => existing.extend(entries),
                    None => out.push((list, entries)),
                }
            }
        }
        Ok(out)
    }

    /// `loadFilterSummary()`: list name => package name => constraint.
    fn load_filter_summary(&self, st: &mut State, info: &FilterInfo) -> Result<HashMap<String, Vec<(String, String)>>> {
        const KEY: &str = "filter-summary.json";
        let Some(url) = &info.summary_url else { return Ok(HashMap::new()) };
        let contents = self.cache.read(KEY).and_then(|c| json::parse(&c).ok()).filter(|c| c.is_object() || c.is_array());
        let last_modified = contents.as_ref().and_then(|c| c.get("last-modified")).and_then(Value::as_str).map(String::from);
        let fetched = self.async_fetch_files(st, &[(url.clone(), KEY.into(), last_modified)])?.pop();
        let data = match fetched {
            Some(Fetched::Fresh) => contents,
            Some(Fetched::Data(d)) => Some(d),
            _ => None,
        };
        let Some(filter) = data.as_ref().and_then(|d| d.get("filter")).filter(|f| f.is_object() || f.is_array()) else {
            bail!(crate::http::TransportError::new(format!("Filter summary URL {url} returned 404 for {}", self.repo_name()), Some(404)));
        };
        let mut summary: HashMap<String, Vec<(String, String)>> = HashMap::new();
        for (list, packages) in entries(filter) {
            let Some(packages) = packages.as_object().filter(|_| !list.chars().all(|c| c.is_ascii_digit())) else {
                if packages.as_array().is_some_and(Vec::is_empty) {
                    continue;
                }
                bail!("Invalid filter summary received from {}: list \"{list}\" must map to an object of package => constraint", self.repo_name());
            };
            for (package, constraint) in packages {
                let Some(constraint) = constraint.as_str() else {
                    bail!("Invalid filter summary received from {}: list \"{list}\" entries must be strings", self.repo_name());
                };
                summary.entry(list.clone()).or_default().push((package.to_lowercase(), constraint.to_string()));
            }
        }
        Ok(summary)
    }
}

/// `parseUserFilterConfig()`.
fn parse_user_filter(raw: Option<&Value>) -> Result<Option<HashSet<String>>> {
    match raw {
        Some(Value::Bool(false)) => Ok(None),
        None | Some(Value::Null) | Some(Value::Bool(true)) => Ok(Some(HashSet::new())),
        Some(Value::Object(m)) => {
            let mut skipped = HashSet::new();
            for (list, value) in m {
                if list.is_empty() || list.parse::<i64>().is_ok() {
                    bail!("Repository \"filter\" keys must be non-empty list-name strings.");
                }
                match value {
                    Value::Bool(true) => {}
                    Value::Bool(false) => {
                        skipped.insert(list.clone());
                    }
                    v => bail!("Repository \"filter\" entry for \"{list}\" must be a boolean; got {}.", var_export(v)),
                }
            }
            Ok(Some(skipped))
        }
        Some(Value::Array(a)) if a.is_empty() => Ok(Some(HashSet::new())),
        Some(Value::Array(_)) => bail!("Repository \"filter\" keys must be non-empty list-name strings."),
        Some(_) => bail!("Repository \"filter\" must be a boolean or an object mapping advertised list names to false."),
    }
}

fn var_export(v: &Value) -> String {
    match v {
        Value::String(s) => format!("'{s}'"),
        Value::Null => "NULL".into(),
        v => v.to_string(),
    }
}

/// `FilterListEntryBuilder::build()`.
fn build_filter_entries(raw: &Value, constraints: &[(String, Constraint)], default_package: Option<&str>) -> Vec<(String, Vec<FilterEntry>)> {
    let mut out: Vec<(String, Vec<FilterEntry>)> = vec![];
    for (list, list_entries) in entries(raw) {
        for data in values(list_entries) {
            let Some(constraint) = data.get("constraint").and_then(Value::as_str) else { continue };
            let Some(package) = data.get("package").and_then(Value::as_str).or(default_package) else { continue };
            let Ok(constraint) = komposer_semver::parse_constraints(constraint) else { continue };
            let Some((_, wanted)) = constraints.iter().find(|(n, _)| n == package) else { continue };
            if !constraint.matches(wanted) {
                continue;
            }
            let s = |k: &str| data.get(k).and_then(Value::as_str).map(String::from);
            let entry = FilterEntry {
                package_name: package.into(),
                list_name: list.clone(),
                constraint,
                url: s("url"),
                reason: s("reason"),
                id: s("id"),
                source: s("source"),
            };
            match out.iter_mut().find(|(l, _)| *l == list) {
                Some((_, e)) => e.push(entry),
                None => out.push((list.clone(), vec![entry])),
            }
        }
    }
    out
}

/// `[^a-z0-9.]` => `-` on the sanitized URL: Composer's per-repository cache directory name.
pub(crate) fn cache_dir_name(url: &str) -> String {
    static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)[^a-z0-9.]").unwrap());
    RE.replace_all(&sanitize(url), "-").into_owned()
}

/// `$data[$key] = $value` on decoded JSON (an empty list becomes a map, like PHP arrays).
/// The value of the trailing `"last-modified"` key a cached p2 file ends with, found without decoding it.
fn last_modified_of(text: &str) -> Option<&str> {
    const KEY: &str = "\"last-modified\":\"";
    let start = text.rfind(KEY)? + KEY.len();
    let len = text[start..].find('"')?;
    // only the top-level key appended last counts
    (text[start + len + 1..].trim() == "}").then(|| &text[start..start + len])
}

/// `body` with `"last-modified"` appended as its last key, when it is a plain JSON object without one.
fn with_last_modified(body: &[u8], data: &Value, last_modified: &str) -> Option<Vec<u8>> {
    let map = data.as_object()?;
    if map.contains_key("last-modified") {
        return None;
    }
    let end = body.iter().rposition(|b| !b.is_ascii_whitespace())?;
    if body[end] != b'}' {
        return None;
    }
    let mut out = body[..end].to_vec();
    if !map.is_empty() {
        out.push(b',');
    }
    out.extend_from_slice(b"\"last-modified\":");
    out.extend_from_slice(
        json::encode(&Value::String(last_modified.into()), Flags { pretty: false, unescaped_slashes: true, unescaped_unicode: true }).as_bytes(),
    );
    out.push(b'}');
    Some(out)
}

/// The parts of a p2 file the pool filters and audits use.
fn policy_sections(data: &Value) -> Value {
    let mut m = Map::new();
    for key in ["security-advisories", "filter"] {
        if let Some(v) = data.get(key) {
            m.insert(key.into(), v.clone());
        }
    }
    Value::Object(m)
}

/// `f` over `items` on all cores, in order. Small inputs stay on the calling thread.
fn par_map<T: Send, R: Send>(items: Vec<T>, f: impl Fn(T) -> R + Sync) -> Vec<R> {
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).min(items.len() / 2);
    if threads <= 1 {
        return items.into_iter().map(f).collect();
    }
    // handed out one at a time, as their cost varies a lot (laravel/framework next to a polyfill)
    let queue = Mutex::new(items.into_iter().enumerate());
    let mut out: Vec<(usize, R)> = std::thread::scope(|s| {
        let worker = || {
            let mut done = vec![];
            loop {
                let next = queue.lock().unwrap().next();
                let Some((i, item)) = next else { return done };
                done.push((i, f(item)));
            }
        };
        let handles: Vec<_> = (0..threads).map(|_| s.spawn(worker)).collect();
        handles.into_iter().flat_map(|h| h.join().unwrap()).collect()
    });
    out.sort_unstable_by_key(|(i, _)| *i);
    out.into_iter().map(|(_, r)| r).collect()
}

fn set_key(data: &mut Value, key: &str, value: Value) {
    if let Value::Array(a) = data {
        *data = Value::Object(a.drain(..).enumerate().map(|(i, v)| (i.to_string(), v)).collect());
    }
    if let Value::Object(m) = data {
        m.insert(key.into(), value);
    }
}

/// `foreach ($array as $key => $value)` over a decoded list or map.
fn entries(v: &Value) -> Vec<(String, &Value)> {
    match v {
        Value::Object(m) => m.iter().map(|(k, v)| (k.clone(), v)).collect(),
        Value::Array(a) => a.iter().enumerate().map(|(i, v)| (i.to_string(), v)).collect(),
        _ => vec![],
    }
}

fn values(v: &Value) -> impl Iterator<Item = &Value> {
    let (list, map) = match v {
        Value::Array(a) => (Some(a.iter()), None),
        Value::Object(m) => (None, Some(m.values())),
        _ => (None, None),
    };
    list.into_iter().flatten().chain(map.into_iter().flatten())
}

fn string_values(v: Option<&Value>) -> Vec<String> {
    v.map(values).into_iter().flatten().filter_map(Value::as_str).map(String::from).collect()
}

fn push_unique(list: &mut Vec<PackageRef>, p: PackageRef) {
    if !list.iter().any(|x| Arc::ptr_eq(x, &p)) {
        list.push(p);
    }
}

/// `version_normalized`, normalizing `version` when it is missing (or the Composer 1 default branch alias).
fn normalized_version(data: &Map<String, Value>) -> Result<String> {
    match data.get("version_normalized").and_then(Value::as_str) {
        Some(v) if v != DEFAULT_BRANCH_ALIAS => Ok(v.to_string()),
        _ => komposer_semver::normalize(&data.get("version").map(scalar).unwrap_or_default()).map_err(|e| anyhow!("{e}")),
    }
}

/// `isVersionAcceptable()`: the version or its branch alias passes stability and constraint.
fn is_version_acceptable(
    constraint: Option<&Constraint>,
    name: &str,
    data: &Map<String, Value>,
    normalized: &str,
    stability: Option<(&HashSet<Stability>, &HashMap<String, Stability>)>,
) -> bool {
    let mut versions = vec![normalized.to_string()];
    if let Ok(Some(alias)) = loader::branch_alias(data) {
        versions.push(alias);
    }
    versions.iter().any(|v| {
        stability.is_none_or(|(a, f)| is_package_acceptable(a, f, &[name.to_string()], komposer_semver::parse_stability(v)))
            && constraint.is_none_or(|c| c.matches_version(v))
    })
}

/// The search regex of `ArrayRepository::search()`.
fn query_regex(query: &str, quote: bool) -> Result<Regex> {
    let parts: Vec<String> = query.split_whitespace().map(|p| if quote { regex::escape(p) } else { p.to_string() }).collect();
    Regex::new(&format!("(?i)(?:{})", parts.join("|"))).map_err(|e| anyhow!("Invalid search query \"{query}\": {e}"))
}

impl Repository for ComposerRepository {
    fn loaded_files(&self) -> Vec<String> {
        let st = self.state.lock().unwrap();
        // prefetched files nobody asked for are left out, so hints of removed dependencies go away
        let mut files: Vec<String> = st.sections.keys().filter(|f| !st.prefetched.contains_key(*f)).cloned().collect();
        files.sort();
        files
    }

    fn prefetch(&self, names: &[String], dev_names: &[String]) {
        let Ok(root) = self.root() else { return };
        if root.lazy_url.is_none() {
            return;
        }
        let mut st = self.state.lock().unwrap();
        let wanted = |n: &String| {
            let n = n.to_lowercase();
            (!is_platform_package(&n) && n != ROOT_NAME && (!root.has_available_list() || root.contains(&n))).then_some(n)
        };
        let files: Vec<(String, String)> = names
            .iter()
            .filter_map(wanted)
            .map(|n| (n.clone(), n))
            .chain(dev_names.iter().filter_map(wanted).map(|n| (format!("{n}~dev"), n)))
            .filter(|(f, _)| !st.prefetched.contains_key(f) && !st.sections.contains_key(f))
            .collect();
        // failures resurface (and are reported) when the files are requested for real
        if let Ok(results) = self.cached_async_downloads(&mut st, &root, &files) {
            for ((file, _), result) in files.into_iter().zip(results) {
                st.prefetched.insert(file, result);
            }
        }
    }

    fn search(&self, query: &str, mode: SearchMode, kind: Option<&str>) -> Result<Vec<Value>> {
        self.search_impl(query, mode, kind)
    }

    fn repo_name(&self) -> String {
        format!("composer repo ({})", sanitize(&self.url))
    }

    fn load_packages(&self, req: &LoadRequest) -> Result<LoadResult> {
        let mut st = self.state.lock().unwrap();
        let root = self.load_root(&mut st, None)?;
        if root.lazy_url.is_none() {
            return self.all_packages(&mut st, &root)?.load_packages(req);
        }
        let stability = Some((req.acceptable_stabilities, req.stability_flags));
        let mut result = LoadResult::default();
        let mut lazy = vec![];
        for (name, constraint) in req.names {
            if !root.partial.as_ref().is_some_and(|p| p.contains_key(name)) {
                lazy.push((name.clone(), constraint.clone()));
                continue;
            }
            let candidates = self.what_provides_partial(&root, name, stability, req.already_loaded)?;
            if !candidates.is_empty() && !result.names_found.contains(name) {
                result.names_found.push(name.clone());
            }
            let mut matches = vec![];
            for c in &candidates {
                if constraint.as_ref().is_none_or(|con| con.matches_version(&c.version)) {
                    push_unique(&mut matches, c.clone());
                    if let Some(of) = &c.alias_of {
                        push_unique(&mut matches, of.clone());
                    }
                }
            }
            // aliases of matched packages, even if they did not match the constraint
            for c in &candidates {
                if c.alias_of.as_ref().is_some_and(|of| matches.iter().any(|m| Arc::ptr_eq(m, of))) {
                    push_unique(&mut matches, c.clone());
                }
            }
            result.packages.extend(matches);
        }
        if !lazy.is_empty() {
            if root.has_available_list() {
                lazy.retain(|(name, _)| root.contains(&name.to_lowercase()));
            }
            let (found, packages) = self.load_async(&mut st, &root, &lazy, stability, req.already_loaded)?;
            for name in found {
                if !result.names_found.contains(&name) {
                    result.names_found.push(name);
                }
            }
            result.packages.extend(packages);
        }
        Ok(result)
    }

    fn packages(&self) -> Result<Vec<PackageRef>> {
        let mut st = self.state.lock().unwrap();
        let root = self.load_root(&mut st, None)?;
        if root.lazy_url.is_none() {
            return self.all_packages(&mut st, &root)?.packages();
        }
        if let (Some(available), None) = (&root.available, &root.patterns) {
            let names: Vec<(String, Option<Constraint>)> = available.iter().map(|n| (n.clone(), None)).collect();
            return Ok(self.load_async(&mut st, &root, &names, None, &HashMap::new())?.1);
        }
        if let Some(partial) = &root.partial {
            let mut out = vec![];
            for name in partial.keys() {
                out.extend(self.what_provides_partial(&root, name, None, &HashMap::new())?);
            }
            return Ok(out);
        }
        Ok(st.loaded.clone())
    }

    fn find_packages(&self, name: &str, constraint: Option<&Constraint>) -> Result<Vec<PackageRef>> {
        let name = name.to_lowercase();
        let mut st = self.state.lock().unwrap();
        let root = self.load_root(&mut st, None)?;
        if root.lazy_url.is_none() {
            let all = self.all_packages(&mut st, &root)?;
            drop(st);
            return all.find_packages(&name, constraint);
        }
        let matching = |p: &PackageRef| constraint.is_none_or(|c| c.matches(&Constraint::single(Op::Eq, p.version.clone())));
        if root.partial.as_ref().is_some_and(|p| p.contains_key(&name)) {
            return Ok(self.what_provides_partial(&root, &name, None, &HashMap::new())?.into_iter().filter(matching).collect());
        }
        if root.has_available_list() && !root.contains(&name) {
            return Ok(vec![]);
        }
        Ok(self.load_async(&mut st, &root, &[(name, constraint.cloned())], None, &HashMap::new())?.1)
    }

    fn has_security_advisories(&self) -> bool {
        self.root_with_max_age(Some(600)).is_ok_and(|r| r.advisories.as_ref().is_some_and(|(metadata, api)| *metadata || api.is_some()))
    }

    fn security_advisories(&self, constraints: &[(String, Constraint)], allow_partial: bool) -> Result<AdvisoryResult> {
        let root = self.root_with_max_age(Some(600))?;
        let mut result = AdvisoryResult::default();
        let Some((metadata, api_url)) = root.advisories.clone() else { return Ok(result) };
        // respect available-package-patterns / available-packages directives from the repo
        let mut map: Vec<(String, Constraint)> =
            constraints.iter().filter(|(n, _)| !root.has_available_list() || root.contains(&n.to_lowercase())).cloned().collect();
        let create = |name: &str, data: &Value, map: &[(String, Constraint)]| -> Result<Option<SecurityAdvisory>> {
            let advisory = create_advisory(name, data);
            if !allow_partial && advisory.is_partial() {
                bail!("Advisory for {name} could not be loaded as a full advisory from {}\n{}", self.repo_name(), json::encode(data, Flags::PLAIN));
            }
            let wanted = map.iter().find(|(n, _)| n == name).map(|(_, c)| c);
            Ok(wanted.is_some_and(|c| advisory.affected_versions.matches(c)).then_some(advisory))
        };
        let add = |result: &mut AdvisoryResult, name: &str, list: Vec<SecurityAdvisory>| {
            if !result.names_found.iter().any(|n| n == name) {
                result.names_found.push(name.to_string());
            }
            if !list.is_empty() {
                result.advisories.insert(name.to_string(), list);
            }
        };

        if metadata && (allow_partial || api_url.is_none()) {
            let wanted: Vec<(String, String)> =
                map.iter().map(|(n, _)| n.to_lowercase()).filter(|n| !is_platform_package(n) && n != ROOT_NAME).map(|n| (n.clone(), n)).collect();
            let downloads = {
                let mut st = self.state.lock().unwrap();
                self.cached_sections(&mut st, &root, &wanted)?
            };
            let mut done = HashSet::new();
            for ((name, _), response) in wanted.iter().zip(downloads) {
                let Some(list) = response.as_ref().and_then(|r| r.get("security-advisories")).filter(|a| a.is_array() || a.is_object()) else {
                    continue;
                };
                let mut advisories = vec![];
                for data in values(list) {
                    advisories.extend(create(name, data, &map)?);
                }
                add(&mut result, name, advisories);
                done.insert(name.clone());
            }
            map.retain(|(n, _)| !done.contains(n));
        }

        if let Some(api_url) = api_url.filter(|_| !map.is_empty()) {
            let names: Vec<String> = map.iter().map(|(n, _)| n.clone()).collect();
            let requests = names
                .chunks(ADVISORY_API_BATCH_SIZE)
                .map(|batch| {
                    let opts = RequestOptions {
                        method: Some("POST".into()),
                        body: Some(http_build_query("packages", batch).into_bytes()),
                        timeout: Some(std::time::Duration::from_secs(10)),
                        ..self.options.clone()
                    }
                    .header("Content-type: application/x-www-form-urlencoded");
                    (api_url.clone(), opts)
                })
                .collect();
            let mut warned_unrequested = false;
            for (i, response) in self.http.get_many(requests).into_iter().enumerate() {
                let data = response?.json()?;
                if i == 0 {
                    output_warnings(&self.io, &self.url, &data);
                }
                for (name, list) in data.get("advisories").map(entries).unwrap_or_default() {
                    if !names.contains(&name) {
                        if !warned_unrequested {
                            let requested =
                                if names.len() > 20 { format!("{} and {} more", names[..20].join(", "), names.len() - 20) } else { names.join(", ") };
                            self.io.warning(&format!(
                                "{} returned names which were not requested in response to the security-advisories API. {name} was not requested but is present in the response. Requested names were: {requested}",
                                self.repo_name()
                            ));
                            warned_unrequested = true;
                        }
                        continue;
                    }
                    let mut advisories = vec![];
                    for data in values(list) {
                        advisories.extend(create(&name, data, &map)?);
                    }
                    add(&mut result, &name, advisories);
                }
            }
        }
        Ok(result)
    }

    fn filter_entries(&self, constraints: &[(String, Constraint)], lists: &[String]) -> Result<Vec<FilterEntry>> {
        if !self.has_filter()? {
            return Ok(vec![]);
        }
        let provided = self.filter_lists()?;
        let relevant: Vec<String> = lists.iter().filter(|l| provided.contains(l)).cloned().collect();
        if relevant.is_empty() {
            return Ok(vec![]);
        }
        let entries = self.filter(constraints, &relevant)?.into_iter().filter(|(list, _)| relevant.contains(list)).flat_map(|(_, e)| e);
        Ok(entries.filter(|e| constraints.iter().any(|(n, c)| *n == e.package_name && e.constraint.matches(c))).collect())
    }

    fn providers(&self, name: &str) -> Result<Vec<String>> {
        Ok(self.provider_descriptions(name)?.into_iter().map(|(n, _)| n).collect())
    }

    /// `ComposerRepository::getProviders()`: from `providers-api`, or the loaded metadata.
    fn provider_descriptions(&self, name: &str) -> Result<Vec<(String, Option<String>)>> {
        let root = self.root()?;
        if let Some(api) = &root.providers_api_url {
            let result = match self.http.get(&api.replace("%package%", name), &self.options) {
                Ok(r) => r.json()?,
                Err(e) if error_status(&e) == Some(404) => return Ok(vec![]),
                Err(e) => return Err(e),
            };
            output_warnings(&self.io, &self.url, &result);
            let providers = result.get("providers").map(values).into_iter().flatten();
            return Ok(providers
                .filter_map(|p| {
                    let n = p.get("name").and_then(Value::as_str)?;
                    Some((n.to_string(), p.get("description").and_then(Value::as_str).map(String::from)))
                })
                .collect());
        }
        let mut out: Vec<(String, Option<String>)> = vec![];
        let description = |v: &Value| v.get("description").and_then(Value::as_str).map(String::from);
        for versions in root.partial.iter().flat_map(|p| p.values()) {
            for candidate in versions {
                let candidate_name = candidate.get("name").and_then(Value::as_str).unwrap_or("");
                if candidate.get("provide").and_then(|p| p.get(name)).is_some() && !out.iter().any(|(n, _)| n == candidate_name) {
                    out.push((candidate_name.into(), description(candidate)));
                }
            }
        }
        let loaded = if root.lazy_url.is_none() { self.packages()? } else { self.state.lock().unwrap().loaded.clone() };
        for p in loaded {
            if p.provides.iter().any(|l| l.target == name) && !out.iter().any(|(n, _)| *n == p.pretty_name) {
                out.push((p.pretty_name.clone(), p.meta.get("description").and_then(Value::as_str).map(String::from)));
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestServer, test_config};
    use komposer_package::config::Config;
    use std::collections::HashSet;

    fn repo_with(server: &TestServer, config: &Config, repo: Value) -> ComposerRepository {
        let mut repo = repo;
        repo["url"] = format!("{}{}", server.url, repo["url"].as_str().unwrap_or("")).into();
        ComposerRepository::new(&repo, HttpClient::new(Arc::new(Io::null()), config).unwrap()).unwrap()
    }

    fn repo(server: &TestServer, repo: Value) -> ComposerRepository {
        repo_with(server, &test_config(), repo)
    }

    fn eq(version: &str) -> Constraint {
        Constraint::single(Op::Eq, version)
    }

    fn load(repo: &ComposerRepository, names: &[&str], stabilities: &[Stability]) -> LoadResult {
        let names: Vec<(String, Option<Constraint>)> = names.iter().map(|n| (n.to_string(), None)).collect();
        let acceptable: HashSet<Stability> = stabilities.iter().copied().collect();
        let req =
            LoadRequest { names: &names, acceptable_stabilities: &acceptable, stability_flags: &HashMap::new(), already_loaded: &HashMap::new() };
        repo.load_packages(&req).unwrap()
    }

    fn pretty(packages: &[PackageRef]) -> Vec<String> {
        packages.iter().map(|p| format!("{} {}", p.pretty_name, p.pretty_version)).collect()
    }

    #[test]
    fn expands_minified_metadata() {
        let minified = vec![
            json!({"name": "foo/bar", "version": "2.0.0", "version_normalized": "2.0.0.0", "type": "library", "scripts": {"foo": ["bar"]}, "license": ["MIT"]}),
            json!({"version": "1.2.0", "version_normalized": "1.2.0.0", "license": ["GPL"], "homepage": "https://example.org", "scripts": "__unset"}),
            json!({"version": "1.0.0", "version_normalized": "1.0.0.0", "homepage": "__unset"}),
            json!({"version": "0.1.0", "version_normalized": "0.1.0.0"}),
        ];
        let expanded = expand(&minified);
        assert_eq!(
            expanded[1],
            json!({"name": "foo/bar", "version": "1.2.0", "version_normalized": "1.2.0.0", "type": "library", "license": ["GPL"], "homepage": "https://example.org"})
        );
        assert_eq!(
            expanded[1].as_object().unwrap().keys().collect::<Vec<_>>(),
            ["name", "version", "version_normalized", "type", "license", "homepage"]
        );
        assert_eq!(
            expanded[3],
            json!({"name": "foo/bar", "version": "0.1.0", "version_normalized": "0.1.0.0", "type": "library", "license": ["GPL"]})
        );
        let with_nulls = expand(&[json!({"name": "foo/bar", "version": "2.0.0", "dist": null}), json!({"version": "1.0.0"})]);
        assert_eq!(with_nulls[1], json!({"name": "foo/bar", "version": "1.0.0", "dist": null}));
    }

    /// `ComposerRepositoryTest::testLoadData()`.
    #[test]
    fn loads_root_package_formats() {
        let formats = [
            (json!({"foo/bar": {"name": "foo/bar", "versions": {"1.0.0": {"name": "foo/bar", "version": "1.0.0"}}}}), vec!["foo/bar 1.0.0"]),
            (
                json!({"packages": {"bar/foo": {"3.14": {"name": "bar/foo", "version": "3.14"}, "3.145": {"name": "bar/foo", "version": "3.145"}}}}),
                vec!["bar/foo 3.14", "bar/foo 3.145"],
            ),
            (
                json!({"packages": {"bar/foo": [{"name": "bar/foo", "version": "3.14"}, {"name": "bar/foo", "version": "3.145"}]}}),
                vec!["bar/foo 3.14", "bar/foo 3.145"],
            ),
        ];
        for (root, expected) in formats {
            let server = TestServer::new();
            server.route("/packages.json", 200, &[], &root.to_string());
            let repo = repo(&server, json!({}));
            assert_eq!(pretty(&repo.packages().unwrap()), expected);
        }
    }

    #[test]
    fn includes_and_notify_url() {
        let server = TestServer::new();
        server.route(
            "/packages.json",
            200,
            &[],
            &json!({"packages": [], "notify-batch": "/downloads/", "includes": {"include/all$abc.json": {"sha1": "x"}}}).to_string(),
        );
        server.route("/include/all%24abc.json", 200, &[], &json!({"packages": {"a/a": {"1.0.0": {"name": "a/a", "version": "1.0.0", "dist": {"type": "zip", "url": format!("{}/dists/a.zip", server.url)}}}}}).to_string());
        let repo = repo(&server, json!({"options": {"http": {"header": ["X-Test: 1"]}}}));
        let packages = load(&repo, &["a/a"], &[Stability::Stable]).packages;
        assert_eq!(pretty(&packages), ["a/a 1.0.0"]);
        assert_eq!(packages[0].notification_url.as_deref(), Some(format!("{}/downloads/", server.url).as_str()));
        // dists served by the repository get its transport options
        assert_eq!(packages[0].transport_options, json!({"http": {"header": ["X-Test: 1"]}}));
        assert_eq!(server.requests()[1].header("x-test"), Some("1"));
    }

    #[test]
    fn lazy_loads_names_concurrently_with_dev_files() {
        let server = TestServer::new();
        server.route("/packages.json", 200, &[], &json!({"metadata-url": "/p2/%package%.json"}).to_string());
        let versions = json!({"minified": "composer/2.0", "packages": {"a/a": [
            {"name": "a/a", "version": "2.0.0", "version_normalized": "2.0.0.0", "require": {"b/b": "^1.0"}},
            {"version": "1.0.0", "version_normalized": "1.0.0.0", "require": "__unset"},
        ]}});
        server.route("/p2/a/a.json", 200, &[("Last-Modified", "Tue, 01 Jan 2030 00:00:00 GMT")], &versions.to_string());
        server.route("/p2/a/a~dev.json", 200, &[], &json!({"packages": {"a/a": [{"name": "a/a", "version": "dev-main", "version_normalized": "dev-main", "extra": {"branch-alias": {"dev-main": "3.x-dev"}}}]}}).to_string());
        server.route("/p2/b/b.json", 200, &[], &json!({"packages": {"b/b": [{"name": "b/b", "version": "1.0.0"}]}}).to_string());
        let config = test_config();
        let repo = repo_with(&server, &config, json!({}));

        let result = load(&repo, &["a/a", "b/b", "c/c", "php", "ext-json"], &[Stability::Stable, Stability::Dev]);
        assert_eq!(result.names_found, ["a/a", "b/b"]);
        assert_eq!(pretty(&result.packages), ["a/a 2.0.0", "a/a 1.0.0", "b/b 1.0.0", "a/a 3.x-dev", "a/a dev-main"]);
        assert_eq!(result.packages[1].requires.len(), 0);
        let mut paths = server.paths();
        paths.sort();
        assert_eq!(
            paths,
            ["/p2/a/a.json", "/p2/a/a~dev.json", "/p2/b/b.json", "/p2/b/b~dev.json", "/p2/c/c.json", "/p2/c/c~dev.json", "/packages.json"]
        );

        // the cache keeps the last-modified date, and a new repository revalidates with it
        let cache = repo.cache.read("provider-a~a.json").unwrap();
        assert!(cache.contains(r#""last-modified":"Tue, 01 Jan 2030 00:00:00 GMT""#));
        server.route_times("/p2/a/a.json", 304, &[], "", 1);
        let routes_before = server.requests().len();
        let again = repo_with(&server, &config, json!({}));
        assert_eq!(pretty(&load(&again, &["a/a"], &[Stability::Stable]).packages), ["a/a 2.0.0", "a/a 1.0.0"]);
        let new_requests = &server.requests()[routes_before..];
        let p2 = new_requests.iter().find(|r| r.path == "/p2/a/a.json").unwrap();
        assert_eq!(p2.header("if-modified-since"), Some("Tue, 01 Jan 2030 00:00:00 GMT"));
        // stable only: no ~dev file
        assert!(!new_requests.iter().any(|r| r.path == "/p2/a/a~dev.json"));
    }

    #[test]
    fn prefetched_files_are_used_and_only_loaded_ones_become_hints() {
        let server = TestServer::new();
        server.route("/packages.json", 200, &[], &json!({"metadata-url": "/p2/%package%.json"}).to_string());
        server.route("/p2/a/a.json", 200, &[], &json!({"packages": {"a/a": [{"name": "a/a", "version": "1.0.0"}]}}).to_string());
        server.route("/p2/b/b.json", 200, &[], &json!({"packages": {"b/b": [{"name": "b/b", "version": "1.0.0"}]}}).to_string());
        let repo = repo(&server, json!({}));
        repo.prefetch(&["a/a".into(), "b/b".into(), "php".into()], &[]);
        let mut paths = server.paths();
        paths.sort();
        assert_eq!(paths, ["/p2/a/a.json", "/p2/b/b.json", "/packages.json"]);
        assert_eq!(pretty(&load(&repo, &["a/a"], &[Stability::Stable]).packages), ["a/a 1.0.0"]);
        assert_eq!(server.paths().len(), 3, "a/a came from the prefetch");
        // b/b was never asked for, so the next update does not prefetch it again
        assert_eq!(repo.loaded_files(), ["a/a"]);
    }

    #[test]
    fn only_dev_stability_skips_the_stable_file_and_filters_constraints() {
        let server = TestServer::new();
        server.route("/packages.json", 200, &[], &json!({"metadata-url": "/p2/%package%.json", "available-packages": ["a/a"]}).to_string());
        server.route(
            "/p2/a/a~dev.json",
            200,
            &[],
            &json!({"packages": {"a/a": [{"name": "a/a", "version": "dev-main"}, {"name": "a/a", "version": "dev-other"}]}}).to_string(),
        );
        let repo = repo(&server, json!({}));
        let names = vec![("a/a".to_string(), Some(eq("dev-main"))), ("x/y".to_string(), None)];
        let acceptable: HashSet<Stability> = [Stability::Dev].into();
        let req =
            LoadRequest { names: &names, acceptable_stabilities: &acceptable, stability_flags: &HashMap::new(), already_loaded: &HashMap::new() };
        let result = repo.load_packages(&req).unwrap();
        assert_eq!(pretty(&result.packages), ["a/a dev-main"]);
        // x/y is not in available-packages, a/a.json is not needed
        assert_eq!(server.paths(), ["/packages.json", "/p2/a/a~dev.json"]);
    }

    #[test]
    fn inline_packages_next_to_metadata_url() {
        let server = TestServer::new();
        server.route("/packages.json", 200, &[], &json!({"metadata-url": "/p2/%package%.json", "packages": {"foo/bar": {"dev-branch": {"name": "foo/bar", "version": "dev-branch"}, "v1.0.0": {"name": "foo/bar", "version": "v1.0.0", "provide": {"x/y": "1.0"}}}}}).to_string());
        let repo = repo(&server, json!({"url": "/packages.json"}));
        assert_eq!(repo.package_names(None).unwrap(), ["foo/bar"]);
        assert_eq!(pretty(&load(&repo, &["foo/bar"], &[Stability::Stable]).packages), ["foo/bar v1.0.0"]);
        assert_eq!(repo.providers("x/y").unwrap(), ["foo/bar"]);
        // nothing but the root file was needed
        assert_eq!(server.paths(), ["/packages.json"]);
    }

    #[test]
    fn network_failures_fall_back_to_the_cache() {
        let server = TestServer::new();
        server.route_times("/packages.json", 200, &[], &json!({"packages": {"a/a": [{"name": "a/a", "version": "1.0.0"}]}}).to_string(), 1);
        server.route("/packages.json", 500, &[], "");
        let config = test_config();
        assert_eq!(repo_with(&server, &config, json!({})).packages().unwrap().len(), 1);
        // the next run cannot reach the server but still has the cached root file
        assert_eq!(repo_with(&server, &config, json!({})).packages().unwrap().len(), 1);
    }

    #[test]
    fn search() {
        let server = TestServer::new();
        server.route("/packages.json", 200, &[], &json!({"search": "/search.json?q=%query%&type=%type%"}).to_string());
        server.route(
            "/search.json?q=foo&type=composer-plugin",
            200,
            &[],
            &json!({"results": [{"name": "foo", "description": null}, {"name": "virt", "virtual": true}]}).to_string(),
        );
        server.route("/search.json?q=foo+bar&type=", 200, &[], "[]");
        let mut config = test_config();
        config.merge_config(&json!({"cache-read-only": true}));
        let repo = repo_with(&server, &config, json!({}));
        assert_eq!(repo.search("foo", SearchMode::Fulltext, Some("composer-plugin")).unwrap(), [json!({"name": "foo", "description": null})]);
        assert!(repo.search("foo bar", SearchMode::Fulltext, None).unwrap().is_empty());

        let server = TestServer::new();
        server.route("/packages.json", 200, &[], &json!({"search": "/search.json?q=%query%"}).to_string());
        let results =
            json!({"results": [{"name": "foo1", "description": null, "abandoned": true}, {"name": "foo2", "description": null, "abandoned": "bar"}]});
        server.route("/search.json?q=foo", 200, &[], &results.to_string());
        assert_eq!(Value::Array(repo_with(&server, &config, json!({})).search("foo", SearchMode::Fulltext, None).unwrap()), results["results"]);
    }

    #[test]
    fn canonicalizes_urls() {
        let server = TestServer::new();
        let mut repo = repo(&server, json!({}));
        for (expected, url, repo_url) in [
            ("https://example.org/path/to/file", "/path/to/file", "https://example.org"),
            ("https://example.org/canonic_url", "https://example.org/canonic_url", "https://should-not-see-me.test"),
            ("file:///path/to/repository/file", "/path/to/repository/file", "file:///path/to/repository"),
            ("invalid_repo_url", "/path/to/file", "invalid_repo_url"),
            ("https://example.org/path/to/unusual_$0_filename", "/path/to/unusual_$0_filename", "https://example.org"),
        ] {
            repo.url = repo_url.into();
            assert_eq!(repo.canonicalize_url(url), expected);
        }
        assert_eq!(cache_dir_name("https://repo.packagist.org"), "https---repo.packagist.org");
        let packagist =
            ComposerRepository::new(&json!({"url": "https://packagist.org"}), HttpClient::new(Arc::new(Io::null()), &test_config()).unwrap())
                .unwrap();
        assert_eq!(packagist.url(), "https://repo.packagist.org");
        assert_eq!(packagist.repo_name(), "composer repo (https://repo.packagist.org)");
    }

    fn advisory(id: &str, affected: &str) -> Value {
        json!({"advisoryId": id, "packageName": "foo/bar", "remoteId": "test", "title": "Security Advisory", "link": null, "cve": "CVE-1999-1000",
            "affectedVersions": affected, "source": "Tests", "reportedAt": "2024-04-30 12:37:47", "composerRepository": "Package Repository",
            "severity": "high", "sources": [{"name": "Security Advisory", "remoteId": "test"}]})
    }

    #[test]
    fn security_advisories_api() {
        let server = TestServer::new();
        server.route(
            "/packages.json",
            200,
            &[],
            &json!({"metadata-url": "/p2/%package%.json", "security-advisories": {"api-url": "/security-advisories"}}).to_string(),
        );
        server.route(
            "/security-advisories",
            200,
            &[],
            &json!({"advisories": {"foo/bar": [advisory("A1", ">=1.0.0,<1.1.0"), advisory("A2", ">=2.0.0"), advisory("A3", ">=1.0.0,<1.1.0")]}})
                .to_string(),
        );
        let repo = repo(&server, json!({"url": "/packages.json", "options": {"http": {"header": ["X-Repo: yes"]}}}));
        assert!(repo.has_security_advisories());
        let result = repo.security_advisories(&[("foo/bar".into(), eq("1.0.0.0"))], false).unwrap();
        let ids: Vec<&str> = result.advisories["foo/bar"].iter().map(|a| a.advisory_id.as_str()).collect();
        assert_eq!(ids, ["A1", "A3"]);
        assert_eq!(result.names_found, ["foo/bar"]);
        assert_eq!(result.advisories["foo/bar"][0].reported_at.as_deref(), Some("2024-04-30T12:37:47+00:00"));
        let req = &server.requests()[1];
        assert_eq!((req.method.as_str(), req.body.as_str()), ("POST", "packages%5B0%5D=foo%2Fbar"));
        assert_eq!(req.header("content-type"), Some("application/x-www-form-urlencoded"));
        assert_eq!(req.header("x-repo"), Some("yes"));
    }

    #[test]
    fn security_advisories_are_batched() {
        let server = TestServer::new();
        server.route(
            "/packages.json",
            200,
            &[],
            &json!({"metadata-url": "/p2/%package%.json", "security-advisories": {"api-url": "/security-advisories"}}).to_string(),
        );
        server.route_times("/security-advisories", 200, &[], &json!({"advisories": []}).to_string(), 2);
        server.route("/security-advisories", 200, &[], &json!({"advisories": {"foo/pkg1100": [advisory("A", ">=1.0.0")]}}).to_string());
        let repo = repo(&server, json!({}));
        let map: Vec<(String, Constraint)> = (0..1200).map(|i| (format!("foo/pkg{i}"), Constraint::Any)).collect();
        let result = repo.security_advisories(&map, false).unwrap();
        let posts: Vec<_> = server.requests().into_iter().filter(|r| r.method == "POST").collect();
        assert_eq!(posts.len(), 3);
        let mut counts: Vec<usize> = posts.iter().map(|r| r.body.matches("packages%5B").count()).collect();
        counts.sort();
        assert_eq!(counts, [200, 500, 500]);
        // whichever batch got the advisory, only that name is reported
        assert!(result.advisories.keys().all(|k| k == "foo/pkg1100"));
    }

    #[test]
    fn partial_advisories_from_metadata() {
        let server = TestServer::new();
        server.route(
            "/packages.json",
            200,
            &[],
            &json!({"metadata-url": "/p2/%package%.json", "available-packages": ["foo/bar"], "security-advisories": {"metadata": true}}).to_string(),
        );
        server.route(
            "/p2/foo/bar.json",
            200,
            &[],
            &json!({"packages": {"foo/bar": []}, "security-advisories": [{"advisoryId": "P1", "affectedVersions": "<1.2"}]}).to_string(),
        );
        let repo = repo(&server, json!({}));
        let result = repo.security_advisories(&[("foo/bar".into(), eq("1.0.0.0")), ("other/pkg".into(), eq("1.0.0.0"))], true).unwrap();
        assert!(result.advisories["foo/bar"][0].is_partial());
        assert!(repo.security_advisories(&[("foo/bar".into(), eq("1.0.0.0"))], false).is_err());
        let err =
            ComposerRepository::new(&json!({"url": format!("{}/bad", server.url)}), HttpClient::new(Arc::new(Io::null()), &test_config()).unwrap())
                .unwrap();
        server.route(
            "/bad/packages.json",
            200,
            &[],
            &json!({"metadata-url": "/p2/%package%.json", "security-advisories": {"metadata": true}}).to_string(),
        );
        assert!(err.security_advisories(&[], true).err().unwrap().to_string().starts_with("Invalid security advisory configuration on"));
    }

    fn filter_root(filter: Value) -> String {
        json!({"metadata-url": "/p2/%package%.json", "filter": filter}).to_string()
    }

    fn entry(id: &str) -> Value {
        json!({"constraint": "*", "url": "https://example.org/x/filters.json", "reason": "Malicious code detected", "id": id})
    }

    #[test]
    fn filter_lists() {
        let server = TestServer::new();
        server.route(
            "/packages.json",
            200,
            &[],
            &filter_root(json!({"metadata": true, "lists": {"test": {"enabled": true}, "advisories": {"enabled": true}}})),
        );
        server.route("/p2/acme/package.json", 200, &[], &json!({"filter": {"test": [entry("ID-test")]}}).to_string());
        let repo = repo(&server, json!({}));
        assert_eq!(repo.filter_lists().unwrap(), ["test"]);
        let entries = repo.filter_entries(&[("acme/package".into(), eq("1.0.0.0"))], &["test".into()]).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(
            (entries[0].package_name.as_str(), entries[0].list_name.as_str(), entries[0].id.as_deref()),
            ("acme/package", "test", Some("ID-test"))
        );
        assert_eq!(entries[0].constraint, Constraint::Any);
        assert_eq!(entries[0].reason.as_deref(), Some("Malicious code detected"));
    }

    #[test]
    fn user_filter_config() {
        let server = TestServer::new();
        let repo = repo(&server, json!({"filter": false}));
        assert!(!repo.has_filter().unwrap());
        assert!(repo.filter_lists().unwrap().is_empty());
        assert!(server.paths().is_empty());

        server.route("/packages.json", 200, &[], &filter_root(json!({"metadata": true, "lists": {"malware": {"enabled": true}, "typosquatting": {"enabled": true}, "deprecated": {"enabled": true}}})));
        let opted_out = super::tests::repo(&server, json!({"filter": {"typosquatting": false, "unknown-list": false}}));
        assert!(opted_out.has_filter().unwrap());
        assert_eq!(opted_out.filter_lists().unwrap(), ["malware", "deprecated"]);
        let with_true = super::tests::repo(&server, json!({"filter": {"malware": true, "typosquatting": false}}));
        assert_eq!(with_true.filter_lists().unwrap(), ["malware", "deprecated"]);
        let http = HttpClient::new(Arc::new(Io::null()), &test_config()).unwrap();
        assert!(ComposerRepository::new(&json!({"url": "https://x", "filter": "yes"}), http.clone()).is_err());
        assert!(
            ComposerRepository::new(&json!({"url": "https://x", "filter": {"a": 1}}), http)
                .err()
                .unwrap()
                .to_string()
                .contains("must be a boolean; got 1.")
        );
    }

    #[test]
    fn filter_summary_limits_metadata_fetches() {
        let server = TestServer::new();
        server.route("/packages.json", 200, &[], &filter_root(json!({"metadata": true, "lists": {"malware": {"enabled": true}, "typosquatting": {"enabled": true}}, "summary-url": "/lists/all/summary.json"})));
        server.route(
            "/lists/all/summary.json",
            200,
            &[],
            &json!({"filter": {"malware": {"evil/pkg": "^1.0"}, "typosquatting": {"lookalike/pkg": "*"}}}).to_string(),
        );
        server.route("/p2/evil/pkg.json", 200, &[], &json!({"filter": {"malware": [entry("ID-test")]}}).to_string());
        let wanted = [("evil/pkg".to_string(), eq("1.0.0.0")), ("safe/pkg".to_string(), eq("1.0.0.0")), ("lookalike/pkg".to_string(), eq("1.0.0.0"))];
        let entries = repo(&server, json!({})).filter_entries(&wanted, &["malware".into()]).unwrap();
        assert_eq!(entries.iter().map(|e| e.package_name.as_str()).collect::<Vec<_>>(), ["evil/pkg"]);
        assert_eq!(server.paths(), ["/packages.json", "/lists/all/summary.json", "/p2/evil/pkg.json"]);

        // a constraint the summary does not intersect needs no metadata
        let server2 = TestServer::new();
        server2.route(
            "/packages.json",
            200,
            &[],
            &filter_root(json!({"metadata": true, "lists": {"malware": {"enabled": true}}, "summary-url": "/lists/all/summary.json"})),
        );
        server2.route("/lists/all/summary.json", 200, &[], &json!({"filter": {"malware": {"evil/pkg": "^1.0"}}}).to_string());
        assert!(repo(&server2, json!({})).filter_entries(&[("evil/pkg".into(), eq("2.5.0.0"))], &["malware".into()]).unwrap().is_empty());
        assert_eq!(server2.paths(), ["/packages.json", "/lists/all/summary.json"]);
    }

    #[test]
    fn filter_summary_is_skipped_once_metadata_was_fetched_and_revalidated_with_304() {
        let server = TestServer::new();
        let lm = [("Last-Modified", "Tue, 01 Jan 2030 00:00:00 GMT")];
        server.route(
            "/packages.json",
            200,
            &lm,
            &filter_root(json!({"metadata": true, "lists": {"malware": {"enabled": true}}, "summary-url": "/lists/all/summary.json"})),
        );
        server.route_times("/lists/all/summary.json", 200, &lm, &json!({"filter": {"malware": {"evil/pkg": "*"}}}).to_string(), 1);
        server.route_times("/p2/evil/pkg.json", 200, &lm, &json!({"filter": {"malware": [entry("ID-test")]}}).to_string(), 1);
        server.route("/lists/all/summary.json", 304, &[], "");
        server.route("/p2/evil/pkg.json", 304, &[], "");
        let config = test_config();
        let wanted = [("evil/pkg".to_string(), eq("1.0.0.0"))];
        let first = repo_with(&server, &config, json!({}));
        assert_eq!(first.filter_entries(&wanted, &["malware".into()]).unwrap().len(), 1);
        // same instance: metadata is fresh, no more requests
        assert_eq!(first.filter_entries(&wanted, &["malware".into()]).unwrap().len(), 1);
        assert_eq!(server.requests().len(), 3);
        // a new instance revalidates the cached summary and metadata (packages.json is fresh enough)
        let second = repo_with(&server, &config, json!({}));
        assert_eq!(second.filter_entries(&wanted, &["malware".into()]).unwrap().len(), 1);
        assert_eq!(server.paths()[3..], ["/lists/all/summary.json", "/p2/evil/pkg.json"]);
    }

    #[test]
    fn filter_api_url() {
        let server = TestServer::new();
        server.route(
            "/packages.json",
            200,
            &[],
            &filter_root(json!({"metadata": true, "lists": {"malware": {"enabled": true}}, "api-url": "/api/filter"})),
        );
        let mut api_entry = entry("ID-api");
        api_entry["package"] = "evil/pkg".into();
        server.route("/api/filter", 200, &[], &json!({"filter": {"malware": [api_entry]}}).to_string());
        server.route("/p2/evil/pkg.json", 200, &[], &json!({"filter": {"malware": [entry("ID-meta")]}}).to_string());
        let repo = repo(&server, json!({}));
        let wanted = [("evil/pkg".to_string(), eq("1.0.0.0")), ("safe/pkg".to_string(), eq("1.0.0.0"))];
        let entries = repo.filter_entries(&wanted, &["malware".into()]).unwrap();
        assert_eq!(entries[0].id.as_deref(), Some("ID-api"));
        let post = server.requests().into_iter().find(|r| r.method == "POST").unwrap();
        assert_eq!(post.body, r#"{"packages":["pkg:\/\/composer\/evil\/pkg","pkg:\/\/composer\/safe\/pkg"],"lists":["malware"]}"#);
        assert_eq!(post.header("content-type"), Some("application/json"));

        // once package metadata was loaded in this run, the api is skipped
        let names = vec![("evil/pkg".to_string(), None)];
        let acceptable: HashSet<Stability> = [Stability::Stable].into();
        repo.load_packages(&LoadRequest {
            names: &names,
            acceptable_stabilities: &acceptable,
            stability_flags: &HashMap::new(),
            already_loaded: &HashMap::new(),
        })
        .unwrap();
        let entries = repo.filter_entries(&wanted[..1], &["malware".into()]).unwrap();
        assert_eq!(entries[0].id.as_deref(), Some("ID-meta"));
    }

    #[test]
    #[ignore = "network: talks to repo.packagist.org"]
    fn live_packagist_monolog() {
        let config = test_config();
        let http = HttpClient::new(Arc::new(Io::null()), &config).unwrap();
        let repo = ComposerRepository::new(&json!({"type": "composer", "url": "https://repo.packagist.org"}), http).unwrap();
        let result = load(&repo, &["monolog/monolog"], &[Stability::Stable]);
        assert_eq!(result.names_found, ["monolog/monolog"]);
        let v1 = result.packages.iter().find(|p| p.pretty_version == "1.0.0").expect("monolog 1.0.0");
        assert_eq!(v1.dist.as_ref().unwrap().kind, "zip");
        assert!(repo.has_security_advisories());
        let advisories = repo.security_advisories(&[("monolog/monolog".into(), eq("1.10.0.0"))], false).unwrap();
        assert!(advisories.advisories.get("monolog/monolog").is_some_and(|a| !a.is_empty()));
    }
}
