//! `Composer\Config` plus `Factory::createConfig()`: defaults, `COMPOSER_HOME/config.json`,
//! `auth.json`, `COMPOSER_AUTH`, and the project's `config` section.

use crate::json;
use crate::loader::scalar_string_ref as scalar;
use anyhow::{Context, Result};
use regex::Regex;
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

/// Path-like keys with `COMPOSER_*` env overrides.
const PATH_KEYS: [&str; 9] =
    ["vendor-dir", "bin-dir", "data-dir", "cache-dir", "cache-files-dir", "cache-repo-dir", "cache-vcs-dir", "cafile", "capath"];

/// Keys whose values are merged instead of replaced.
const MERGED_AUTH_KEYS: [&str; 8] =
    ["bitbucket-oauth", "github-oauth", "gitlab-oauth", "gitlab-token", "http-basic", "bearer", "client-certificate", "forgejo-token"];

#[derive(Clone, Debug)]
pub struct Config {
    config: Map<String, Value>,
    /// (name, definition); `None` names are anonymous (list) entries
    repositories: Vec<(Option<String>, Value)>,
    base_dir: Option<PathBuf>,
    use_env: bool,
}

impl Config {
    /// `new Config($useEnvironment, $baseDir)`: defaults only.
    pub fn new(use_env: bool, base_dir: Option<&Path>) -> Config {
        let Value::Object(config) = default_config() else { unreachable!() };
        Config {
            config,
            repositories: vec![(Some("packagist.org".into()), json!({"type": "composer", "url": "https://repo.packagist.org"}))],
            base_dir: base_dir.map(Path::to_path_buf),
            use_env,
        }
    }

    /// `Factory::createConfig()`.
    pub fn create(cwd: &Path) -> Result<Config> {
        let mut config = Config::new(true, Some(cwd));
        let home = home_dir();
        config.merge_config(&json!({
            "home": home,
            "cache-dir": cache_dir(&home),
            "data-dir": data_dir(&home),
        }));

        let file = config.home().join("config.json");
        if file.exists() {
            let v = json::read(&file)?;
            if let Value::Object(m) = v {
                config.merge(&m);
            }
        }
        let auth = config.home().join("auth.json");
        if auth.exists()
            && let Value::Object(m) = json::read(&auth)?
        {
            config.merge_config(&Value::Object(m));
        }
        if let Ok(env) = std::env::var("COMPOSER_AUTH")
            && !env.is_empty()
        {
            let v = json::parse(&env).context("COMPOSER_AUTH environment variable is malformed, should be a valid JSON object")?;
            config.merge_config(&v);
        }
        Ok(config)
    }

    /// Merges the `config` keys of a config/composer.json-shaped object.
    pub fn merge_config(&mut self, values: &Value) {
        let mut m = Map::new();
        m.insert("config".into(), values.clone());
        self.merge(&m);
    }

    /// `Config::merge()`: `{"config": {...}, "repositories": ...}`.
    pub fn merge(&mut self, data: &Map<String, Value>) {
        if let Some(Value::Object(cfg)) = data.get("config") {
            for (key, val) in cfg {
                let current = self.config.get(key).cloned();
                let merged = match (key.as_str(), current, val) {
                    (k, Some(Value::Object(mut cur)), Value::Object(new)) if MERGED_AUTH_KEYS.contains(&k) => {
                        cur.extend(new.clone());
                        Value::Object(cur)
                    }
                    ("allow-plugins", Some(Value::Object(cur)), Value::Object(new)) => {
                        let mut out = new.clone();
                        out.extend(cur);
                        out.extend(new.clone());
                        Value::Object(out)
                    }
                    ("github-domains" | "gitlab-domains", Some(Value::Array(mut cur)), Value::Array(new)) => {
                        for v in new {
                            if !cur.contains(v) {
                                cur.push(v.clone());
                            }
                        }
                        Value::Array(cur)
                    }
                    ("preferred-install", Some(cur), new) if cur.is_object() || new.is_object() => {
                        let as_map = |v: Value| match v {
                            Value::Object(m) => m,
                            v => Map::from_iter([("*".to_string(), v)]),
                        };
                        let mut out = as_map(cur);
                        out.extend(as_map(new.clone()));
                        // the full match pattern needs to be last
                        if let Some(w) = out.shift_remove("*") {
                            out.insert("*".into(), w);
                        }
                        Value::Object(out)
                    }
                    ("audit", Some(Value::Object(mut cur)), Value::Object(new)) => {
                        let ignores = cur.get("ignore").cloned();
                        cur.extend(new.clone());
                        if let (Some(Value::Object(mut old)), Some(Value::Object(add))) = (ignores.clone(), new.get("ignore")) {
                            old.extend(add.clone());
                            cur.insert("ignore".into(), Value::Object(old));
                        } else if let (Some(Value::Array(mut old)), Some(Value::Array(add))) = (ignores, new.get("ignore")) {
                            old.extend(add.clone());
                            cur.insert("ignore".into(), Value::Array(old));
                        }
                        Value::Object(cur)
                    }
                    (_, _, v) => v.clone(),
                };
                self.config.insert(key.clone(), merged);
            }
        }

        let repos: Vec<(Option<String>, Value)> = match data.get("repositories") {
            Some(Value::Array(a)) => a.iter().map(|v| (None, v.clone())).collect(),
            Some(Value::Object(m)) => m.iter().map(|(k, v)| (Some(k.clone()), v.clone())).collect(),
            _ => vec![],
        };
        if repos.is_empty() {
            return;
        }
        // new repositories take precedence: they are prepended, in their own order
        let mut prepend = vec![];
        for (name, repo) in repos.into_iter().rev() {
            if repo == Value::Bool(false) {
                if let Some(n) = &name {
                    self.disable_repo(n);
                }
                continue;
            }
            if let Value::Object(m) = &repo
                && m.len() == 1
                && m.values().next() == Some(&Value::Bool(false))
            {
                let n = m.keys().next().unwrap().clone();
                self.disable_repo(&n);
                continue;
            }
            if let (Some("composer"), Some(url)) = (repo.get("type").and_then(Value::as_str), repo.get("url").and_then(Value::as_str)) {
                static PACKAGIST: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^https?://(?:[a-z0-9-.]+\.)?packagist.org(/|$)").unwrap());
                if PACKAGIST.is_match(url) {
                    self.disable_repo("packagist.org");
                }
            }
            let name = name.filter(|n| n.parse::<usize>().is_err()).map(|n| if n == "packagist" { "packagist.org".into() } else { n });
            if let Some(n) = &name {
                self.repositories.retain(|(existing, _)| existing.as_ref() != Some(n));
            }
            prepend.push((name, repo));
        }
        prepend.reverse();
        prepend.append(&mut self.repositories);
        self.repositories = prepend;
    }

    /// `disableRepoByName()`.
    fn disable_repo(&mut self, name: &str) {
        let exists = self.repositories.iter().any(|(n, _)| n.as_deref() == Some(name));
        // BC support for the default "packagist" named repo
        let name = if !exists && name == "packagist" { "packagist.org" } else { name };
        self.repositories.retain(|(n, _)| n.as_deref() != Some(name));
    }

    /// `getRepositories()`: enabled repository definitions, highest priority first.
    pub fn repositories(&self) -> Vec<Value> {
        self.repositories.iter().map(|(_, v)| v.clone()).collect()
    }

    /// Raw (unprocessed) value.
    pub fn raw(&self, key: &str) -> Option<&Value> {
        self.config.get(key).filter(|v| !v.is_null())
    }

    /// `Config::get()` for generic values, with `{$var}` interpolation of strings.
    pub fn get(&self, key: &str) -> Option<Value> {
        match key {
            _ if PATH_KEYS.contains(&key) => self.get_string(key).map(Value::String),
            "process-timeout" => Some(self.process_timeout().into()),
            _ => self.raw(key).map(|v| match v {
                Value::String(s) => Value::String(self.interpolate(s)),
                v => v.clone(),
            }),
        }
    }

    /// String-valued keys, with the `COMPOSER_*` env overrides of path keys.
    pub fn get_string(&self, key: &str) -> Option<String> {
        if PATH_KEYS.contains(&key) {
            let env = format!("COMPOSER_{}", key.to_uppercase().replace('-', "_"));
            let raw = self.env(&env).or_else(|| self.raw(key).and_then(scalar))?;
            let val = expand_path(self.interpolate(&raw).trim_end_matches(['/', '\\']));
            if !key.ends_with("-dir") {
                return Some(val);
            }
            return Some(self.realpath(&val));
        }
        if key == "home" {
            return self.raw(key).and_then(scalar).map(|h| self.interpolate(&expand_path(&h)).trim_end_matches(['/', '\\']).to_string());
        }
        if key == "autoloader-suffix" {
            return self.raw(key).and_then(scalar).filter(|s| !s.is_empty()).map(|s| self.interpolate(&s));
        }
        self.raw(key).and_then(scalar).map(|s| self.interpolate(&s))
    }

    /// Path keys (`vendor-dir`, `bin-dir`, `cache-*-dir`...), absolute.
    pub fn get_path(&self, key: &str) -> PathBuf {
        PathBuf::from(self.get_string(key).unwrap_or_default())
    }

    /// Path keys as configured, relative to the project when they are (`RELATIVE_PATHS`).
    pub fn get_relative_path(&self, key: &str) -> String {
        let env = format!("COMPOSER_{}", key.to_uppercase().replace('-', "_"));
        let raw = self.env(&env).or_else(|| self.raw(key).and_then(scalar)).unwrap_or_default();
        expand_path(self.interpolate_with(&raw, true).trim_end_matches(['/', '\\']))
    }

    pub fn home(&self) -> PathBuf {
        PathBuf::from(self.get_string("home").unwrap_or_default())
    }

    /// Booleans; `cache-read-only` and `htaccess-protect` have `COMPOSER_*` overrides.
    pub fn get_bool(&self, key: &str) -> bool {
        if key == "secure-http" && self.get_bool("disable-tls") {
            return false;
        }
        let env = matches!(key, "cache-read-only" | "htaccess-protect")
            .then(|| self.env(&format!("COMPOSER_{}", key.to_uppercase().replace('-', "_"))))
            .flatten();
        match env {
            Some(v) => v != "false" && !v.is_empty() && v != "0",
            None => match self.raw(key) {
                Some(Value::String(s)) => s != "false" && !s.is_empty() && s != "0",
                Some(v) => crate::loader::php_truthy(v),
                None => false,
            },
        }
    }

    pub fn process_timeout(&self) -> u64 {
        let v = self.env("COMPOSER_PROCESS_TIMEOUT").and_then(|v| v.parse::<i64>().ok());
        let v = v.or_else(|| self.raw("process-timeout").and_then(|v| v.as_i64().or_else(|| v.as_str()?.parse().ok())));
        v.unwrap_or(300).max(0) as u64
    }

    /// `cache-ttl` / `cache-files-ttl` in seconds.
    pub fn cache_ttl(&self, key: &str) -> u64 {
        let int = |k: &str| self.raw(k).and_then(|v| v.as_i64().or_else(|| v.as_str()?.parse().ok()));
        match key {
            "cache-files-ttl" => int(key).map(|v| v.max(0) as u64).unwrap_or_else(|| self.cache_ttl("cache-ttl")),
            _ => int("cache-ttl").unwrap_or(15552000).max(0) as u64,
        }
    }

    /// `cache-files-maxsize` in bytes.
    pub fn cache_files_maxsize(&self) -> u64 {
        static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^\s*([0-9.]+)\s*(?:([kmg])(?:i?b)?)?\s*$").unwrap());
        let raw = self.raw("cache-files-maxsize").and_then(scalar).unwrap_or_else(|| "300MiB".into());
        let Some(c) = RE.captures(&raw) else { return 300 * 1024 * 1024 };
        let mut size: f64 = c[1].parse().unwrap_or(0.0);
        let mult = match c.get(2).map(|m| m.as_str().to_ascii_lowercase()) {
            Some(u) if u == "g" => 1024f64.powi(3),
            Some(u) if u == "m" => 1024f64.powi(2),
            Some(u) if u == "k" => 1024.0,
            _ => 1.0,
        };
        size *= mult;
        size.max(0.0) as u64
    }

    fn env(&self, var: &str) -> Option<String> {
        if !self.use_env {
            return None;
        }
        std::env::var(var).ok()
    }

    fn interpolate(&self, s: &str) -> String {
        self.interpolate_with(s, false)
    }

    /// `process()`: replaces `{$key}` with the value of `key`.
    fn interpolate_with(&self, s: &str, relative: bool) -> String {
        static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\{\$(.+)\}").unwrap());
        RE.replace_all(
            s,
            |c: &regex::Captures| {
                if relative && c[1].ends_with("-dir") { self.get_relative_path(&c[1]) } else { self.get_string(&c[1]).unwrap_or_default() }
            },
        )
        .into_owned()
    }

    fn realpath(&self, path: &str) -> String {
        static ABS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^(?:/|[a-z]:|[a-z0-9.]+://|\\\\)").unwrap());
        match &self.base_dir {
            Some(base) if !ABS.is_match(path) => format!("{}/{}", base.display(), path),
            _ => path.to_string(),
        }
    }

    pub fn base_dir(&self) -> Option<&Path> {
        self.base_dir.as_deref()
    }

    /// The merged `config` map (for `config --list`-style output).
    pub fn all(&self) -> &Map<String, Value> {
        &self.config
    }
}

/// `Platform::expandPath()`: leading `~` and `$VAR` / `%VAR%`.
pub fn expand_path(path: &str) -> String {
    static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^~|^\$([A-Za-z_][A-Za-z0-9_]*)|^%(\w+)%").unwrap());
    RE.replace(path, |c: &regex::Captures| {
        if c[0].starts_with('~') {
            return user_dir();
        }
        let var = c.get(1).or(c.get(2)).map(|m| m.as_str()).unwrap_or("");
        std::env::var(var).unwrap_or_default()
    })
    .into_owned()
}

fn user_dir() -> String {
    std::env::var("HOME").unwrap_or_default().trim_end_matches('/').to_string()
}

fn use_xdg() -> bool {
    std::env::vars_os().any(|(k, _)| k.to_string_lossy().starts_with("XDG_")) || Path::new("/etc/xdg").is_dir()
}

fn non_empty_env(var: &str) -> Option<String> {
    std::env::var(var).ok().filter(|v| !v.is_empty())
}

/// `Factory::getHomeDir()`.
pub fn home_dir() -> String {
    if let Some(home) = non_empty_env("COMPOSER_HOME") {
        return home;
    }
    let user = user_dir();
    let mut dirs = vec![];
    if use_xdg() {
        let xdg = non_empty_env("XDG_CONFIG_HOME").unwrap_or_else(|| format!("{user}/.config"));
        dirs.push(format!("{xdg}/composer"));
    }
    dirs.push(format!("{user}/.composer"));
    dirs.iter().find(|d| Path::new(d).is_dir()).cloned().unwrap_or_else(|| dirs[0].clone())
}

/// `Factory::getCacheDir()`.
pub fn cache_dir(home: &str) -> String {
    if let Some(dir) = non_empty_env("COMPOSER_CACHE_DIR") {
        return dir;
    }
    if let Some(home) = non_empty_env("COMPOSER_HOME") {
        return format!("{home}/cache");
    }
    let user = user_dir();
    if cfg!(target_os = "macos") {
        return format!("{user}/Library/Caches/composer");
    }
    if home == format!("{user}/.composer") && Path::new(&format!("{home}/cache")).is_dir() {
        return format!("{home}/cache");
    }
    if use_xdg() {
        let xdg = non_empty_env("XDG_CACHE_HOME").unwrap_or_else(|| format!("{user}/.cache"));
        return format!("{xdg}/composer");
    }
    format!("{home}/cache")
}

/// `Factory::getDataDir()`.
pub fn data_dir(home: &str) -> String {
    if let Some(home) = non_empty_env("COMPOSER_HOME") {
        return home;
    }
    let user = user_dir();
    if home != format!("{user}/.composer") && use_xdg() {
        let xdg = non_empty_env("XDG_DATA_HOME").unwrap_or_else(|| format!("{user}/.local/share"));
        return format!("{xdg}/composer");
    }
    home.to_string()
}

/// `Config::$defaultConfig`.
fn default_config() -> Value {
    serde_json::from_str(DEFAULT_CONFIG).unwrap()
}

const DEFAULT_CONFIG: &str = r#"{
    "process-timeout": 300,
    "use-include-path": false,
    "allow-plugins": [],
    "use-parent-dir": "prompt",
    "preferred-install": "dist",
    "audit": {"ignore": [], "abandoned": "fail"},
    "policy": true,
    "notify-on-install": true,
    "github-protocols": ["https", "ssh", "git"],
    "gitlab-protocol": null,
    "vendor-dir": "vendor",
    "bin-dir": "{$vendor-dir}/bin",
    "cache-dir": "{$home}/cache",
    "data-dir": "{$home}",
    "cache-files-dir": "{$cache-dir}/files",
    "cache-repo-dir": "{$cache-dir}/repo",
    "cache-vcs-dir": "{$cache-dir}/vcs",
    "cache-ttl": 15552000,
    "cache-files-ttl": null,
    "cache-files-maxsize": "300MiB",
    "cache-read-only": false,
    "bin-compat": "auto",
    "discard-changes": false,
    "autoloader-suffix": null,
    "sort-packages": false,
    "optimize-autoloader": false,
    "classmap-authoritative": false,
    "apcu-autoloader": false,
    "prepend-autoloader": true,
    "update-with-minimal-changes": false,
    "github-domains": ["github.com"],
    "bitbucket-expose-hostname": true,
    "disable-tls": false,
    "secure-http": true,
    "secure-svn-domains": [],
    "cafile": null,
    "capath": null,
    "github-expose-hostname": true,
    "gitlab-domains": ["gitlab.com"],
    "store-auths": "prompt",
    "platform": [],
    "archive-format": "tar",
    "archive-dir": ".",
    "htaccess-protect": true,
    "use-github-api": true,
    "lock": true,
    "platform-check": "php-only",
    "bitbucket-oauth": [],
    "github-oauth": [],
    "gitlab-oauth": [],
    "gitlab-token": [],
    "http-basic": [],
    "bearer": [],
    "custom-headers": [],
    "bump-after-update": false,
    "allow-missing-requirements": false,
    "client-certificate": [],
    "forgejo-domains": ["codeberg.org"],
    "forgejo-token": [],
    "source-fallback": false
}"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interpolates_and_resolves_dirs() {
        let mut c = Config::new(false, Some(Path::new("/proj")));
        c.merge_config(&json!({"home": "/h", "cache-dir": "/c"}));
        assert_eq!(c.get_string("vendor-dir").unwrap(), "/proj/vendor");
        assert_eq!(c.get_string("bin-dir").unwrap(), "/proj/vendor/bin");
        assert_eq!(c.get_relative_path("bin-dir"), "vendor/bin");
        assert_eq!(c.get_string("cache-files-dir").unwrap(), "/c/files");
        assert_eq!(c.cache_files_maxsize(), 300 * 1024 * 1024);
    }

    #[test]
    fn repositories_prepend_and_disable_packagist() {
        let mut c = Config::new(false, None);
        let data = json!({"repositories": [{"type": "vcs", "url": "a"}, {"packagist.org": false}]});
        c.merge(data.as_object().unwrap());
        assert_eq!(c.repositories(), vec![json!({"type": "vcs", "url": "a"})]);

        let mut c = Config::new(false, None);
        c.merge(json!({"repositories": {"packagist": false}}).as_object().unwrap());
        assert!(c.repositories().is_empty());
    }

    #[test]
    fn auth_keys_merge() {
        let mut c = Config::new(false, None);
        c.merge_config(&json!({"http-basic": {"a.com": {"username": "u", "password": "p"}}}));
        c.merge_config(&json!({"http-basic": {"b.com": {"username": "u", "password": "p"}}}));
        assert_eq!(c.raw("http-basic").unwrap().as_object().unwrap().len(), 2);
    }
}
