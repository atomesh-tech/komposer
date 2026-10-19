//! `HttpDownloader` + `CurlDownloader` + `AuthHelper::promptAuthIfNeeded()` on top of reqwest.
//!
//! The API is synchronous; requests run on a tokio runtime owned by the client, so batches
//! ([`HttpClient::get_many`], [`HttpClient::download_many`]) run concurrently, bounded like
//! Composer's `COMPOSER_MAX_PARALLEL_HTTP` (24 by default here, Composer uses 12; HTTP/2 multiplexes them). Composer never prompts here: where it
//! would ask for credentials interactively it prints the explanation and fails.

use crate::auth::{
    AuthStore, BITBUCKET_OAUTH2_ACCESS_TOKEN_URL, bitbucket_authorize_oauth, github_authorize_oauth, github_is_rate_limited, github_rate_limit,
    github_requires_sso, github_sso_url, gitlab_authorize_oauth, is_public_bitbucket_download, string_list,
};
use crate::url::sanitize;
use anyhow::{Context, Result, anyhow, bail};
use komposer_package::config::Config;
use komposer_package::io::{Io, Verbosity};
use komposer_semver::{Constraint, Op};
use regex::Regex;
use serde_json::Value;
use std::collections::HashSet;
use std::fmt;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, LazyLock, Mutex, OnceLock};
use std::task::Poll;
use std::time::Duration;
use tokio::io::AsyncWriteExt;

const MAX_REDIRECTS: u32 = 20;
const MAX_RETRIES: u32 = 3;
/// How long a metadata GET may wait for a response before the same request is sent a second time.
const HEDGE_AFTER: Duration = Duration::from_secs(1);

/// `TransportException`: a failed request, with the HTTP status when there was a response
/// (`499` when the network is disabled by `COMPOSER_DISABLE_NETWORK`).
#[derive(Clone, Debug)]
pub struct TransportError {
    pub message: String,
    pub status: Option<u16>,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
}

impl TransportError {
    pub fn new(message: impl Into<String>, status: Option<u16>) -> TransportError {
        TransportError { message: message.into(), status, headers: vec![], body: None }
    }
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for TransportError {}

/// The HTTP status of a [`TransportError`] anywhere in `err`'s chain.
pub fn error_status(err: &anyhow::Error) -> Option<u16> {
    err.chain().find_map(|e| e.downcast_ref::<TransportError>()).and_then(|e| e.status)
}

/// Per-request options: the subset of PHP stream context options Composer passes around.
#[derive(Clone, Debug)]
pub struct RequestOptions {
    /// `None` is GET
    pub method: Option<String>,
    /// `Name: value` lines
    pub headers: Vec<String>,
    pub body: Option<Vec<u8>>,
    pub timeout: Option<Duration>,
    /// `ssl.verify_peer`
    pub verify_peer: bool,
    /// `retry-auth-failure`
    pub retry_auth_failure: bool,
}

impl Default for RequestOptions {
    fn default() -> RequestOptions {
        RequestOptions { method: None, headers: vec![], body: None, timeout: None, verify_peer: true, retry_auth_failure: true }
    }
}

impl RequestOptions {
    /// From a repository's `options` (stream context options: `http.header`, `http.timeout`,
    /// `http.method`, `http.content`, `ssl.verify_peer`...).
    pub fn from_context(options: &Value) -> RequestOptions {
        let mut opts = RequestOptions::default();
        let http = options.get("http");
        match http.and_then(|h| h.get("header")) {
            Some(Value::String(s)) => opts.headers = s.split("\r\n").filter(|h| !h.is_empty()).map(String::from).collect(),
            Some(Value::Array(a)) => opts.headers = a.iter().filter_map(|h| h.as_str().map(String::from)).collect(),
            _ => {}
        }
        opts.timeout = http.and_then(|h| h.get("timeout")).and_then(Value::as_f64).map(Duration::from_secs_f64);
        opts.method = http.and_then(|h| h.get("method")).and_then(Value::as_str).map(String::from);
        opts.body = http.and_then(|h| h.get("content")).and_then(Value::as_str).map(|s| s.as_bytes().to_vec());
        let ssl = options.get("ssl");
        let disabled = |k: &str| ssl.and_then(|s| s.get(k)).is_some_and(|v| v == &Value::Bool(false));
        opts.verify_peer =
            !(disabled("verify_peer") || disabled("verify_peer_name") || ssl.and_then(|s| s.get("allow_self_signed")) == Some(&Value::Bool(true)));
        opts
    }

    pub fn header(mut self, header: impl Into<String>) -> RequestOptions {
        self.headers.push(header.into());
        self
    }

    fn is_get(&self) -> bool {
        self.method.as_deref().is_none_or(|m| m.eq_ignore_ascii_case("GET"))
    }

    fn has_header(&self, name: &str) -> bool {
        let prefix = format!("{}:", name.to_ascii_lowercase());
        self.headers.iter().any(|h| h.to_ascii_lowercase().starts_with(&prefix))
    }
}

/// `Composer\Util\Http\Response`.
#[derive(Clone, Debug)]
pub struct Response {
    pub url: String,
    pub status: u16,
    /// `HTTP/1.1 404 Not Found`, `HTTP/2 404 `
    pub status_line: String,
    pub headers: Vec<(String, String)>,
    /// Empty for downloads written to disk.
    pub body: Vec<u8>,
}

impl Response {
    pub(crate) fn fake(url: &str, status: u16) -> Response {
        Response { url: url.into(), status, status_line: format!("HTTP/1.1 {status}"), headers: vec![], body: vec![] }
    }

    /// `getHeader()`: the last value of a header, case-insensitive.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().rev().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// `decodeJson()` with PHP array semantics; the body is never echoed in errors.
    pub fn json(&self) -> Result<Value> {
        komposer_package::json::parse(&self.text()).map_err(|_| anyhow!("\"{}\" does not contain valid JSON", sanitize(&self.url)))
    }

    fn is_json(&self) -> bool {
        matches!(self.header("content-type").map(str::to_ascii_lowercase).as_deref(), Some("application/json" | "application/json; charset=utf-8"))
    }
}

/// `HttpDownloader`: a shared, cheaply clonable HTTP client with Composer's auth, retry,
/// redirect and `secure-http` behavior.
#[derive(Clone)]
pub struct HttpClient {
    rt: Arc<tokio::runtime::Runtime>,
    shared: Arc<Shared>,
}

struct Shared {
    client: reqwest::Client,
    insecure: OnceLock<reqwest::Client>,
    io: Arc<Io>,
    config: Config,
    auth: AuthStore,
    github_domains: Vec<String>,
    gitlab_domains: Vec<String>,
    disabled: bool,
    jobs: tokio::sync::Semaphore,
    user_agent: String,
    warned_hosts: Mutex<HashSet<String>>,
    bitbucket_retry: Mutex<HashSet<String>>,
}

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

impl HttpClient {
    /// `Factory::createHttpDownloader()`; credentials come from `config` like `IO::loadConfiguration()`.
    pub fn new(io: Arc<Io>, config: &Config) -> Result<HttpClient> {
        let mut config = config.clone();
        let auth = AuthStore::from_config(&mut config, &io);
        let max_jobs = std::env::var("COMPOSER_MAX_PARALLEL_HTTP").ok().and_then(|v| v.parse::<usize>().ok()).map_or(24, |n| n.clamp(1, 50));
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).thread_name("komposer-http").enable_all().build()?;
        let shared = Shared {
            client: build_client(&config, false)?,
            insecure: OnceLock::new(),
            io,
            github_domains: string_list(&config, "github-domains"),
            gitlab_domains: string_list(&config, "gitlab-domains"),
            config,
            auth,
            disabled: std::env::var("COMPOSER_DISABLE_NETWORK").is_ok_and(|v| !v.is_empty() && v != "0"),
            jobs: tokio::sync::Semaphore::new(max_jobs),
            user_agent: user_agent(),
            warned_hosts: Mutex::default(),
            bitbucket_retry: Mutex::default(),
        };
        Ok(HttpClient { rt: Arc::new(rt), shared: Arc::new(shared) })
    }

    pub fn io(&self) -> &Arc<Io> {
        &self.shared.io
    }

    /// The config, including domains implicitly added for configured credentials.
    pub fn config(&self) -> &Config {
        &self.shared.config
    }

    pub fn auth(&self) -> &AuthStore {
        &self.shared.auth
    }

    pub fn github_domains(&self) -> &[String] {
        &self.shared.github_domains
    }

    pub fn gitlab_domains(&self) -> &[String] {
        &self.shared.gitlab_domains
    }

    /// `Url::getOrigin()` with this client's `gitlab-domains`.
    pub fn origin(&self, url: &str) -> String {
        crate::url::origin(&self.shared.gitlab_domains, url)
    }

    /// `COMPOSER_DISABLE_NETWORK` is set.
    pub fn network_disabled(&self) -> bool {
        self.shared.disabled
    }

    /// `Config::prohibitUrlByConfig()`: enforces `secure-http`, warns about insecure access.
    pub fn prohibit_url_by_config(&self, url: &str, opts: Option<&RequestOptions>) -> Result<()> {
        self.shared.prohibit_url_by_config(url, opts)
    }

    /// `HttpDownloader::get()`.
    pub fn get(&self, url: &str, opts: &RequestOptions) -> Result<Response> {
        let shared = self.shared.clone();
        let (url, opts) = (url.to_string(), opts.clone());
        self.rt.block_on(async move { shared.fetch(url, opts, None).await })
    }

    /// Several `HttpDownloader::add()` + `wait()`: concurrent, results in request order.
    pub fn get_many(&self, requests: Vec<(String, RequestOptions)>) -> Vec<Result<Response>> {
        self.run_all(requests.into_iter().map(|(url, opts)| (url, opts, None)).collect())
    }

    /// `HttpDownloader::copy()`: streams the body to `path` (through `path~`).
    pub fn download_to(&self, url: &str, path: &Path, opts: &RequestOptions) -> Result<Response> {
        let shared = self.shared.clone();
        let (url, opts, path) = (url.to_string(), opts.clone(), path.to_path_buf());
        self.rt.block_on(async move { shared.fetch(url, opts, Some(path)).await })
    }

    /// Several `HttpDownloader::addCopy()`: concurrent, results in request order.
    pub fn download_many(&self, requests: Vec<(String, PathBuf, RequestOptions)>) -> Vec<Result<Response>> {
        self.run_all(requests.into_iter().map(|(url, path, opts)| (url, opts, Some(path))).collect())
    }

    /// `Bitbucket::requestToken()`: exchanges OAuth consumer credentials for an access token.
    pub(crate) fn bitbucket_request_token(&self, origin: &str, key: &str, secret: &str) -> Result<String> {
        let shared = self.shared.clone();
        let (origin, key, secret) = (origin.to_string(), key.to_string(), secret.to_string());
        self.rt.block_on(async move { shared.bitbucket_request_token(&origin, &key, &secret).await })
    }

    fn run_all(&self, requests: Vec<(String, RequestOptions, Option<PathBuf>)>) -> Vec<Result<Response>> {
        let tasks: Vec<_> = requests
            .into_iter()
            .map(|(url, opts, path)| {
                let shared = self.shared.clone();
                self.rt.spawn(async move { shared.fetch(url, opts, path).await })
            })
            .collect();
        self.rt.block_on(async move {
            let mut out = Vec::with_capacity(tasks.len());
            for task in tasks {
                out.push(task.await.unwrap_or_else(|e| Err(anyhow!("HTTP task failed: {e}"))));
            }
            out
        })
    }
}

fn build_client(config: &Config, insecure: bool) -> Result<reqwest::Client> {
    // ring instead of rustls' default aws-lc-rs, which is a few times larger; installing again is a no-op error
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10))
        // Composer only has the 300s request timeout; a stalled connection or response is retried instead
        .read_timeout(Duration::from_secs(30))
        .http2_keep_alive_interval(Duration::from_secs(5))
        .http2_keep_alive_timeout(Duration::from_secs(10))
        .tls_danger_accept_invalid_certs(insecure);
    if let Some(cafile) = config.get_string("cafile").filter(|f| !f.is_empty()) {
        let pem = std::fs::read(&cafile).map_err(|_| TransportError::new("The configured cafile was not valid or could not be read.", None))?;
        let certs = reqwest::Certificate::from_pem_bundle(&pem)
            .map_err(|_| TransportError::new("The configured cafile was not valid or could not be read.", None))?;
        builder = builder.tls_certs_merge(certs);
    }
    Ok(builder.build()?)
}

/// `Composer/<version> (<os>; <release>; ...)`: Packagist relies on the `Composer/` prefix.
fn user_agent() -> String {
    let uname = |flag: &str| {
        let mut cmd = std::process::Command::new("uname");
        cmd.arg(flag);
        let out = crate::util::execute(cmd, None);
        if out.ok() { out.stdout.trim().to_string() } else { "Unknown".into() }
    };
    let (os, release) = if cfg!(windows) { ("Windows NT".to_string(), "Unknown".to_string()) } else { (uname("-s"), uname("-r")) };
    let ci = if std::env::var("CI").is_ok_and(|v| !v.is_empty() && v != "0") { "; CI" } else { "" };
    format!("Composer/{} ({os}; {release}; komposer {}; reqwest{ci})", crate::COMPOSER_VERSION, env!("CARGO_PKG_VERSION"))
}

impl Shared {
    fn origin(&self, url: &str) -> String {
        crate::url::origin(&self.gitlab_domains, url)
    }

    fn prohibit_url_by_config(&self, url: &str, opts: Option<&RequestOptions>) -> Result<()> {
        static VALID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^[a-z][a-z0-9+.-]*://[^/?#\s]+").unwrap());
        static HTTP: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^https?://").unwrap());
        if !VALID.is_match(url) && !HTTP.is_match(url) {
            return Ok(());
        }
        let parts = crate::url::parse(url);
        let scheme = parts.scheme.unwrap_or("").to_ascii_lowercase();
        let host = parts.host.map(String::from);
        if matches!(scheme.as_str(), "http" | "git" | "ftp" | "svn") {
            if self.config.get_bool("secure-http") {
                if scheme == "svn" {
                    if host.as_ref().is_some_and(|h| string_list(&self.config, "secure-svn-domains").contains(h)) {
                        return Ok(());
                    }
                    bail!(TransportError::new(
                        format!(
                            "Your configuration does not allow connections to {}. See https://getcomposer.org/doc/06-config.md#secure-svn-domains for details.",
                            sanitize(url)
                        ),
                        None
                    ));
                }
                bail!(TransportError::new(
                    format!(
                        "Your configuration does not allow connections to {}. See https://getcomposer.org/doc/06-config.md#secure-http for details.",
                        sanitize(url)
                    ),
                    None
                ));
            }
            if let Some(host) = &host
                && self.warned_hosts.lock().unwrap().insert(format!("{scheme}:{host}"))
            {
                self.io.warning(&format!("Warning: Accessing {host} over {scheme} which is an insecure protocol."));
            }
        }
        if let (Some(host), Some(opts)) = (&host, opts)
            && !opts.verify_peer
            && self.warned_hosts.lock().unwrap().insert(format!("ssl:{host}"))
        {
            self.io.warning(&format!("Warning: Accessing {host} with verify_peer disabled."));
        }
        Ok(())
    }

    fn fetch(self: Arc<Self>, url: String, opts: RequestOptions, copy_to: Option<PathBuf>) -> BoxFuture<'static, Result<Response>> {
        Box::pin(async move { self.fetch_inner(url, opts, copy_to).await })
    }

    async fn fetch_inner(self: Arc<Self>, mut url: String, opts: RequestOptions, copy_to: Option<PathBuf>) -> Result<Response> {
        static URL_CREDS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^https?://([^:/]+):([^@/]+)@([^/]+)").unwrap());
        static IS_HTTP: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^https?://").unwrap());
        if url.is_empty() {
            bail!("$url must not be an empty string");
        }
        // capture username/password from the URL if there is one
        if let Some(c) = URL_CREDS.captures(&url) {
            self.auth.set(&self.origin(&url), &crate::util::rawurldecode(&c[1]), Some(&crate::util::rawurldecode(&c[2])));
        }
        if !IS_HTTP.is_match(&url) {
            return read_local(&url, copy_to.as_deref());
        }
        if self.disabled {
            if opts.has_header("if-modified-since") {
                return Ok(Response::fake(&url, 304));
            }
            bail!(TransportError::new(format!("Network disabled, request canceled: {}", sanitize(&url)), Some(499)));
        }

        let (mut redirects, mut retries) = (0, 0);
        loop {
            let origin = self.origin(&url);
            self.prohibit_url_by_config(&url, Some(&opts))?;
            if redirects == 0 && retries == 0 {
                let if_modified = if opts.has_header("if-modified-since") { " if modified" } else { "" };
                self.io.write_error_at(&format!("Downloading {}{if_modified}", sanitize(&url)), Verbosity::Debug);
            }
            let response = {
                let _permit = self.jobs.acquire().await?;
                self.send(&url, &origin, &opts, copy_to.as_deref()).await
            };
            let response = match response {
                Ok(r) => r,
                Err(e) if opts.is_get() && retries < MAX_RETRIES => {
                    retries += 1;
                    self.io.write_error_at(&format!("Retrying ({retries}) {} due to {e}", sanitize(&url)), Verbosity::Debug);
                    retry_delay(retries).await;
                    continue;
                }
                Err(e) => {
                    if let Some(path) = &copy_to {
                        let _ = std::fs::remove_file(tilde(path));
                    }
                    bail!(TransportError::new(format!("Transfer error while downloading {}: {e:#}", sanitize(&url)), None))
                }
            };
            let status = response.status;
            self.io.write_error_at(&format!("[{status}] {}", sanitize(&url)), Verbosity::Debug);

            let warnings = status >= 300 && response.is_json() && response.json().is_ok_and(|data| output_warnings(&self.io, &origin, &data));
            if self.authenticated_retry_needed(&url, &origin, &response, &opts, retries).await? {
                retries += 1;
                continue;
            }
            // 3xx redirects, 304 Not Modified excluded
            if (300..400).contains(&status) && status != 304 && redirects < MAX_REDIRECTS {
                url = self.redirect_target(&url, &response, redirects)?;
                redirects += 1;
                continue;
            }
            if (400..600).contains(&status) {
                let retryable = matches!(status, 423 | 425 | 500 | 502 | 503 | 504 | 507 | 510)
                    // codeload.github.com intermittently returns 400 on reused connections
                    || (status == 400 && crate::url::parse(&url).host == Some("codeload.github.com"));
                if retryable && opts.is_get() && retries < MAX_RETRIES {
                    retries += 1;
                    self.io.write_error_at(&format!("Retrying ({retries}) {} due to status code {status}", sanitize(&url)), Verbosity::Debug);
                    retry_delay(retries).await;
                    continue;
                }
                let message = response.status_line.clone();
                return Err(fail_response(&url, response, &message, warnings, copy_to.as_deref()));
            }
            if let Some(path) = &copy_to {
                tokio::fs::rename(tilde(path), path).await.with_context(|| format!("Could not write {}", path.display()))?;
            }
            return Ok(response);
        }
    }

    /// One request without redirects; `copy_to` bodies of successful responses go to `path~`.
    async fn send(&self, url: &str, origin: &str, opts: &RequestOptions, copy_to: Option<&Path>) -> Result<Response> {
        let client = if opts.verify_peer {
            &self.client
        } else {
            self.insecure.get_or_init(|| build_client(&self.config, true).unwrap_or_else(|_| self.client.clone()))
        };
        let method = reqwest::Method::from_bytes(opts.method.as_deref().unwrap_or("GET").to_ascii_uppercase().as_bytes())?;
        let mut req = client.request(method, url).timeout(opts.timeout.unwrap_or(Duration::from_secs(300)));
        let mut headers = opts.headers.clone();
        headers.extend(self.auth.headers(&self.io, &self.gitlab_domains, origin, url));
        if !opts.has_header("user-agent") {
            headers.push(format!("User-Agent: {}", self.user_agent));
        }
        if opts.body.is_some() && !opts.has_header("content-type") {
            headers.push("Content-Type: application/x-www-form-urlencoded".into());
        }
        for h in &headers {
            if let Some((name, value)) = h.split_once(':') {
                req = req.header(name.trim(), value.trim());
            }
        }
        if let Some(body) = &opts.body {
            req = req.body(body.clone());
        }
        let mut res = match req.try_clone() {
            Some(again) if copy_to.is_none() && opts.is_get() => self.hedged(url, req.send(), again).await?,
            _ => req.send().await?,
        };
        let status = res.status();
        let status_line = match res.version() {
            reqwest::Version::HTTP_2 => format!("HTTP/2 {} ", status.as_u16()),
            v => format!("{v:?} {} {}", status.as_u16(), status.canonical_reason().unwrap_or("")),
        };
        let headers: Vec<(String, String)> =
            res.headers().iter().map(|(k, v)| (k.as_str().to_string(), String::from_utf8_lossy(v.as_bytes()).into_owned())).collect();
        let mut body = vec![];
        match copy_to {
            Some(path) if status.as_u16() < 300 => {
                if let Some(dir) = path.parent() {
                    tokio::fs::create_dir_all(dir).await?;
                }
                let tmp = tilde(path);
                let mut file = tokio::fs::File::create(&tmp).await.map_err(|e| {
                    TransportError::new(format!("The \"{}\" file could not be written to {}: {e}", sanitize(url), path.display()), None)
                })?;
                while let Some(chunk) = res.chunk().await? {
                    file.write_all(&chunk).await?;
                }
                file.flush().await?;
            }
            _ => body = res.bytes().await?.to_vec(),
        }
        Ok(Response { url: url.to_string(), status: status.as_u16(), status_line, headers, body })
    }

    /// `first`, unless it has no response after [`HEDGE_AFTER`]: requests get stuck at the CDN while their
    /// siblings take milliseconds, and the same request sent `again` usually gets through right away.
    async fn hedged(
        &self,
        url: &str,
        first: impl Future<Output = reqwest::Result<reqwest::Response>>,
        again: reqwest::RequestBuilder,
    ) -> reqwest::Result<reqwest::Response> {
        let mut first = std::pin::pin!(first);
        if let Ok(response) = tokio::time::timeout(HEDGE_AFTER, &mut first).await {
            return response;
        }
        self.io.write_error_at(&format!("Sending {} again as it is taking long", sanitize(url)), Verbosity::Debug);
        let mut second = std::pin::pin!(again.send());
        std::future::poll_fn(|cx| match first.as_mut().poll(cx) {
            Poll::Ready(response) => Poll::Ready(response),
            Poll::Pending => second.as_mut().poll(cx),
        })
        .await
    }

    /// `CurlDownloader::handleRedirect()`.
    fn redirect_target(&self, url: &str, response: &Response, redirects: u32) -> Result<String> {
        let Some(location) = response.header("location").filter(|l| !l.is_empty()) else {
            bail!(TransportError::new(
                format!("The \"{}\" file could not be downloaded, got redirect without Location ({})", sanitize(url), response.status_line),
                Some(response.status)
            ));
        };
        let target = reqwest::Url::parse(url).and_then(|base| base.join(location)).map(String::from).unwrap_or_else(|_| location.to_string());
        if !crate::url::is_allowed_redirect(&target) {
            bail!(TransportError::new(
                format!("Could not follow the redirect to \"{}\" because only http and https redirects are supported.", sanitize(&target)),
                None
            ));
        }
        self.io.write_error_at(&format!("Following redirect ({}) {}", redirects + 1, sanitize(&target)), Verbosity::Debug);
        Ok(target)
    }

    /// `CurlDownloader::isAuthenticatedRetryNeeded()`.
    async fn authenticated_retry_needed(
        self: &Arc<Self>,
        url: &str,
        origin: &str,
        response: &Response,
        opts: &RequestOptions,
        retries: u32,
    ) -> Result<bool> {
        static HTML: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^text/html\b").unwrap());
        if matches!(response.status, 401 | 403) && opts.retry_auth_failure {
            let reason = Some(response.status_line.as_str());
            if self.prompt_auth_if_needed(url, origin, response.status, reason, &response.headers, retries, Some(&response.body)).await? {
                return Ok(true);
            }
        }
        let location = response.header("location");
        let mut needs_auth = None;
        if origin == "bitbucket.org"
            && !is_public_bitbucket_download(url)
            && url.ends_with(".zip")
            && location.is_none_or(|l| !l.ends_with(".zip"))
            && response.header("content-type").is_some_and(|c| HTML.is_match(c))
        {
            needs_auth = Some("Bitbucket requires authentication and it was not provided");
        }
        if response.status == 404 && self.gitlab_domains.iter().any(|d| d == origin) && url.contains("archive.zip") {
            needs_auth = Some("GitLab requires authentication and it was not provided");
        }
        let Some(message) = needs_auth else { return Ok(false) };
        if opts.retry_auth_failure && self.prompt_auth_if_needed(url, origin, 401, None, &[], retries, None).await? {
            return Ok(true);
        }
        Err(fail_response(url, response.clone(), message, false, None))
    }

    /// `AuthHelper::promptAuthIfNeeded()` for a non-interactive console. Returns whether to retry.
    #[allow(clippy::too_many_arguments)]
    async fn prompt_auth_if_needed(
        self: &Arc<Self>,
        url: &str,
        origin: &str,
        status: u16,
        reason: Option<&str>,
        headers: &[(String, String)],
        retry_count: u32,
        body: Option<&[u8]>,
    ) -> Result<bool> {
        let surl = sanitize(url);
        let cannot_authenticate =
            |origin: &str| -> anyhow::Error { TransportError::new(format!("Could not authenticate against {origin}"), Some(401)).into() };
        if self.github_domains.iter().any(|d| d == origin) {
            // concurrent requests retry once with credentials another request obtained
            if self.auth.has(origin) && retry_count == 0 {
                return Ok(true);
            }
            if github_requires_sso(headers) {
                let sso = sanitize(&github_sso_url(headers).unwrap_or_default());
                self.io.write_error(&format!("GitHub API token requires SSO authorization. Authorize this token at {sso}\n"));
                bail!(TransportError::new(format!("Could not authenticate against {origin}"), Some(403)));
            }
            let message = if github_is_rate_limited(headers) {
                let (limit, reset) = github_rate_limit(headers);
                let hint = if self.auth.has(origin) {
                    "Review your configured GitHub OAuth token or enter a new one to go over the API rate limit."
                } else {
                    "Create a GitHub OAuth token to go over the API rate limit."
                };
                let limit = limit.parse::<i64>().unwrap_or(0);
                format!(
                    "GitHub API limit ({limit} calls/hr) is exhausted, could not fetch {surl}. {hint} You can also wait until {reset} for the rate limit to reset.\n"
                )
            } else {
                let api_message = body
                    .and_then(|b| serde_json::from_slice::<Value>(b).ok())
                    .and_then(|v| v.get("message").and_then(Value::as_str).map(String::from));
                match api_message {
                    Some(m) => format!("\nCould not fetch {surl}: {m}"),
                    None if self.auth.has(origin) => format!(
                        "\nCould not fetch {surl}, please review your configured GitHub OAuth token or enter a new one to access private repos"
                    ),
                    None => format!("\nCould not fetch {surl}, please create a GitHub OAuth token to access private repos"),
                }
            };
            // a token that changes nothing would retry forever
            let before = self.auth.get(origin);
            if !github_authorize_oauth(&self.auth, &self.github_domains, origin) || self.auth.get(origin) == before {
                self.io.write_error(&message);
                return Err(cannot_authenticate(origin));
            }
            return Ok(true);
        }
        if self.gitlab_domains.iter().any(|d| d == origin) {
            let message = format!(
                "\nCould not fetch {surl}, enter your {origin} credentials {}",
                if status == 401 { "to access private repos" } else { "to go over the API rate limit" }
            );
            let invalid = || -> anyhow::Error { TransportError::new(format!("Invalid credentials for '{surl}', aborting."), Some(status)).into() };
            let auth = self.auth.get(origin);
            if auth.as_ref().is_some_and(|a| matches!(a.password.as_deref(), Some("gitlab-ci-token" | "private-token" | "oauth2"))) {
                return Err(invalid());
            }
            if !gitlab_authorize_oauth(&self.auth, &self.config, &self.gitlab_domains, origin) {
                self.io.write_error(&message);
                return Err(cannot_authenticate(origin));
            }
            if auth.is_some() && self.auth.get(origin) == auth {
                return Err(invalid());
            }
            return Ok(true);
        }
        if matches!(origin, "bitbucket.org" | "api.bitbucket.org") {
            let origin = "bitbucket.org";
            let mut ask_for_token = true;
            if let Some(auth) = self.auth.get(origin) {
                if auth.username != "x-token-auth" {
                    let token = self.bitbucket_request_token(origin, &auth.username, auth.password.as_deref().unwrap_or("")).await?;
                    if !token.is_empty() {
                        self.auth.set(origin, "x-token-auth", Some(&token));
                        ask_for_token = false;
                    }
                } else if self.bitbucket_retry.lock().unwrap().insert(url.to_string()) {
                    // concurrent requests all fail before the first one fixes the token
                    ask_for_token = false;
                } else {
                    return Err(cannot_authenticate(origin));
                }
            }
            if ask_for_token {
                let message = format!(
                    "\nCould not fetch {surl}, please create a bitbucket OAuth token to {}",
                    if matches!(status, 401 | 403) { "access private repos" } else { "go over the API rate limit" }
                );
                if !bitbucket_authorize_oauth(&self.auth, origin) {
                    self.io.write_error(&message);
                    return Err(cannot_authenticate(origin));
                }
            }
            return Ok(true);
        }
        // 404s are only handled for github
        if status == 404 {
            return Ok(false);
        }
        let reason = reason.unwrap_or("");
        let message = match status {
            401 => format!("The '{surl}' URL required authentication (HTTP 401).\nYou must be using the interactive console to authenticate"),
            403 => format!("The '{surl}' URL could not be accessed (HTTP 403): {reason}"),
            _ => format!("Unknown error code '{status}', reason: {reason}"),
        };
        bail!(TransportError::new(message, Some(status)))
    }

    /// `Bitbucket::requestToken()` with an OAuth consumer; an empty string when it was rejected.
    async fn bitbucket_request_token(self: &Arc<Self>, origin: &str, key: &str, secret: &str) -> Result<String> {
        let configured = self.config.raw("bitbucket-oauth").and_then(|c| c.get(origin));
        if let Some(token) = configured.and_then(|c| c.get("access-token")).and_then(Value::as_str)
            && configured.and_then(|c| c.get("access-token-expiration")).and_then(Value::as_i64).is_some_and(|exp| exp >= crate::util::now_ts())
        {
            return Ok(token.to_string());
        }
        self.auth.set(origin, key, Some(secret));
        let opts = RequestOptions {
            method: Some("POST".into()),
            body: Some(b"grant_type=client_credentials".to_vec()),
            retry_auth_failure: false,
            ..Default::default()
        };
        match self.clone().fetch(BITBUCKET_OAUTH2_ACCESS_TOKEN_URL.into(), opts, None).await {
            Ok(response) => {
                let token = response.json()?;
                match (token.get("access_token").and_then(Value::as_str), token.get("expires_in")) {
                    (Some(t), Some(_)) => Ok(t.to_string()),
                    _ => bail!("Expected a token configured with expires_in and access_token present, got {}", response.text()),
                }
            }
            Err(e) => match error_status(&e) {
                Some(400) => {
                    self.io.error("Invalid OAuth consumer provided.");
                    self.io.write_error("This can have three reasons:");
                    self.io.write_error("1. You are authenticating with a bitbucket username/password combination");
                    self.io.write_error("2. You are using an OAuth consumer, but didn't configure a (dummy) callback url");
                    self.io.write_error("3. You are using an OAuth consumer, but didn't configure it as private consumer");
                    Ok(String::new())
                }
                Some(401 | 403) => {
                    self.io.error("Invalid OAuth consumer provided.");
                    self.io.write_error("You can also add it manually later by using \"composer config --global --auth bitbucket-oauth.bitbucket.org <consumer-key> <consumer-secret>\"");
                    Ok(String::new())
                }
                _ => Err(e),
            },
        }
    }
}

/// `restartJobWithDelay()` backoff of Composer 2.10: none, 100ms, then 500ms.
async fn retry_delay(retries: u32) {
    let ms = match retries {
        0 | 1 => return,
        2 => 100,
        _ => 500,
    };
    tokio::time::sleep(Duration::from_millis(ms)).await;
}

fn tilde(path: &Path) -> PathBuf {
    PathBuf::from(format!("{}~", path.display()))
}

/// `CurlDownloader::failResponse()`.
fn fail_response(url: &str, response: Response, message: &str, warnings_output: bool, copy_to: Option<&Path>) -> anyhow::Error {
    if let Some(path) = copy_to {
        let _ = std::fs::remove_file(tilde(path));
    }
    let mut details = String::new();
    // skip dumping the raw JSON body when the warnings were already presented
    if !warnings_output && response.is_json() {
        let body = String::from_utf8_lossy(&response.body);
        let cut: String = body.chars().take(200).collect();
        details = format!(":\n{cut}{}", if body.len() > 200 { "..." } else { "" });
    }
    let message = format!("The \"{}\" file could not be downloaded ({message}){details}", sanitize(url));
    TransportError { message, status: Some(response.status), headers: response.headers, body: Some(response.body) }.into()
}

/// `file://` and plain paths, which Composer reads through `RemoteFilesystem`.
fn read_local(url: &str, copy_to: Option<&Path>) -> Result<Response> {
    let path = crate::util::platform_path(url);
    let fail = |e: std::io::Error| -> anyhow::Error {
        let status = (e.kind() == std::io::ErrorKind::NotFound).then_some(404);
        TransportError::new(format!("The \"{}\" file could not be downloaded: {e}", sanitize(url)), status).into()
    };
    let body = match copy_to {
        Some(target) => {
            std::fs::copy(&path, target).map_err(fail)?;
            vec![]
        }
        None => std::fs::read(&path).map_err(fail)?,
    };
    Ok(Response { url: url.into(), status: 200, status_line: "HTTP/1.1 200 OK".into(), headers: vec![], body })
}

/// `HttpDownloader::outputWarnings()` (Composer 2.10): prints `warning`/`info` messages a
/// repository sends, when they apply to this Composer version.
pub fn output_warnings(io: &Io, url: &str, data: &Value) -> bool {
    static ANSI: LazyLock<Regex> = LazyLock::new(|| Regex::new("\x1b\\[[;\\d]*m").unwrap());
    let clean = |msg: &str| if io.is_decorated() { msg.to_string() } else { ANSI.replace_all(msg, "").into_owned() };
    let applies = |versions: &str| {
        let composer = komposer_semver::normalize(crate::COMPOSER_VERSION).unwrap_or_default();
        komposer_semver::parse_constraints(versions).is_ok_and(|c| c.matches(&Constraint::single(Op::Eq, composer)))
    };
    let ucfirst = |t: &str| t[..1].to_uppercase() + &t[1..];
    let mut wrote = false;
    // legacy warning/info keys
    for kind in ["warning", "info"] {
        let Some(msg) = data.get(kind).filter(|m| !komposer_package::loader::php_empty(m)).and_then(Value::as_str) else { continue };
        if let Some(versions) = data.get(format!("{kind}-versions")).filter(|m| !komposer_package::loader::php_empty(m)).and_then(Value::as_str)
            && !applies(versions)
        {
            continue;
        }
        io.write_error(&format!("<{kind}>{} from {}: {}</{kind}>", ucfirst(kind), sanitize(url), clean(msg)));
        wrote = true;
    }
    // Composer 2.2+ format with multiple messages
    for (key, kind) in [("warnings", "warning"), ("infos", "info")] {
        let Some(Value::Array(specs)) = data.get(key) else { continue };
        for spec in specs {
            if !applies(spec.get("versions").and_then(Value::as_str).unwrap_or("")) {
                continue;
            }
            let msg = spec.get("message").and_then(Value::as_str).unwrap_or("");
            io.write_error(&format!("<{kind}>{} from {}: {}</{kind}>", ucfirst(kind), sanitize(url), clean(msg)));
            wrote = true;
        }
    }
    wrote
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestServer, test_config};

    fn client(config: &Config) -> HttpClient {
        HttpClient::new(Arc::new(Io::null()), config).unwrap()
    }

    #[test]
    fn get_many_runs_concurrently_and_keeps_order() {
        let server = TestServer::new();
        for i in 0..30 {
            server.route(&format!("/f{i}"), 200, &[], &format!("body{i}"));
        }
        let http = client(&test_config());
        let reqs = (0..30).map(|i| (format!("{}/f{i}", server.url), RequestOptions::default())).collect();
        let out = http.get_many(reqs);
        for (i, r) in out.into_iter().enumerate() {
            assert_eq!(r.unwrap().text(), format!("body{i}"));
        }
    }

    #[test]
    fn follows_redirects_and_reports_failures() {
        let server = TestServer::new();
        server.route("/old", 302, &[("Location", "/new")], "");
        server.route("/new", 200, &[("Last-Modified", "Tue, 01 Jan 2030 00:00:00 GMT")], "ok");
        server.route("/json404", 404, &[("Content-Type", "application/json")], r#"{"error":"nope"}"#);
        let http = client(&test_config());
        let r = http.get(&format!("{}/old", server.url), &RequestOptions::default()).unwrap();
        assert_eq!((r.status, r.text().as_str()), (200, "ok"));
        assert_eq!(r.header("last-modified"), Some("Tue, 01 Jan 2030 00:00:00 GMT"));

        let err = http.get(&format!("{}/json404", server.url), &RequestOptions::default()).unwrap_err();
        assert_eq!(error_status(&err), Some(404));
        assert_eq!(
            err.to_string(),
            format!("The \"{}/json404\" file could not be downloaded (HTTP/1.1 404 Not Found):\n{{\"error\":\"nope\"}}", server.url)
        );
    }

    #[test]
    fn retries_server_errors() {
        let server = TestServer::new();
        server.route_times("/flaky", 503, &[], "", 2);
        server.route("/flaky", 200, &[], "fine");
        let http = client(&test_config());
        assert_eq!(http.get(&format!("{}/flaky", server.url), &RequestOptions::default()).unwrap().text(), "fine");
        assert_eq!(server.requests().len(), 3);
    }

    #[test]
    fn sends_stuck_requests_again() {
        let server = TestServer::new();
        server.route_times("/stuck", 0, &[], "", 1);
        server.route("/stuck", 200, &[], "fine");
        let http = client(&test_config());
        let started = std::time::Instant::now();
        assert_eq!(http.get(&format!("{}/stuck", server.url), &RequestOptions::default()).unwrap().text(), "fine");
        assert!(started.elapsed() < Duration::from_secs(5), "waited {:?}", started.elapsed());
        assert_eq!(server.requests().len(), 2);
    }

    #[test]
    fn enforces_secure_http() {
        let http = client(&Config::new(false, None));
        let err = http.get("http://example.org/packages.json", &RequestOptions::default()).unwrap_err();
        assert_eq!(
            err.to_string(),
            "Your configuration does not allow connections to http://example.org/packages.json. See https://getcomposer.org/doc/06-config.md#secure-http for details."
        );
    }

    #[test]
    fn sends_auth_and_user_agent_and_downloads_files() {
        let server = TestServer::new();
        server.route("/dist.zip", 200, &[], "zipdata");
        let origin = server.url.trim_start_matches("http://").to_string();
        let mut config = test_config();
        config.merge_config(&serde_json::json!({"bearer": {origin.clone(): "tok"}}));
        let http = client(&config);
        let dir = crate::test_support::temp_dir("http-dl");
        let target = dir.join("a/b.zip");
        let out = http.download_many(vec![(format!("{}/dist.zip", server.url), target.clone(), RequestOptions::default())]);
        assert_eq!(out[0].as_ref().unwrap().status, 200);
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "zipdata");
        let req = &server.requests()[0];
        assert_eq!(req.header("authorization"), Some("Bearer tok"));
        assert!(req.header("user-agent").unwrap().starts_with("Composer/2.10.3 ("));
    }

    #[test]
    fn unauthorized_without_interaction_fails_like_composer() {
        let server = TestServer::new();
        server.route("/private", 401, &[], "");
        let http = client(&test_config());
        let err = http.get(&format!("{}/private", server.url), &RequestOptions::default()).unwrap_err();
        assert_eq!(error_status(&err), Some(401));
        assert_eq!(
            err.to_string(),
            format!(
                "The '{}/private' URL required authentication (HTTP 401).\nYou must be using the interactive console to authenticate",
                server.url
            )
        );
    }

    /// `AuthHelperTest::testPromptAuthIfNeededGitLabNoAuthChange()`.
    #[test]
    fn rejected_gitlab_tokens_abort() {
        let server = TestServer::new();
        server.route("/api/v4/projects", 401, &[], "");
        let origin = server.url.trim_start_matches("http://").to_string();
        let mut config = test_config();
        config.merge_config(&serde_json::json!({"gitlab-domains": [origin.clone()], "gitlab-token": {origin.clone(): "tok"}}));
        let http = client(&config);
        let url = format!("{}/api/v4/projects", server.url);
        let err = http.get(&url, &RequestOptions::default()).unwrap_err();
        assert_eq!(err.to_string(), format!("Invalid credentials for '{url}', aborting."));
        assert_eq!(error_status(&err), Some(401));
        assert_eq!(server.requests()[0].header("private-token"), Some("tok"));
    }

    #[test]
    fn reads_local_files() {
        let dir = crate::test_support::temp_dir("http-local");
        std::fs::write(dir.join("packages.json"), "{}").unwrap();
        let http = client(&test_config());
        assert_eq!(http.get(&format!("file://{}/packages.json", dir.display()), &RequestOptions::default()).unwrap().text(), "{}");
        let err = http.get(&format!("file://{}/missing.json", dir.display()), &RequestOptions::default()).unwrap_err();
        assert_eq!(error_status(&err), Some(404));
    }

    #[test]
    fn repo_options() {
        let o = RequestOptions::from_context(&serde_json::json!({"http": {"header": ["X-A: 1"], "timeout": 5}, "ssl": {"verify_peer": false}}));
        assert_eq!(o.headers, ["X-A: 1"]);
        assert_eq!(o.timeout, Some(Duration::from_secs(5)));
        assert!(!o.verify_peer);
    }
}
