//! Credentials: `IOInterface`'s authentication store filled by `BaseIO::loadConfiguration()`,
//! `AuthHelper::addAuthenticationOptions()` and the non-interactive token lookups of
//! `Util\GitHub`, `Util\GitLab` and `Util\Bitbucket`.

use crate::url::sanitize_username;
use base64::Engine;
use komposer_package::config::Config;
use komposer_package::io::{Io, Verbosity};
use regex::Regex;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::process::Command;
use std::sync::{LazyLock, Mutex, RwLock};

/// `['username' => ..., 'password' => ...]`; Composer keeps markers like `x-oauth-basic`,
/// `bearer` or `private-token` in the password slot to tell token kinds apart.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Credentials {
    pub username: String,
    pub password: Option<String>,
}

impl Credentials {
    fn password(&self) -> &str {
        self.password.as_deref().unwrap_or("")
    }
}

/// Credentials per origin (`IOInterface::getAuthentication()` and friends), shared by HTTP and git.
#[derive(Default)]
pub struct AuthStore {
    map: RwLock<HashMap<String, Credentials>>,
    displayed: Mutex<HashMap<String, String>>,
}

impl AuthStore {
    /// `BaseIO::loadConfiguration()`. Domains with GitHub/GitLab credentials are added to
    /// `github-domains`/`gitlab-domains` of `config`, like Composer does implicitly.
    pub fn from_config(config: &mut Config, io: &Io) -> AuthStore {
        let store = AuthStore::default();
        let section = |config: &Config, key: &str| match config.raw(key) {
            Some(Value::Object(m)) => m.clone(),
            _ => Default::default(),
        };
        let set = |domain: &str, user: &str, pass: Option<&str>| {
            if let Some(existing) = store.get(domain)
                && (existing.username != user || existing.password.as_deref() != pass)
            {
                io.warning(&format!("Warning: You should avoid overwriting already defined auth settings for {domain}."));
            }
            store.set(domain, user, pass);
        };
        let add_domain = |config: &mut Config, key: &str, domain: &str| {
            if !string_list(config, key).iter().any(|d| d == domain) {
                io.write_error_at(
                    &format!("{domain} is not in the configured {key}, adding it implicitly as authentication is configured for this domain"),
                    Verbosity::Debug,
                );
                config.merge_config(&json!({ key: [domain] }));
            }
        };

        for (domain, cred) in section(config, "bitbucket-oauth") {
            set(&domain, str_at(&cred, "consumer-key"), Some(str_at(&cred, "consumer-secret")));
        }
        for (domain, token) in section(config, "github-oauth") {
            if domain != "github.com" {
                add_domain(config, "github-domains", &domain);
            }
            set(&domain, token.as_str().unwrap_or(""), Some("x-oauth-basic"));
        }
        for (domain, token) in section(config, "gitlab-oauth") {
            if domain != "gitlab.com" {
                add_domain(config, "gitlab-domains", &domain);
            }
            let token = if token.is_object() { str_at(&token, "token") } else { token.as_str().unwrap_or("") };
            set(&domain, token, Some("oauth2"));
        }
        for (domain, token) in section(config, "gitlab-token") {
            if domain != "gitlab.com" {
                add_domain(config, "gitlab-domains", &domain);
            }
            let (user, pass) = gitlab_token(&token);
            set(&domain, user, Some(pass));
        }
        for (domain, cred) in section(config, "forgejo-token") {
            add_domain(config, "forgejo-domains", &domain);
            set(&domain, str_at(&cred, "username"), Some(str_at(&cred, "token")));
        }
        for (domain, cred) in section(config, "http-basic") {
            set(&domain, str_at(&cred, "username"), cred.get("password").and_then(Value::as_str));
        }
        for (domain, token) in section(config, "bearer") {
            set(&domain, token.as_str().unwrap_or(""), Some("bearer"));
        }
        for (domain, headers) in section(config, "custom-headers") {
            if !headers.is_null() {
                let encoded = komposer_package::json::encode(&headers, komposer_package::json::Flags::PLAIN);
                set(&domain, &encoded, Some("custom-headers"));
            }
        }
        store
    }

    pub fn has(&self, origin: &str) -> bool {
        self.map.read().unwrap().contains_key(origin)
    }

    pub fn get(&self, origin: &str) -> Option<Credentials> {
        self.map.read().unwrap().get(origin).cloned()
    }

    pub fn set(&self, origin: &str, username: &str, password: Option<&str>) {
        let creds = Credentials { username: username.into(), password: password.map(String::from) };
        self.map.write().unwrap().insert(origin.into(), creds);
    }

    /// `AuthHelper::findAuthOrigin()`: `api.github.com`/`api.bitbucket.org` fall back to the bare domain.
    pub fn find_auth_origin(&self, origin: &str) -> Option<String> {
        if self.has(origin) {
            return Some(origin.into());
        }
        if matches!(origin, "api.bitbucket.org" | "api.github.com") {
            let canonical = origin.replacen("api.", "", 1);
            return self.has(&canonical).then_some(canonical);
        }
        None
    }

    /// `AuthHelper::addAuthenticationOptions()`: the headers to send to `origin` for `url`.
    pub fn headers(&self, io: &Io, gitlab_domains: &[String], origin: &str, url: &str) -> Vec<String> {
        static GITHUB_API: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^https?://api\.github\.com/").unwrap());
        let mut headers = vec![];
        let Some(origin) = self.find_auth_origin(origin) else { return headers };
        let Some(auth) = self.get(&origin) else { return headers };
        let in_gitlab = gitlab_domains.contains(&origin);
        let password = auth.password();
        let message = match password {
            "bearer" => {
                headers.push(format!("Authorization: Bearer {}", auth.username));
                None
            }
            "custom-headers" => match serde_json::from_str::<Vec<String>>(&auth.username) {
                Ok(custom) => {
                    headers.extend(custom);
                    Some("Using custom HTTP headers for authentication".to_string())
                }
                Err(_) => None,
            },
            "x-oauth-basic" if origin == "github.com" => GITHUB_API.is_match(url).then(|| {
                headers.push(format!("Authorization: token {}", auth.username));
                "Using GitHub token authentication".to_string()
            }),
            "oauth2" if in_gitlab => {
                headers.push(format!("Authorization: Bearer {}", auth.username));
                Some("Using GitLab OAuth token authentication".into())
            }
            "private-token" | "gitlab-ci-token" if in_gitlab => {
                headers.push(format!("PRIVATE-TOKEN: {}", auth.username));
                Some("Using GitLab private token authentication".into())
            }
            _ if origin == "bitbucket.org" && url != BITBUCKET_OAUTH2_ACCESS_TOKEN_URL && auth.username == "x-token-auth" => {
                (!is_public_bitbucket_download(url)).then(|| {
                    headers.push(format!("Authorization: Bearer {password}"));
                    "Using Bitbucket OAuth token authentication".to_string()
                })
            }
            _ => {
                let encoded = base64::engine::general_purpose::STANDARD.encode(format!("{}:{password}", auth.username));
                headers.push(format!("Authorization: Basic {encoded}"));
                Some(format!("Using HTTP basic authentication with username \"{}\"", sanitize_username(&auth.username)))
            }
        };
        if let Some(message) = message {
            let mut displayed = self.displayed.lock().unwrap();
            if displayed.get(&origin) != Some(&message) {
                io.write_error_at(&message, Verbosity::Debug);
                displayed.insert(origin, message);
            }
        }
        headers
    }
}

/// `Bitbucket::OAUTH2_ACCESS_TOKEN_URL`.
pub const BITBUCKET_OAUTH2_ACCESS_TOKEN_URL: &str = "https://bitbucket.org/site/oauth2/access_token";

fn str_at<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

/// A `gitlab-token` entry: a plain token or `{"username": .., "token": ..}`.
fn gitlab_token(token: &Value) -> (&str, &str) {
    if token.is_object() { (str_at(token, "username"), str_at(token, "token")) } else { (token.as_str().unwrap_or(""), "private-token") }
}

pub(crate) fn string_list(config: &Config, key: &str) -> Vec<String> {
    match config.get(key) {
        Some(Value::Array(a)) => a.iter().filter_map(|v| v.as_str().map(String::from)).collect(),
        _ => vec![],
    }
}

/// `AuthHelper::isPublicBitBucketDownload()`.
pub fn is_public_bitbucket_download(url: &str) -> bool {
    let parts = crate::url::parse(url);
    if !parts.host.unwrap_or("").contains("bitbucket.org") {
        // Bitbucket downloads are hosted on amazonaws, no need to authenticate there
        return true;
    }
    // public downloads follow /{user}/{repo}/downloads/{whatever}
    let segments: Vec<&str> = parts.path.split('/').collect();
    segments.len() >= 4 && segments[3] == "downloads"
}

/// `git config <key>`, e.g. `github.accesstoken`.
fn git_config(key: &str) -> Option<String> {
    let mut cmd = Command::new("git");
    cmd.args(["config", key]);
    let out = crate::util::execute(cmd, None);
    out.ok().then(|| out.stdout.trim().to_string())
}

/// `GitHub::authorizeOAuth()`: a token from `git config github.accesstoken`.
pub fn github_authorize_oauth(store: &AuthStore, github_domains: &[String], origin: &str) -> bool {
    if !github_domains.iter().any(|d| d == origin) {
        return false;
    }
    match git_config("github.accesstoken") {
        Some(token) => {
            store.set(origin, &token, Some("x-oauth-basic"));
            true
        }
        None => false,
    }
}

/// `GitLab::authorizeOAuth()`: git config tokens, then the `gitlab-token` config.
pub fn gitlab_authorize_oauth(store: &AuthStore, config: &Config, gitlab_domains: &[String], origin: &str) -> bool {
    static PORT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r":\d+").unwrap());
    let bc_origin = PORT.replace(origin, "").into_owned();
    if !gitlab_domains.iter().any(|d| *d == origin || *d == bc_origin) {
        return false;
    }
    if let Some(token) = git_config("gitlab.accesstoken") {
        store.set(origin, &token, Some("oauth2"));
        return true;
    }
    if let (Some(user), Some(token)) = (git_config("gitlab.deploytoken.user"), git_config("gitlab.deploytoken.token")) {
        store.set(origin, &user, Some(&token));
        return true;
    }
    let tokens = config.raw("gitlab-token");
    let Some(token) = tokens.and_then(|t| t.get(&bc_origin).or_else(|| t.get(origin))) else { return false };
    let (user, pass) = gitlab_token(token);
    // tokens stored the other way around are swapped back
    if matches!(user, "private-token" | "gitlab-ci-token" | "oauth2") {
        store.set(origin, pass, Some(user));
    } else {
        store.set(origin, user, Some(pass));
    }
    true
}

/// `Bitbucket::authorizeOAuth()`: a token from `git config bitbucket.accesstoken`.
pub fn bitbucket_authorize_oauth(store: &AuthStore, origin: &str) -> bool {
    if origin != "bitbucket.org" {
        return false;
    }
    match git_config("bitbucket.accesstoken") {
        Some(token) => {
            store.set(origin, "x-token-auth", Some(&token));
            true
        }
        None => false,
    }
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> impl Iterator<Item = &'a str> {
    headers.iter().filter(move |(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.trim())
}

/// `GitHub::isRateLimited()`.
pub fn github_is_rate_limited(headers: &[(String, String)]) -> bool {
    header(headers, "x-ratelimit-remaining").any(|v| v == "0")
}

/// `GitHub::getRateLimit()`: `(limit, reset time)`, `?` when unknown.
pub fn github_rate_limit(headers: &[(String, String)]) -> (String, String) {
    let limit = header(headers, "x-ratelimit-limit").last().and_then(|v| v.parse::<i64>().ok()).map_or("?".into(), |v| v.to_string());
    let reset = header(headers, "x-ratelimit-reset")
        .last()
        .and_then(|v| v.parse::<i64>().ok())
        .and_then(|ts| chrono::DateTime::from_timestamp(ts, 0))
        .map(|d| d.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| "?".into());
    (limit, reset)
}

/// `GitHub::requiresSso()`.
pub fn github_requires_sso(headers: &[(String, String)]) -> bool {
    header(headers, "x-github-sso").any(|v| v.to_ascii_lowercase().starts_with("required"))
}

/// `GitHub::getSsoUrl()`.
pub fn github_sso_url(headers: &[(String, String)]) -> Option<String> {
    static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\burl=([^\s;]+)").unwrap());
    header(headers, "x-github-sso").filter(|v| v.starts_with("required")).find_map(|v| RE.captures(v).map(|c| c[1].to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(origin: &str, user: &str, pass: Option<&str>) -> AuthStore {
        let s = AuthStore::default();
        s.set(origin, user, pass);
        s
    }

    fn gitlab() -> Vec<String> {
        vec!["gitlab.com".into()]
    }

    #[test]
    fn no_credentials_no_headers() {
        let io = Io::null();
        assert!(AuthStore::default().headers(&io, &gitlab(), "github.com", "https://api.github.com/").is_empty());
    }

    #[test]
    fn bearer_and_github_tokens() {
        let io = Io::null();
        let s = store("example.org", "my_token", Some("bearer"));
        assert_eq!(s.headers(&io, &gitlab(), "example.org", "https://example.org/x"), ["Authorization: Bearer my_token"]);
        let s = store("github.com", "gh_tok", Some("x-oauth-basic"));
        assert_eq!(s.headers(&io, &gitlab(), "github.com", "https://api.github.com/"), ["Authorization: token gh_tok"]);
        // only API URLs get the token
        assert!(s.headers(&io, &gitlab(), "github.com", "https://github.com/a/b").is_empty());
        // api.github.com falls back to github.com credentials
        assert_eq!(s.headers(&io, &gitlab(), "api.github.com", "https://api.github.com/"), ["Authorization: token gh_tok"]);
    }

    #[test]
    fn gitlab_tokens() {
        let io = Io::null();
        for (pass, expected) in
            [("oauth2", "Authorization: Bearer tok"), ("private-token", "PRIVATE-TOKEN: tok"), ("gitlab-ci-token", "PRIVATE-TOKEN: tok")]
        {
            let s = store("gitlab.com", "tok", Some(pass));
            assert_eq!(s.headers(&io, &gitlab(), "gitlab.com", "https://gitlab.com/api/v4/x"), [expected]);
        }
    }

    #[test]
    fn bitbucket_oauth_and_public_urls() {
        let io = Io::null();
        let s = store("bitbucket.org", "x-token-auth", Some("tok"));
        assert_eq!(s.headers(&io, &gitlab(), "bitbucket.org", "https://bitbucket.org/site/oauth2/authorize"), ["Authorization: Bearer tok"]);
        for url in [
            "https://bitbucket.org/user/repo/downloads/whatever",
            "https://bbuseruploads.s3.amazonaws.com/9421ee72-638e-43a9-82ea-39cfaae2bfaa/downloads/b87c59d9-54f3-4922-b711-d89059ec3bcf",
        ] {
            assert!(s.headers(&io, &gitlab(), "bitbucket.org", url).is_empty(), "{url}");
            assert!(is_public_bitbucket_download(url));
        }
        assert!(!is_public_bitbucket_download("https://bitbucket.org/site/oauth2/authorize"));
    }

    #[test]
    fn basic_and_custom_headers() {
        let io = Io::null();
        let s = store("example.org", "my_username", Some("my_password"));
        let expected = format!("Authorization: Basic {}", base64::engine::general_purpose::STANDARD.encode("my_username:my_password"));
        assert_eq!(s.headers(&io, &gitlab(), "example.org", "https://example.org/x"), [expected]);
        // bitbucket.org with a non x-token-auth username is basic auth
        let s = store("bitbucket.org", "my_username", Some("my_password"));
        assert_eq!(s.headers(&io, &gitlab(), "bitbucket.org", "https://bitbucket.org/x").len(), 1);

        let mut config = Config::new(false, None);
        config.merge_config(&json!({"custom-headers": {"example.org": ["API-TOKEN: abc", "X-Foo: bar"]}}));
        let s = AuthStore::from_config(&mut config, &io);
        assert_eq!(s.headers(&io, &gitlab(), "example.org", "https://example.org/x"), ["API-TOKEN: abc", "X-Foo: bar"]);
    }

    #[test]
    fn loads_config_and_adds_domains() {
        let io = Io::null();
        let mut config = Config::new(false, None);
        config.merge_config(&json!({
            "github-oauth": {"github.com": "a", "ghe.example": "b"},
            "gitlab-token": {"gitlab.example": {"username": "u", "token": "t"}, "gitlab.com": "plain"},
            "http-basic": {"repo.example": {"username": "u", "password": "p"}},
            "bitbucket-oauth": {"bitbucket.org": {"consumer-key": "k", "consumer-secret": "s"}},
        }));
        let s = AuthStore::from_config(&mut config, &io);
        assert_eq!(s.get("ghe.example").unwrap().password.as_deref(), Some("x-oauth-basic"));
        assert_eq!(s.get("gitlab.com").unwrap(), Credentials { username: "plain".into(), password: Some("private-token".into()) });
        assert_eq!(s.get("gitlab.example").unwrap().username, "u");
        assert_eq!(s.get("bitbucket.org").unwrap().username, "k");
        assert!(string_list(&config, "github-domains").contains(&"ghe.example".to_string()));
        assert!(string_list(&config, "gitlab-domains").contains(&"gitlab.example".to_string()));
    }

    #[test]
    fn find_auth_origin_cases() {
        for (origin, has_origin, canonical, has_canonical, expected) in [
            ("api.github.com", true, "github.com", false, Some("api.github.com")),
            ("api.github.com", false, "github.com", true, Some("github.com")),
            ("api.bitbucket.org", false, "bitbucket.org", true, Some("bitbucket.org")),
            ("api.github.com", false, "github.com", false, None),
            ("example.org", false, "example.org", false, None),
        ] {
            let s = AuthStore::default();
            if has_origin {
                s.set(origin, "u", None);
            }
            if has_canonical {
                s.set(canonical, "u", None);
            }
            assert_eq!(s.find_auth_origin(origin).as_deref(), expected);
        }
    }

    #[test]
    fn github_rate_limit_headers() {
        let headers = vec![
            ("X-RateLimit-Limit".to_string(), "60".to_string()),
            ("X-RateLimit-Remaining".to_string(), "0".to_string()),
            ("X-RateLimit-Reset".to_string(), "1700000000".to_string()),
            ("X-GitHub-SSO".to_string(), "required; url=https://github.com/orgs/x/sso?authorization_request=abc".to_string()),
        ];
        assert!(github_is_rate_limited(&headers));
        assert_eq!(github_rate_limit(&headers), ("60".into(), "2023-11-14 22:13:20".into()));
        assert!(github_requires_sso(&headers));
        assert_eq!(github_sso_url(&headers).as_deref(), Some("https://github.com/orgs/x/sso?authorization_request=abc"));
    }
}
