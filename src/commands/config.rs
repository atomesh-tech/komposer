//! `config`: `ConfigCommand` and `BaseConfigCommand`, writing through `JsonConfigSource`.

use crate::cli::ConfigArgs;
use crate::composer::composer_file;
use crate::json_manipulator::{JsonManipulator, is_int_key, is_numeric, php_array, php_get};
use anyhow::{Context, Result, anyhow, bail};
use komposer_package::config::{self as cfg, Config};
use komposer_package::io::{Io, Verbosity};
use komposer_package::json::{self, Flags};
use regex::Regex;
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};

const CONFIGURABLE_PACKAGE_PROPERTIES: [&str; 12] = [
    "name",
    "type",
    "description",
    "homepage",
    "version",
    "minimum-stability",
    "prefer-stable",
    "keywords",
    "license",
    "repositories",
    "suggest",
    "extra",
];

/// Config keys whose `[]` value is shown as `{}` (their schema type includes `object`).
const OBJECT_KEYS: [&str; 14] = [
    "platform",
    "allow-plugins",
    "preferred-install",
    "audit",
    "policy",
    "github-oauth",
    "gitlab-oauth",
    "gitlab-token",
    "bearer",
    "custom-headers",
    "forgejo-token",
    "http-basic",
    "client-certificate",
    "bitbucket-oauth",
];

/// `PolicyConfig::BUILTIN_LIST_NAMES` and `NON_LIST_KEYS`.
const POLICY_NON_CUSTOM: [&str; 4] = ["advisories", "abandoned", "malware", "ignore-unreachable"];
const IGNORE_UNREACHABLE_SCOPES: [&str; 3] = ["audit", "install", "update"];

static REPOSITORY_KEY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^repos?(?:itories)?(?:\.(.+))?").unwrap());
static AUTH_KEY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(bitbucket-oauth|github-oauth|gitlab-oauth|gitlab-token|http-basic|custom-headers|bearer|forgejo-token)\.(.+)").unwrap()
});
/// Keys `JsonConfigSource` stores as `config.<key>.<host>`.
static HOST_KEY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(bitbucket-oauth|github-oauth|gitlab-oauth|gitlab-token|bearer|http-basic|custom-headers|forgejo-token|platform)\.").unwrap()
});

pub fn config(io: &Arc<Io>, args: &ConfigArgs, _run_scripts: bool) -> Result<i32> {
    run(io, args, &std::env::current_dir()?, &cfg::home_dir())
}

/// `BaseConfigCommand::initialize()` and `ConfigCommand::execute()` in `cwd` with the given `COMPOSER_HOME`.
fn run(io: &Io, args: &ConfigArgs, cwd: &Path, home_env: &str) -> Result<i32> {
    if args.global && args.file.is_some() {
        bail!("--file and --global can not be combined");
    }
    let home = PathBuf::from(cfg::expand_path(home_env).trim_end_matches(['/', '\\']));
    // with --global, relative paths resolve against the home directory
    let mut settings = Settings::create(if args.global { &home } else { cwd }, home_env)?;
    let home = settings.config.home();

    let file = match (&args.file, args.global) {
        (_, true) => home.join("config.json"),
        (Some(f), _) => f.clone(),
        (None, _) => composer_file()?,
    };
    // `composer global config` creates the global composer.json
    if (file == Path::new("composer.json") || file == Path::new("./composer.json"))
        && !file.exists()
        && std::fs::canonicalize(cwd).ok() == std::fs::canonicalize(&home).ok()
    {
        std::fs::write(&file, "{\n}\n")?;
    }
    if args.global && !file.exists() {
        create_private(&file, &json!({"config": {}}))?;
    }
    if !file.exists() {
        bail!("File \"{}\" cannot be found in the current directory", file.display());
    }
    let auth_file = match args.global {
        true => home.join("auth.json"),
        false => file.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new(".")).join("auth.json"),
    };
    if args.global && !auth_file.exists() {
        let empty = ["bitbucket-oauth", "github-oauth", "gitlab-oauth", "gitlab-token", "http-basic", "bearer", "forgejo-token"];
        create_private(&auth_file, &Value::Object(empty.iter().map(|k| (k.to_string(), json!({}))).collect()))?;
    }

    if args.editor {
        return open_editor(if args.auth { &auth_file } else { &file });
    }

    if !args.global {
        settings.merge(&json::read(&file)?, &file.display().to_string());
        let auth = if auth_file.exists() { json::read(&auth_file)? } else { json!([]) };
        settings.merge(&json!({"config": auth}), &auth_file.display().to_string());
    }
    let cmd =
        ConfigCommand { io, args, settings, source: ConfigSource { file, auth: false }, auth_source: ConfigSource { file: auth_file, auth: true } };
    cmd.execute()?;
    Ok(0)
}

/// Writes a new file readable only by the user, like `JsonFile::write()` plus `chmod 0600`.
fn create_private(path: &Path, data: &Value) -> Result<()> {
    json::write(path, data)?;
    chmod_private(path);
    Ok(())
}

fn chmod_private(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

/// `--editor`: `$EDITOR`, else the first of Composer's candidate editors on the PATH.
fn open_editor(file: &Path) -> Result<i32> {
    let editor = std::env::var("EDITOR").ok().filter(|e| !e.is_empty()).or_else(|| {
        ["editor", "vim", "vi", "nano", "pico", "ed"].into_iter().find(|c| crate::scripts::find_executable(c).is_some()).map(String::from)
    });
    if let Some(editor) = editor {
        // Composer quotes $EDITOR as a single program name too
        std::process::Command::new(&editor).arg(file).status().with_context(|| format!("Could not run the editor {editor}"))?;
    }
    Ok(0)
}

/// `Composer\Config` as the config command sees it: values (delegated to [`Config`]), where
/// each one comes from, and the repositories keyed the way PHP keys them.
struct Settings {
    config: Config,
    sources: HashMap<String, String>,
    repos: Vec<(String, Value)>,
}

impl Settings {
    /// `Factory::createConfig()`.
    fn create(base_dir: &Path, home: &str) -> Result<Settings> {
        let config = Config::new(true, Some(base_dir));
        let packagist = json!({"type": "composer", "url": "https://repo.packagist.org"});
        let mut s = Settings { sources: HashMap::new(), repos: vec![("packagist.org".into(), packagist.clone())], config };
        for (k, v) in s.config.all().clone() {
            set_source(&mut s.sources, &k, &v, "default");
        }
        set_source(&mut s.sources, "repositories.packagist.org", &packagist, "default");
        s.merge(&json!({"config": {"home": home, "cache-dir": cfg::cache_dir(home), "data-dir": cfg::data_dir(home)}}), "default");

        let home = s.config.home();
        for (file, auth) in [(home.join("config.json"), false), (home.join("auth.json"), true)] {
            if file.exists() {
                let data = json::read(&file)?;
                s.merge(&if auth { json!({"config": data}) } else { data }, &file.display().to_string());
            }
        }
        if let Ok(env) = std::env::var("COMPOSER_AUTH")
            && !env.is_empty()
        {
            let data = json::parse(&env).context("COMPOSER_AUTH environment variable is malformed, should be a valid JSON object")?;
            s.merge(&json!({"config": data}), "COMPOSER_AUTH");
        }
        Ok(s)
    }

    /// `Config::merge()`.
    fn merge(&mut self, data: &Value, source: &str) {
        let Value::Object(data) = data else { return };
        self.config.merge(data);
        if let Some(Value::Object(config)) = data.get("config") {
            for (k, v) in config {
                set_source(&mut self.sources, k, v, source);
            }
        }
        let new: Vec<(String, &Value)> = match data.get("repositories") {
            Some(Value::Array(a)) => a.iter().enumerate().map(|(i, v)| (i.to_string(), v)).collect(),
            Some(Value::Object(m)) => m.iter().map(|(k, v)| (k.clone(), v)).collect(),
            _ => return,
        };
        self.repos.reverse();
        for (name, repo) in new.into_iter().rev() {
            if *repo == Value::Bool(false) {
                self.disable_repository(&name);
                continue;
            }
            if let Some((only, Value::Bool(false))) = repo.as_object().filter(|m| m.len() == 1).and_then(|m| m.iter().next()) {
                self.disable_repository(only);
                continue;
            }
            if let (Some("composer"), Some(url)) = (repo.get("type").and_then(Value::as_str), repo.get("url").and_then(Value::as_str)) {
                static PACKAGIST: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^https?://(?:[a-z0-9-.]+\.)?packagist.org(/|$)").unwrap());
                if PACKAGIST.is_match(url) {
                    self.disable_repository("packagist.org");
                }
            }
            let key = if is_int_key(&name) && self.repos.iter().any(|(k, _)| *k == name) {
                // `$this->repositories[] = $repository` when the index is taken
                let next = self.repos.iter().filter_map(|(k, _)| k.parse::<i64>().ok().filter(|_| is_int_key(k))).max().map_or(0, |m| m + 1);
                next.to_string()
            } else if name == "packagist" {
                "packagist.org".into()
            } else {
                name
            };
            match self.repos.iter_mut().find(|(k, _)| *k == key) {
                Some(slot) => slot.1 = repo.clone(),
                None => self.repos.push((key.clone(), repo.clone())),
            }
            set_source(&mut self.sources, &format!("repositories.{key}"), repo, source);
        }
        self.repos.reverse();
    }

    /// `disableRepoByName()`.
    fn disable_repository(&mut self, name: &str) {
        let name = if name == "packagist" && !self.repos.iter().any(|(k, _)| k == name) { "packagist.org" } else { name };
        self.repos.retain(|(k, _)| k != name);
    }

    /// `getRepositories()` as a PHP array.
    fn repositories(&self) -> Value {
        json::php_normalize(Value::Object(self.repos.iter().cloned().collect()))
    }

    /// `getSourceOfValue()`; reading a value from its environment variable records that as the source.
    fn source_of(&self, key: &str) -> String {
        const ENV_KEYS: [&str; 12] = [
            "vendor-dir",
            "bin-dir",
            "process-timeout",
            "data-dir",
            "cache-dir",
            "cache-files-dir",
            "cache-repo-dir",
            "cache-vcs-dir",
            "cafile",
            "capath",
            "cache-read-only",
            "htaccess-protect",
        ];
        let env = format!("COMPOSER_{}", key.to_uppercase().replace('-', "_"));
        if ENV_KEYS.contains(&key) && std::env::var_os(&env).is_some() {
            return env;
        }
        self.sources.get(key).cloned().unwrap_or_else(|| "unknown".into())
    }

    /// `Config::get($key, $relative ? RELATIVE_PATHS : 0)`.
    fn get(&self, key: &str, relative: bool) -> Value {
        config_value(&self.config, key, relative)
    }

    /// `Config::all()['config']`.
    fn all(&self, relative: bool) -> Map<String, Value> {
        self.config.all().keys().map(|k| (k.clone(), self.get(k, relative))).collect()
    }
}

/// `Config::get($key, $relative ? RELATIVE_PATHS : 0)`: `config`'s value as Composer hands it out, with paths,
/// environment overrides and PHP's array shapes applied.
pub(crate) fn config_value(c: &Config, key: &str, relative: bool) -> Value {
    let env = |var: &str| std::env::var(var).ok();
    match key {
        "vendor-dir" | "bin-dir" | "data-dir" | "cache-dir" | "cache-files-dir" | "cache-repo-dir" | "cache-vcs-dir" | "cafile" | "capath" => {
            Value::String(if relative { c.get_relative_path(key) } else { c.get_string(key).unwrap_or_default() })
        }
        "process-timeout" => c.process_timeout().into(),
        "cache-read-only" | "htaccess-protect" | "disable-tls" | "secure-http" | "use-github-api" | "lock" | "source-fallback" => {
            c.get_bool(key).into()
        }
        "cache-ttl" | "cache-files-ttl" => c.cache_ttl(key).into(),
        "cache-files-maxsize" => c.cache_files_maxsize().into(),
        "home" => c.home().display().to_string().into(),
        "bin-compat" => env("COMPOSER_BIN_COMPAT").filter(|v| !v.is_empty()).map_or_else(|| c.get(key).unwrap_or_default(), Value::String),
        "discard-changes" => match env("COMPOSER_DISCARD_CHANGES") {
            Some(v) if v == "stash" => v.into(),
            Some(v) => php_bool(&v).into(),
            None => c.get(key).unwrap_or_default(),
        },
        "github-protocols" => {
            let mut protos = php_array(c.get(key).unwrap_or_default());
            if c.raw("secure-http").is_some_and(komposer_package::loader::php_truthy)
                && let Some(git) = protos.iter().find(|(_, v)| *v == "git").map(|(k, _)| k.clone())
            {
                // keys are kept, so this is no longer a list unless git was last
                protos.shift_remove(&git);
            }
            json::php_normalize(Value::Object(protos))
        }
        "autoloader-suffix" => c.get_string(key).map_or(Value::Null, Value::String),
        "audit" => {
            let mut audit = c.get(key).unwrap_or_default();
            if let Some(v) = env("COMPOSER_AUDIT_ABANDONED")
                && let Some(m) = audit.as_object_mut()
            {
                m.insert("abandoned".into(), v.into());
            }
            if let Some(v) = env("COMPOSER_SECURITY_BLOCKING_ABANDONED").filter(|v| !v.is_empty())
                && let Some(m) = audit.as_object_mut()
            {
                m.insert("block-abandoned".into(), matches!(v.as_str(), "1" | "true" | "on").into());
            }
            audit
        }
        "policy" => match (env("COMPOSER_POLICY").filter(|v| !v.is_empty()).as_deref(), c.get(key).unwrap_or_default()) {
            (Some("0" | "false" | "off"), _) => false.into(),
            (Some("1" | "true" | "on"), Value::Bool(false)) => true.into(),
            (_, policy) => policy,
        },
        _ => c.get(key).unwrap_or_default(),
    }
}

/// `setSourceOfConfigValue()`.
fn set_source(sources: &mut HashMap<String, String>, path: &str, value: &Value, source: &str) {
    sources.insert(path.into(), source.into());
    for (k, v) in entries(value) {
        set_source(sources, &format!("{path}.{k}"), v, source);
    }
}

/// `foreach ($array as $key => $value)`.
fn entries(v: &Value) -> Box<dyn Iterator<Item = (String, &Value)> + '_> {
    match v {
        Value::Object(m) => Box::new(m.iter().map(|(k, v)| (k.clone(), v))),
        Value::Array(a) => Box::new(a.iter().enumerate().map(|(i, v)| (i.to_string(), v))),
        _ => Box::new(std::iter::empty()),
    }
}

fn is_php_array(v: &Value) -> bool {
    v.is_array() || v.is_object()
}

/// `$value === []`.
fn is_empty_array(v: &Value) -> bool {
    v.as_array().is_some_and(Vec::is_empty) || v.as_object().is_some_and(Map::is_empty)
}

/// PHP string conversion of a scalar.
fn php_string(v: &Value) -> String {
    match v {
        Value::Null | Value::Bool(false) => String::new(),
        Value::Bool(true) => "1".into(),
        Value::String(s) => s.clone(),
        v => v.to_string(),
    }
}

/// Composer's `$booleanValidator`.
fn is_bool_str(v: &str) -> bool {
    matches!(v, "true" | "false" | "1" | "0")
}

/// `$val !== 'false' && (bool) $val`.
fn php_bool(v: &str) -> bool {
    !matches!(v, "false" | "" | "0")
}

/// `intval()` of a numeric string.
fn intval(v: &str) -> Value {
    let v = v.trim();
    v.parse::<i64>().unwrap_or_else(|_| v.parse::<f64>().map_or(0, |f| f as i64)).into()
}

/// `JsonFile::parseJson()` of a command line value.
fn parse_json(v: &str) -> Result<Value> {
    json::parse(v).map_err(|e| anyhow!("The input does not contain valid JSON\n{e}"))
}

/// `PolicyConfig::getFutureReservedListNameError()`.
fn reserved_policy_error(name: &str) -> Result<()> {
    const RESERVED: [&str; 10] =
        ["package", "packages", "license", "licence", "licenses", "licences", "support", "maintenance", "security", "minimum-release-age"];
    if name.starts_with("ignore") {
        bail!("Invalid dependency policy name: \"{name}\" starts with reserved prefix \"ignore\".");
    }
    if RESERVED.contains(&name) {
        bail!("Invalid dependency policy name: \"{name}\" is reserved for future use.");
    }
    Ok(())
}

/// `--merge`: `array_merge()` for two lists, `$value + $current` otherwise (or an error if
/// `strict` and only one of them is a list).
fn merge_with(value: Value, current: Option<&Value>, key: &str, strict: bool) -> Result<Value> {
    let Some(current) = current.filter(|c| is_php_array(c)).filter(|_| is_php_array(&value)) else { return Ok(value) };
    match (value, current) {
        (Value::Array(new), Value::Array(old)) => Ok(Value::Array(old.iter().cloned().chain(new).collect())),
        (new, old) if !strict || (new.is_object() && old.is_object()) => {
            let mut out = php_array(new);
            for (k, v) in php_array(old.clone()) {
                out.entry(k).or_insert(v);
            }
            Ok(Value::Object(out))
        }
        _ => bail!("Cannot merge array and object for {key}"),
    }
}

type Single = (fn(&str) -> bool, fn(&str) -> Value);
type Multi = fn(&[String]) -> Result<(), &'static str>;

/// `$uniqueConfigValues`: validator and normalizer of single-value config settings.
fn unique_config_value(key: &str) -> Option<Single> {
    let keep: fn(&str) -> Value = |v| v.into();
    let null_or_keep: fn(&str) -> Value = |v| if v == "null" { Value::Null } else { v.into() };
    let boolean: Single = (is_bool_str, |v| php_bool(v).into());
    Some(match key {
        "process-timeout" | "cache-ttl" | "cache-files-ttl" => (is_numeric, intval),
        "use-include-path"
        | "use-github-api"
        | "notify-on-install"
        | "sort-packages"
        | "optimize-autoloader"
        | "classmap-authoritative"
        | "apcu-autoloader"
        | "prepend-autoloader"
        | "update-with-minimal-changes"
        | "disable-tls"
        | "secure-http"
        | "source-fallback"
        | "github-expose-hostname"
        | "htaccess-protect"
        | "lock"
        | "allow-plugins"
        | "audit.ignore-unreachable"
        | "audit.block-insecure"
        | "audit.block-abandoned"
        | "policy.advisories.block"
        | "policy.malware.block"
        | "policy.abandoned.block"
        | "policy.ignore-unreachable" => boolean,
        "preferred-install" => (|v| matches!(v, "auto" | "source" | "dist"), keep),
        "gitlab-protocol" => (|v| matches!(v, "git" | "http" | "https"), keep),
        "store-auths" | "use-parent-dir" => {
            (|v| matches!(v, "true" | "false" | "prompt"), |v| if v == "prompt" { v.into() } else { php_bool(v).into() })
        }
        "vendor-dir" | "bin-dir" | "archive-dir" | "archive-format" | "data-dir" | "cache-dir" | "cache-files-dir" | "cache-repo-dir"
        | "cache-vcs-dir" => (|_| true, keep),
        "cache-files-maxsize" => (|v| Regex::new(r"(?i)^\s*([0-9.]+)\s*(?:([kmg])(?:i?b)?)?\s*$").unwrap().is_match(v), keep),
        "bin-compat" => (|v| matches!(v, "auto" | "full" | "proxy" | "symlink"), keep),
        "discard-changes" => (|v| v == "stash" || is_bool_str(v), |v| if v == "stash" { v.into() } else { php_bool(v).into() }),
        "autoloader-suffix" => (|_| true, null_or_keep),
        "bump-after-update" => {
            (|v| matches!(v, "dev" | "no-dev") || is_bool_str(v), |v| if v == "dev" || v == "no-dev" { v.into() } else { php_bool(v).into() })
        }
        "cafile" => (|v| Path::new(v).exists() && std::fs::File::open(v).is_ok(), null_or_keep),
        "capath" => (|v| Path::new(v).is_dir() && std::fs::read_dir(v).is_ok(), null_or_keep),
        "platform-check" => (|v| v == "php-only" || is_bool_str(v), |v| if v == "php-only" { v.into() } else { php_bool(v).into() }),
        "audit.abandoned" | "policy.advisories.audit" | "policy.malware.audit" | "policy.abandoned.audit" => {
            (|v| matches!(v, "ignore" | "report" | "fail"), keep)
        }
        "policy.malware.block-scope" => (|v| matches!(v, "all" | "update" | "install"), keep),
        _ => return None,
    })
}

/// `$multiConfigValues`: validation of multi-value config settings.
fn multi_config_value(key: &str) -> Option<Multi> {
    Some(match key {
        "github-protocols" => |v| {
            if v.iter().all(|p| ["git", "https", "ssh"].contains(&p.as_str())) { Ok(()) } else { Err("valid protocols include: git, https, ssh") }
        },
        "github-domains" | "gitlab-domains" | "policy.malware.ignore-source" => |_| Ok(()),
        "audit.ignore-severity" | "policy.advisories.ignore-severity" => |v| {
            if v.iter().all(|s| ["low", "medium", "high", "critical"].contains(&s.as_str())) {
                Ok(())
            } else {
                Err("valid severities include: low, medium, high, critical")
            }
        },
        _ => return None,
    })
}

/// `$uniqueProps`: validator and normalizer of single-value package properties.
fn unique_prop(key: &str) -> Option<Single> {
    Some(match key {
        "name" | "type" | "description" | "homepage" | "version" => (|_| true, |v| v.into()),
        "minimum-stability" => (|_| true, |v| normalize_stability(v).into()),
        "prefer-stable" => (is_bool_str, |v| php_bool(v).into()),
        _ => return None,
    })
}

/// `BasePackage::STABILITIES`.
pub(crate) const STABILITIES: [&str; 5] = ["stable", "RC", "beta", "alpha", "dev"];

/// The `composer-schema.json` rules that `config` and `init` writes can break, worded like
/// `JsonFile::validateSchema()` errors.
pub(crate) fn schema_error(path: &str, value: &Value) -> Option<String> {
    const NAME: &str = r"^[a-z0-9]([_.-]?[a-z0-9]+)*/[a-z0-9](([_.]|-{1,2})?[a-z0-9]+)*$";
    const TYPE: &str = r"^[a-z0-9-]+$";
    const VERSION: &str = r"^[vV]?\d+(?:[.-]\d+){0,3}[._-]?(?:(?:[sS][tT][aA][bB][lL][eE]|[bB][eE][tT][aA]|[bB]|[rR][cC]|[aA][lL][pP][hH][aA]|[aA]|[pP][aA][tT][cC][hH]|[pP][lL]|[pP])(?:(?:[.-]?\d+)*)?)?(?:[.-]?[dD][eE][vV]|\.x-dev)?(?:\+.*)?$|^dev-.*$";
    // ponytail: a URL shape check, not FILTER_VALIDATE_URL
    const URL: &str = r"(?i)^[a-z][a-z0-9+.-]*://[^\s/?#]+\S*$";
    let s = value.as_str().unwrap_or_default();
    let pattern = match path {
        "name" => NAME,
        "type" => TYPE,
        "version" => VERSION,
        _ => "",
    };
    if !pattern.is_empty() && !Regex::new(pattern).unwrap().is_match(s) {
        return Some(format!("{path} : Does not match the regex pattern {pattern}"));
    }
    match path {
        "homepage" if !Regex::new(URL).unwrap().is_match(s) => Some("homepage : Invalid URL format".into()),
        "minimum-stability" if !["dev", "alpha", "beta", "rc", "RC", "stable"].contains(&s) => {
            Some(r#"minimum-stability : Does not have a value in the enumeration ["dev","alpha","beta","rc","RC","stable"]"#.into())
        }
        "config.autoloader-suffix" | "config.cafile" | "config.capath" if value.is_null() => {
            Some(format!("{path} : NULL value found, but a string is required"))
        }
        _ => None,
    }
}

/// `JsonConfigSource::manipulateJson()`'s error when the result does not match the schema.
fn reverted(error: &str) -> String {
    format!(
        "Failed to update composer.json with a valid format, reverting to the original content. Please report an issue to us with details (command you run and a copy of your composer.json). \n{error}"
    )
}

/// `VersionParser::normalizeStability()`.
fn normalize_stability(v: &str) -> String {
    let v = v.to_lowercase();
    if v == "rc" { "RC".into() } else { v }
}

struct ConfigCommand<'a> {
    io: &'a Io,
    args: &'a ConfigArgs,
    settings: Settings,
    source: ConfigSource,
    auth_source: ConfigSource,
}

impl ConfigCommand<'_> {
    /// `ConfigCommand::execute()` after the editor and config loading.
    fn execute(&self) -> Result<()> {
        let args = self.args;
        if args.list {
            let s = &self.settings;
            let all = json!({"repositories": s.repositories(), "config": s.all(false)});
            let raw = json!({"repositories": s.repositories(), "config": s.config.all()});
            self.list_configuration(&all, Some(&raw), None);
            return Ok(());
        }
        let Some(key) = args.setting_key.as_deref() else { return Ok(()) };
        let values = &args.setting_value;
        if !values.is_empty() && args.unset {
            bail!("You can not combine a setting value with --unset");
        }
        if values.is_empty() && !args.unset {
            return self.show(key);
        }
        self.set(key, values)
    }

    /// Prints a setting.
    fn show(&self, key: &str) -> Result<()> {
        let raw_data = json::read(&self.source.file)?;
        let mut source = self.settings.source_of(key);
        let value = if let Some(c) = REPOSITORY_KEY.captures(key) {
            let repos = self.settings.repositories();
            match c.get(1) {
                None => repos,
                Some(name) => php_get(&repos, name.as_str())
                    .filter(|v| !v.is_null())
                    .cloned()
                    .ok_or_else(|| anyhow!("There is no {} repository defined", name.as_str()))?,
            }
        } else if key.find('.').is_some_and(|i| i > 0) {
            let bits: Vec<&str> = key.split('.').collect();
            let mut data = if bits[0] == "extra" || bits[0] == "suggest" { raw_data } else { Value::Object(self.settings.all(false)) };
            // keys may contain dots: look up growing prefixes until one exists
            let (mut pending, mut found) = (None::<String>, false);
            for bit in bits {
                let k = pending.take().map_or_else(|| bit.to_string(), |p| format!("{p}.{bit}"));
                match php_get(&data, &k).filter(|v| !v.is_null()).cloned() {
                    Some(v) => (data, found) = (v, true),
                    None => (pending, found) = (Some(k), false),
                }
            }
            if !found {
                bail!("{key} is not defined.");
            }
            data
        } else if self.settings.config.all().contains_key(key) && !self.settings.get(key, false).is_null() {
            match self.settings.get(key, !self.args.absolute) {
                // ensure we get {} output for properties which are objects
                v if v.as_array().is_some_and(Vec::is_empty) && OBJECT_KEYS.contains(&key) => json!({}),
                v => v,
            }
        } else if let Some(v) = php_get(&raw_data, key).filter(|v| !v.is_null() && CONFIGURABLE_PACKAGE_PROPERTIES.contains(&key)) {
            source = self.source.file.display().to_string();
            v.clone()
        } else if let Some(v) = property_default(key) {
            source = "defaults".into();
            v
        } else {
            bail!("{key} is not defined");
        };
        let shown = match &value {
            Value::Array(_) | Value::Object(_) | Value::Bool(_) => {
                json::encode(&value, Flags { pretty: false, unescaped_slashes: true, unescaped_unicode: true })
            }
            v => php_string(v),
        };
        let source = if self.args.source { format!(" ({source})") } else { String::new() };
        self.io.write_at(&format!("{shown}{source}"), Verbosity::Quiet);
        Ok(())
    }

    /// `listConfiguration()`.
    fn list_configuration(&self, contents: &Value, raw: Option<&Value>, k: Option<&str>) {
        for (key, value) in entries(contents) {
            if k.is_none() && key != "config" && key != "repositories" {
                continue;
            }
            let raw_val = raw.and_then(|r| php_get(r, &key));
            let first_key_numeric = entries(value).next().is_some_and(|(first, _)| is_numeric(&first));
            if is_php_array(value) && (!first_key_numeric || (key == "repositories" && k.is_none())) {
                let sub = format!("{key}.");
                let prefix = format!("{}{}", k.unwrap_or(""), sub.strip_prefix("config.").unwrap_or(&sub));
                self.list_configuration(value, raw_val, Some(&prefix));
                continue;
            }
            let (shown, is_string) = match value {
                Value::Array(_) | Value::Object(_) => {
                    let items: Vec<String> =
                        entries(value).map(|(_, v)| if is_php_array(v) { json::encode(v, Flags::PLAIN) } else { php_string(v) }).collect();
                    (format!("[{}]", items.join(", ")), true)
                }
                Value::Bool(b) => (b.to_string(), true),
                Value::String(s) => (s.clone(), true),
                v => (php_string(v), false),
            };
            let label = format!("{}{key}", k.unwrap_or(""));
            let source = if self.args.source { format!(" ({})", self.settings.source_of(&label)) } else { String::new() };
            let shown = match raw_val {
                Some(Value::String(raw)) if !is_string || *raw != shown => format!("{raw} ({shown})"),
                _ => shown,
            };
            self.io.write_at(&format!("[<comment>{label}</comment>] <info>{shown}</info>{source}"), Verbosity::Quiet);
        }
    }

    /// Writes (or with `--unset` removes) a setting.
    fn set(&self, key: &str, values: &[String]) -> Result<()> {
        let args = self.args;
        let (src, auth) = (&self.source, &self.auth_source);
        let first = || values.first().map(String::as_str).unwrap_or_default();
        let list = || Value::Array(values.iter().map(|v| v.as_str().into()).collect());

        // unsetting audit and policy keys all resolve to the same call
        if args.unset && (key == "audit" || key == "policy" || key.starts_with("policy.")) {
            return src.remove_config_setting(key);
        }

        // policy.*.ignore and policy.advisories.ignore-id, with --json and --merge
        let custom_ignore = key
            .strip_prefix("policy.")
            .and_then(|k| k.strip_suffix(".ignore"))
            .filter(|list| !list.is_empty() && !list.contains('.') && !POLICY_NON_CUSTOM.contains(list));
        if ["policy.advisories.ignore-id", "policy.advisories.ignore", "policy.abandoned.ignore", "policy.malware.ignore"].contains(&key)
            || custom_ignore.is_some()
        {
            if let Some(list) = custom_ignore {
                reserved_policy_error(list)?;
            }
            let mut value = list();
            if args.json {
                value = parse_json(first())?;
                if !is_php_array(&value) {
                    bail!("Expected an array or object for {key}");
                }
            }
            if args.merge {
                let current =
                    std::iter::once("config").chain(key.split('.')).try_fold(json::read(&src.file)?, |cur, bit| php_get(&cur, bit).cloned());
                value = merge_with(value, current.as_ref(), key, true)?;
            }
            return src.add_config_setting(key, &value);
        }

        if key == "policy.ignore-unreachable" {
            let invalid = || anyhow!("valid values for {key} include: {}", IGNORE_UNREACHABLE_SCOPES.join(", "));
            if args.json {
                let value = parse_json(first())?;
                if !is_php_array(&value) {
                    bail!("Expected a boolean or array for {key}");
                }
                if !entries(&value).all(|(_, v)| v.as_str().is_some_and(|s| IGNORE_UNREACHABLE_SCOPES.contains(&s))) {
                    return Err(invalid());
                }
                return src.add_config_setting(key, &value);
            }
            // positional scopes; true/false fall through to the boolean setting
            if IGNORE_UNREACHABLE_SCOPES.contains(&first()) {
                if !values.iter().all(|v| IGNORE_UNREACHABLE_SCOPES.contains(&v.as_str())) {
                    return Err(invalid());
                }
                return src.add_config_setting(key, &list());
            }
        }

        // policy.<list> true|false enables or disables a whole list
        if let Some(list) = key.strip_prefix("policy.").filter(|l| !l.contains('.') && !l.is_empty() && *l != "ignore-unreachable") {
            reserved_policy_error(list)?;
            if !is_bool_str(first()) {
                bail!("\"{}\" is an invalid value for {key}, expected a boolean", first());
            }
            return src.add_config_setting(key, &php_bool(first()).into());
        }

        // custom lists: policy.<name>.block / policy.<name>.audit
        if let Some((list, field)) = key.strip_prefix("policy.").and_then(|k| k.split_once('.'))
            && !list.is_empty()
            && matches!(field, "block" | "audit")
            && !POLICY_NON_CUSTOM.contains(&list)
        {
            reserved_policy_error(list)?;
            if field == "block" {
                if !is_bool_str(first()) {
                    bail!("\"{}\" is an invalid value for {key}, expected a boolean", first());
                }
                return src.add_config_setting(key, &php_bool(first()).into());
            }
            if !matches!(first(), "ignore" | "report" | "fail") {
                bail!("\"{}\" is an invalid value for {key}, must be one of: ignore, report, fail", first());
            }
            return src.add_config_setting(key, &first().into());
        }

        if let Some((list, "sources")) = key.strip_prefix("policy.").and_then(|k| k.split_once('.')).filter(|(l, _)| !l.is_empty()) {
            bail!(
                "Setting dependency policy sources is not supported by `composer config`. Use `composer policy add-source {list} url <https-url>` instead."
            );
        }

        let unique = unique_config_value(key);
        let multi = multi_config_value(key);
        if args.unset && (unique.is_some() || multi.is_some()) {
            if key == "disable-tls" && self.settings.config.get_bool("disable-tls") {
                self.io.write_error("<info>You are now running Composer with SSL/TLS protection enabled.</info>");
            }
            return src.remove_config_setting(key);
        }
        if let Some(callbacks) = unique {
            let value = self.single_value(key, callbacks, values)?;
            if let Some(error) = schema_error(&format!("config.{key}"), &value) {
                bail!(reverted(&error));
            }
            return src.add_config_setting(key, &value);
        }
        if let Some(validate) = multi {
            validate_multi(validate, values)?;
            return src.add_config_setting(key, &list());
        }

        if key.strip_prefix("preferred-install.").is_some_and(|p| !p.is_empty()) {
            if args.unset {
                return src.remove_config_setting(key);
            }
            if !matches!(first(), "auto" | "source" | "dist") {
                bail!("Invalid value for {key}. Should be one of: auto, source, or dist");
            }
            return src.add_config_setting(key, &first().into());
        }

        static ALLOW_PLUGINS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^allow-plugins\.[a-zA-Z0-9/*-]+").unwrap());
        if ALLOW_PLUGINS.is_match(key) {
            if args.unset {
                return src.remove_config_setting(key);
            }
            if !is_bool_str(first()) {
                bail!("\"{}\" is an invalid value", first());
            }
            return src.add_config_setting(key, &php_bool(first()).into());
        }

        let prop = unique_prop(key);
        let multi_prop = matches!(key, "keywords" | "license");
        if args.global && (prop.is_some() || multi_prop || key.starts_with("extra.")) {
            bail!(
                "The {key} property can not be set in the global config.json file. Use `composer global config` to apply changes to the global composer.json"
            );
        }
        if args.unset && (prop.is_some() || multi_prop) {
            return src.remove_property(key);
        }
        if let Some(callbacks) = prop {
            if let ("minimum-stability", [v]) = (key, values)
                && !STABILITIES.contains(&normalize_stability(v).as_str())
            {
                bail!("Invalid stability string \"{}\", expected one of stable, RC, beta, alpha or dev", v.to_lowercase());
            }
            let value = self.single_value(key, callbacks, values)?;
            if let Some(error) = schema_error(key, &value) {
                bail!(reverted(&error));
            }
            return src.add_property(key, &value);
        }
        if multi_prop {
            return src.add_property(key, &list());
        }

        if let Some(name) = REPOSITORY_KEY.captures(key).and_then(|c| c.get(1)).map(|m| m.as_str()) {
            if args.unset {
                return src.remove_repository(name);
            }
            match values {
                [kind, url] => return src.add_repository(name, &json!({"type": kind, "url": url}), args.append),
                [value] if is_bool_str(&value.to_lowercase()) => {
                    if !php_bool(&value.to_lowercase()) {
                        return src.add_repository(name, &Value::Bool(false), args.append);
                    }
                }
                [value] => return src.add_repository(name, &parse_json(value)?, args.append),
                _ => {}
            }
            bail!("You must pass the type and a url. Example: php composer.phar config repositories.foo vcs https://bar.com");
        }

        if key.strip_prefix("extra.").is_some_and(|p| !p.is_empty()) {
            if args.unset {
                return src.remove_property(key);
            }
            let mut value: Value = first().into();
            if args.json {
                value = parse_json(first())?;
                if args.merge {
                    let current = key.split('.').try_fold(json::read(&src.file)?, |cur, bit| php_get(&cur, bit).cloned());
                    value = merge_with(value, current.as_ref(), key, false)?;
                }
            }
            return src.add_property(key, &value);
        }

        if key.strip_prefix("suggest.").is_some_and(|p| !p.is_empty()) {
            if args.unset {
                return src.remove_property(key);
            }
            return src.add_property(key, &values.join(" ").into());
        }

        if matches!(key, "suggest" | "extra") && args.unset {
            return src.remove_property(key);
        }

        if key.strip_prefix("platform.").is_some_and(|p| !p.is_empty()) {
            if args.unset {
                return src.remove_config_setting(key);
            }
            let value = if first() == "false" { Value::Bool(false) } else { first().into() };
            return src.add_config_setting(key, &value);
        }

        if key == "platform" && args.unset {
            return src.remove_config_setting(key);
        }

        if let Some(sub) = key.strip_prefix("audit.").filter(|s| matches!(*s, "ignore" | "ignore-abandoned")) {
            if args.unset {
                return src.remove_config_setting(key);
            }
            let mut value = list();
            if args.json {
                value = parse_json(first())?;
                if !is_php_array(&value) {
                    bail!("Expected an array or object for {key}");
                }
            }
            if args.merge {
                let current = json::read(&src.file)?;
                let current = ["config", "audit", sub].into_iter().try_fold(current, |cur, bit| php_get(&cur, bit).cloned());
                value = merge_with(value, current.as_ref(), key, true)?;
            }
            return src.add_config_setting(key, &value);
        }

        if let Some(c) = AUTH_KEY.captures(key) {
            let kind = &c[1];
            if args.unset {
                auth.remove_config_setting(key)?;
                return src.remove_config_setting(key);
            }
            let n = values.len();
            let value = match (kind, values) {
                ("bitbucket-oauth", [k, s]) => json!({"consumer-key": k, "consumer-secret": s}),
                ("bitbucket-oauth", _) => bail!("Expected two arguments (consumer-key, consumer-secret), got {n}"),
                ("gitlab-token", [user, token]) => json!({"username": user, "token": token}),
                ("github-oauth" | "gitlab-oauth" | "gitlab-token" | "bearer", [token]) => token.as_str().into(),
                ("github-oauth" | "gitlab-oauth" | "gitlab-token" | "bearer", _) => bail!("Too many arguments, expected only one token"),
                ("http-basic", [user, password]) => json!({"username": user, "password": password}),
                ("http-basic", _) => bail!("Expected two arguments (username, password), got {n}"),
                ("custom-headers", []) => bail!("Expected at least one argument (header), got none"),
                ("custom-headers", headers) => {
                    static HEADER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[^:]+:\s*.+$").unwrap());
                    if let Some(bad) = headers.iter().find(|h| !HEADER.is_match(h)) {
                        bail!("Header \"{bad}\" is not in \"Header-Name: Header-Value\" format");
                    }
                    list()
                }
                ("forgejo-token", [user, token]) => json!({"username": user, "token": token}),
                _ => bail!("Expected two arguments (username, access token), got {n}"),
            };
            src.remove_config_setting(key)?;
            return auth.add_config_setting(key, &value);
        }

        if key.strip_prefix("scripts.").is_some_and(|p| !p.is_empty()) {
            if args.unset {
                return src.remove_property(key);
            }
            let value = if values.len() > 1 { list() } else { first().into() };
            return src.add_property(key, &value);
        }

        if args.unset {
            return src.remove_property(key);
        }
        bail!("Setting {key} does not exist or is not supported by this command")
    }

    /// `handleSingleValue()`: the validated and normalized value.
    fn single_value(&self, key: &str, (validate, normalize): Single, values: &[String]) -> Result<Value> {
        let [value] = values else { bail!("You can only pass one value. Example: php composer.phar config process-timeout 300") };
        if !validate(value) {
            bail!("\"{value}\" is an invalid value");
        }
        let normalized = normalize(value);
        if key == "disable-tls" {
            let current = self.settings.config.get_bool("disable-tls");
            if normalized == false && current {
                self.io.write_error("<info>You are now running Composer with SSL/TLS protection enabled.</info>");
            } else if normalized == true && !current {
                self.io.write_error("<warning>You are now running Composer with SSL/TLS protection disabled.</warning>");
            }
        }
        Ok(normalized)
    }
}

/// `handleMultiValue()` validation.
fn validate_multi(validate: Multi, values: &[String]) -> Result<()> {
    validate(values).map_err(|reason| anyhow!("{} is an invalid value ({reason})", json::encode(&json!(values), Flags::PLAIN)))
}

/// `$propertiesDefaults`.
fn property_default(key: &str) -> Option<Value> {
    Some(match key {
        "type" => "library".into(),
        "description" | "homepage" => "".into(),
        "minimum-stability" => "stable".into(),
        "prefer-stable" => false.into(),
        "keywords" | "license" | "suggest" | "extra" => json!([]),
        _ => return None,
    })
}

/// `JsonConfigSource`: formatting-preserving edits of a composer.json, config.json or auth.json,
/// with a full rewrite when [`JsonManipulator`] cannot do them.
struct ConfigSource {
    file: PathBuf,
    /// an auth.json, whose keys are at the top level
    auth: bool,
}

impl ConfigSource {
    /// `addRepository()`.
    fn add_repository(&self, name: &str, config: &Value, append: bool) -> Result<()> {
        self.manipulate(
            |m| m.add_repository(name, config, append),
            |c| {
                let repos = repositories_as_list(c);
                if *config == Value::Bool(false) {
                    for repo in repos.iter_mut() {
                        if repo.get("name").and_then(Value::as_str) == Some(name) {
                            *repo = json!({ name: false });
                            return;
                        }
                        if *repo == json!({ name: false }) {
                            return;
                        }
                    }
                    repos.push(json!({ name: false }));
                    return;
                }
                let config = match config {
                    Value::Object(m) if !name.is_empty() => with_name(name, m.clone()),
                    config => config.clone(),
                };
                // Composer's uniqueness filter keeps every entry
                if append {
                    repos.push(config);
                } else {
                    repos.insert(0, config);
                }
            },
        )
    }

    /// `removeRepository()`.
    fn remove_repository(&self, name: &str) -> Result<()> {
        self.manipulate(
            |m| m.remove_repository(name),
            |c| {
                let repos = c.entry("repositories").or_insert(json!([]));
                let mut map = php_array(repos.take());
                *repos = match map.shift_remove(name) {
                    Some(_) => Value::Object(map),
                    None => Value::Array(map.into_values().collect()),
                };
                if is_empty_array(repos) {
                    c.shift_remove("repositories");
                }
            },
        )
    }

    /// `addConfigSetting()`.
    fn add_config_setting(&self, key: &str, value: &Value) -> Result<()> {
        self.manipulate(
            |m| match (self.auth, key.split_once('.')) {
                (true, Some((main, name))) => m.add_sub_node(main, name, value, true),
                _ => m.add_config_setting(key, value),
            },
            |c| {
                if let Some((k, host)) = key.split_once('.').filter(|_| HOST_KEY.is_match(key)) {
                    let target = if self.auth { c } else { node(c, "config") };
                    node(target, k).insert(host.into(), value.clone());
                } else if key.starts_with("policy.") {
                    let mut bits: Vec<&str> = key.split('.').collect();
                    let last = bits.pop().unwrap_or_default();
                    let target = bits.into_iter().fold(node(c, "config"), node);
                    target.insert(last.into(), value.clone());
                } else {
                    node(c, "config").insert(key.into(), value.clone());
                }
            },
        )
    }

    /// `removeConfigSetting()`.
    fn remove_config_setting(&self, key: &str) -> Result<()> {
        self.manipulate(
            |m| match (self.auth, key.split_once('.')) {
                (true, Some((main, name))) => m.remove_sub_node(main, name),
                _ => m.remove_config_setting(key),
            },
            |c| {
                if let Some((k, host)) = key.split_once('.').filter(|_| HOST_KEY.is_match(key)) {
                    let target = if self.auth { Some(c) } else { c.get_mut("config").and_then(Value::as_object_mut) };
                    if let Some(hosts) = target.and_then(|t| t.get_mut(k)).and_then(Value::as_object_mut) {
                        hosts.shift_remove(host);
                    }
                } else if key.starts_with("policy.") {
                    let mut bits: Vec<&str> = key.split('.').collect();
                    let last = bits.pop().unwrap_or_default();
                    let Some(target) = c.get_mut("config").and_then(|v| walk(v, &bits)) else { return };
                    target.shift_remove(last);
                    // drop now-empty ancestors within the policy subtree, config itself excluded
                    while let Some(leaf) = bits.pop() {
                        let Some(parent) = c.get_mut("config").and_then(|v| walk(v, &bits)) else { return };
                        if !parent.get(leaf).is_some_and(is_empty_array) {
                            break;
                        }
                        parent.shift_remove(leaf);
                    }
                } else if let Some(config) = c.get_mut("config").and_then(Value::as_object_mut) {
                    config.shift_remove(key);
                }
            },
        )
    }

    /// `addProperty()`.
    fn add_property(&self, key: &str, value: &Value) -> Result<()> {
        self.manipulate(
            |m| m.add_property(key, value),
            |c| {
                if key.starts_with("extra.") || key.starts_with("scripts.") {
                    let mut bits: Vec<&str> = key.split('.').collect();
                    let last = bits.pop().unwrap_or_default();
                    // Composer quirk: the walk starts below the first bit and then descends into every bit, the first included
                    let target = bits.iter().copied().fold(node(c, bits[0]), node);
                    target.insert(last.into(), value.clone());
                } else {
                    c.insert(key.into(), value.clone());
                }
            },
        )
    }

    /// `removeProperty()`.
    fn remove_property(&self, key: &str) -> Result<()> {
        self.manipulate(
            |m| m.remove_property(key),
            |c| {
                let lower = key.to_lowercase();
                if key.starts_with("extra.") || key.starts_with("scripts.") || lower.starts_with("autoload.") || lower.starts_with("autoload-dev.") {
                    let mut bits: Vec<&str> = key.split('.').collect();
                    let last = bits.pop().unwrap_or_default();
                    // the same quirk as in add_property()
                    if let Some(target) = c.get_mut(bits[0]).and_then(|v| walk(v, &bits)) {
                        target.shift_remove(last);
                    }
                } else {
                    c.shift_remove(key);
                }
            },
        )
    }

    /// `manipulateJson()`.
    fn manipulate(&self, edit: impl FnOnce(&mut JsonManipulator) -> bool, fallback: impl FnOnce(&mut Map<String, Value>)) -> Result<()> {
        let path = &self.file;
        let exists = path.exists();
        let contents = match (exists, self.auth) {
            (true, _) => {
                if std::fs::metadata(path)?.permissions().readonly() {
                    bail!("The file \"{}\" is not writable.", path.display());
                }
                std::fs::read_to_string(path).with_context(|| format!("The file \"{}\" is not readable.", path.display()))?
            }
            (false, true) => "{\n}\n".into(),
            (false, false) => "{\n    \"config\": {\n    }\n}\n".into(),
        };
        let mut m = JsonManipulator::new(&contents)?;
        if edit(&mut m) {
            std::fs::write(path, m.contents())?;
        } else {
            let Value::Object(mut data) = json::read(path)? else { bail!("\"{}\" does not contain a JSON object", path.display()) };
            fallback(&mut data);
            let mut data = json::php_normalize(Value::Object(data));
            empty_arrays_to_objects(&mut data);
            json::write(path, &data)?;
        }
        if !exists {
            chmod_private(path);
        }
        Ok(())
    }
}

/// `$arr[$key]` auto-vivified as an array, for writing below it.
fn node<'a>(parent: &'a mut Map<String, Value>, key: &str) -> &'a mut Map<String, Value> {
    let v = parent.entry(key).or_insert(Value::Null);
    if !v.is_object() {
        *v = Value::Object(php_array(v.take()));
    }
    v.as_object_mut().unwrap()
}

/// The array at `bits` below `v`, if every step exists and is an array.
fn walk<'a>(v: &'a mut Value, bits: &[&str]) -> Option<&'a mut Map<String, Value>> {
    if !v.is_object() && v.is_array() {
        *v = Value::Object(php_array(v.take()));
    }
    let map = v.as_object_mut()?;
    match bits.split_first() {
        None => Some(map),
        Some((first, rest)) => walk(map.get_mut(*first)?, rest),
    }
}

/// `JsonConfigSource::addRepository()`'s conversion of named repositories to a list.
fn repositories_as_list(c: &mut Map<String, Value>) -> &mut Vec<Value> {
    let repos = c.entry("repositories").or_insert(json!([]));
    if let Value::Object(m) = repos.take() {
        let list = m.into_iter().map(|(key, repo)| match repo {
            _ if is_int_key(&key) => repo,
            Value::Object(_) | Value::Array(_) => with_name(&key, php_array(repo)),
            repo => json!({ key: repo }),
        });
        *repos = Value::Array(list.collect());
    } else if !repos.is_array() {
        *repos = json!([]);
    }
    repos.as_array_mut().unwrap()
}

/// `['name' => $name] + $repository`, unless it is named already.
fn with_name(name: &str, repo: Map<String, Value>) -> Value {
    if repo.contains_key("name") {
        return Value::Object(repo);
    }
    Value::Object(std::iter::once(("name".into(), name.into())).chain(repo).collect())
}

/// Avoids ending up with `[]` for keys that should be objects after a full rewrite.
fn empty_arrays_to_objects(data: &mut Value) {
    let fix = |v: Option<&mut Value>| {
        if let Some(v) = v.filter(|v| v.as_array().is_some_and(Vec::is_empty)) {
            *v = json!({});
        }
    };
    if let Some(config) = data.get_mut("config") {
        if let Some(policy) = config.get_mut("policy") {
            if let Some(lists) = policy.as_object_mut() {
                lists.values_mut().for_each(|v| fix(Some(v)));
            }
            fix(Some(policy));
        }
        for key in [
            "platform",
            "http-basic",
            "bearer",
            "gitlab-token",
            "gitlab-oauth",
            "github-oauth",
            "custom-headers",
            "forgejo-token",
            "preferred-install",
        ] {
            fix(config.get_mut(key));
        }
    }
    for autoload in ["autoload", "autoload-dev"] {
        for key in ["psr-0", "psr-4"] {
            fix(data.get_mut(autoload).and_then(|a| a.get_mut(key)));
        }
    }
    for key in [
        "require",
        "require-dev",
        "conflict",
        "provide",
        "replace",
        "suggest",
        "config",
        "autoload",
        "autoload-dev",
        "scripts",
        "scripts-descriptions",
        "scripts-aliases",
        "support",
    ] {
        fix(data.get_mut(key));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Cli, Command};
    use clap::Parser;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A project directory with its own COMPOSER_HOME, removed on drop.
    struct Project(PathBuf);

    impl Project {
        fn new(composer_json: &str) -> Project {
            static N: AtomicUsize = AtomicUsize::new(0);
            let dir = std::env::temp_dir().join(format!("komposer-config-test-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
            std::fs::create_dir_all(dir.join("home")).unwrap();
            let p = Project(dir);
            std::fs::write(p.file(), json::encode(&serde_json::from_str(composer_json).unwrap(), Flags::FILE) + "\n").unwrap();
            p
        }

        fn file(&self) -> PathBuf {
            self.0.join("composer.json")
        }

        /// `config --file <project>/composer.json <argv>`; the output.
        fn config(&self, argv: &[&str]) -> Result<String> {
            let file = self.file().display().to_string();
            let cli = Cli::try_parse_from(["komposer", "config", "--file", &file].iter().chain(argv))?;
            let Some(Command::Config(args)) = cli.command else { unreachable!() };
            let io = Io::buffered(Verbosity::Normal);
            run(&io, &args, &self.0, &self.0.join("home").display().to_string())?;
            Ok(io.output())
        }

        fn json(&self) -> Value {
            json::read(&self.file()).unwrap()
        }
    }

    impl Drop for Project {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn check_cases<T>(cases: &[T], run: impl Fn(&T) -> Result<(), String>) {
        let failures: Vec<String> = cases.iter().filter_map(|c| run(c).err()).collect();
        assert!(failures.is_empty(), "{} of {} cases failed:\n{}", failures.len(), cases.len(), failures.join("\n"));
    }

    /// `ConfigCommandTest::provideConfigUpdates()`: (label, composer.json, arguments, expected composer.json)
    const UPDATES: &[(&str, &str, &[&str], &str)] = &[
        ("set scripts", "{}", &["scripts.test", "foo bar"], r#"{"scripts":{"test":"foo bar"}}"#),
        ("unset scripts", r#"{"scripts":{"test":"foo bar","lala":"baz"}}"#, &["scripts.lala", "--unset"], r#"{"scripts":{"test":"foo bar"}}"#),
        ("set single config with bool normalizer", "{}", &["use-github-api", "1"], r#"{"config":{"use-github-api":true}}"#),
        ("set multi config", "{}", &["github-protocols", "https", "git"], r#"{"config":{"github-protocols":["https","git"]}}"#),
        ("set version", "{}", &["version", "1.0.0"], r#"{"version":"1.0.0"}"#),
        ("unset version", r#"{"version":"1.0.0"}"#, &["version", "--unset"], "{}"),
        ("unset arbitrary property", r#"{"random-prop":"1.0.0"}"#, &["random-prop", "--unset"], "{}"),
        ("set preferred-install", "{}", &["preferred-install.foo/*", "source"], r#"{"config":{"preferred-install":{"foo/*":"source"}}}"#),
        (
            "unset preferred-install",
            r#"{"config":{"preferred-install":{"foo/*":"source"}}}"#,
            &["preferred-install.foo/*", "--unset"],
            r#"{"config":{"preferred-install":[]}}"#,
        ),
        (
            "unset platform",
            r#"{"config":{"platform":{"php":"7.2.5"},"platform-check":false}}"#,
            &["platform.php", "--unset"],
            r#"{"config":{"platform":[],"platform-check":false}}"#,
        ),
        (
            "set extra with merge",
            "{}",
            &["extra.patches.foo/bar", r#"{"123":"value"}"#, "--json", "--merge"],
            r#"{"extra":{"patches":{"foo/bar":{"123":"value"}}}}"#,
        ),
        (
            "combine extra with merge",
            r#"{"extra":{"patches":{"foo/bar":{"5":"oldvalue"}}}}"#,
            &["extra.patches.foo/bar", r#"{"123":"value"}"#, "--json", "--merge"],
            r#"{"extra":{"patches":{"foo/bar":{"123":"value","5":"oldvalue"}}}}"#,
        ),
        (
            "combine extra with list",
            r#"{"extra":{"patches":{"foo/bar":["oldvalue"]}}}"#,
            &["extra.patches.foo/bar", r#"{"123":"value"}"#, "--json", "--merge"],
            r#"{"extra":{"patches":{"foo/bar":{"123":"value","0":"oldvalue"}}}}"#,
        ),
        (
            "overwrite extra with merge",
            r#"{"extra":{"patches":{"foo/bar":{"123":"oldvalue"}}}}"#,
            &["extra.patches.foo/bar", r#"{"123":"value"}"#, "--json", "--merge"],
            r#"{"extra":{"patches":{"foo/bar":{"123":"value"}}}}"#,
        ),
        (
            "unset autoload",
            r#"{"autoload":{"psr-4":["test"],"classmap":["test"]}}"#,
            &["autoload.psr-4", "--unset"],
            r#"{"autoload":{"classmap":["test"]}}"#,
        ),
        (
            "unset autoload-dev",
            r#"{"autoload-dev":{"psr-4":["test"],"classmap":["test"]}}"#,
            &["autoload-dev.psr-4", "--unset"],
            r#"{"autoload-dev":{"classmap":["test"]}}"#,
        ),
        ("set audit.ignore-unreachable", "{}", &["audit.ignore-unreachable", "true"], r#"{"config":{"audit":{"ignore-unreachable":true}}}"#),
        ("set audit.block-insecure", "{}", &["audit.block-insecure", "false"], r#"{"config":{"audit":{"block-insecure":false}}}"#),
        ("set audit.block-abandoned", "{}", &["audit.block-abandoned", "true"], r#"{"config":{"audit":{"block-abandoned":true}}}"#),
        (
            "unset audit.ignore-unreachable",
            r#"{"config":{"audit":{"ignore-unreachable":true}}}"#,
            &["audit.ignore-unreachable", "--unset"],
            r#"{"config":{"audit":[]}}"#,
        ),
        (
            "set audit.ignore-severity",
            "{}",
            &["audit.ignore-severity", "low", "medium"],
            r#"{"config":{"audit":{"ignore-severity":["low","medium"]}}}"#,
        ),
        (
            "set audit.ignore as array",
            "{}",
            &["audit.ignore", r#"["CVE-2024-1234","GHSA-xxxx-yyyy"]"#, "--json"],
            r#"{"config":{"audit":{"ignore":["CVE-2024-1234","GHSA-xxxx-yyyy"]}}}"#,
        ),
        (
            "set audit.ignore as object",
            "{}",
            &["audit.ignore", r#"{"CVE-2024-1234":"False positive","GHSA-xxxx-yyyy":"Not applicable"}"#, "--json"],
            r#"{"config":{"audit":{"ignore":{"CVE-2024-1234":"False positive","GHSA-xxxx-yyyy":"Not applicable"}}}}"#,
        ),
        (
            "merge audit.ignore array",
            r#"{"config":{"audit":{"ignore":["CVE-2024-1234"]}}}"#,
            &["audit.ignore", r#"["CVE-2024-5678"]"#, "--json", "--merge"],
            r#"{"config":{"audit":{"ignore":["CVE-2024-1234","CVE-2024-5678"]}}}"#,
        ),
        (
            "merge audit.ignore object",
            r#"{"config":{"audit":{"ignore":{"CVE-2024-1234":"Old reason"}}}}"#,
            &["audit.ignore", r#"{"CVE-2024-5678":"New advisory"}"#, "--json", "--merge"],
            r#"{"config":{"audit":{"ignore":{"CVE-2024-5678":"New advisory","CVE-2024-1234":"Old reason"}}}}"#,
        ),
        (
            "overwrite audit.ignore key with merge",
            r#"{"config":{"audit":{"ignore":{"CVE-2024-1234":"Old reason"}}}}"#,
            &["audit.ignore", r#"{"CVE-2024-1234":"New reason"}"#, "--json", "--merge"],
            r#"{"config":{"audit":{"ignore":{"CVE-2024-1234":"New reason"}}}}"#,
        ),
        (
            "set audit.ignore-abandoned as array",
            "{}",
            &["audit.ignore-abandoned", r#"["vendor/package1","vendor/package2"]"#, "--json"],
            r#"{"config":{"audit":{"ignore-abandoned":["vendor/package1","vendor/package2"]}}}"#,
        ),
        (
            "set audit.ignore-abandoned as object",
            "{}",
            &["audit.ignore-abandoned", r#"{"vendor/package1":"Still maintained","vendor/package2":"Fork available"}"#, "--json"],
            r#"{"config":{"audit":{"ignore-abandoned":{"vendor/package1":"Still maintained","vendor/package2":"Fork available"}}}}"#,
        ),
        (
            "merge audit.ignore-abandoned array",
            r#"{"config":{"audit":{"ignore-abandoned":["vendor/package1"]}}}"#,
            &["audit.ignore-abandoned", r#"["vendor/package2"]"#, "--json", "--merge"],
            r#"{"config":{"audit":{"ignore-abandoned":["vendor/package1","vendor/package2"]}}}"#,
        ),
        (
            "merge audit.ignore-abandoned object",
            r#"{"config":{"audit":{"ignore-abandoned":{"vendor/package1":"Old reason"}}}}"#,
            &["audit.ignore-abandoned", r#"{"vendor/package2":"New reason"}"#, "--json", "--merge"],
            r#"{"config":{"audit":{"ignore-abandoned":{"vendor/package2":"New reason","vendor/package1":"Old reason"}}}}"#,
        ),
        ("unset audit.ignore", r#"{"config":{"audit":{"ignore":["CVE-2024-1234"]}}}"#, &["audit.ignore", "--unset"], r#"{"config":{"audit":[]}}"#),
        (
            "unset audit.ignore-abandoned",
            r#"{"config":{"audit":{"ignore-abandoned":["vendor/package1"]}}}"#,
            &["audit.ignore-abandoned", "--unset"],
            r#"{"config":{"audit":[]}}"#,
        ),
        ("unset policy", r#"{"config":{"policy":{"advisories":false}}}"#, &["policy", "--unset"], r#"{"config":[]}"#),
        (
            "set policy.advisories.block false via 0",
            "{}",
            &["policy.advisories.block", "0"],
            r#"{"config":{"policy":{"advisories":{"block":false}}}}"#,
        ),
        ("set policy.advisories.block true via 1", "{}", &["policy.advisories.block", "1"], r#"{"config":{"policy":{"advisories":{"block":true}}}}"#),
        ("set policy.advisories.audit", "{}", &["policy.advisories.audit", "report"], r#"{"config":{"policy":{"advisories":{"audit":"report"}}}}"#),
        ("set policy.malware.block false", "{}", &["policy.malware.block", "false"], r#"{"config":{"policy":{"malware":{"block":false}}}}"#),
        (
            "set policy.malware.block-scope",
            "{}",
            &["policy.malware.block-scope", "install"],
            r#"{"config":{"policy":{"malware":{"block-scope":"install"}}}}"#,
        ),
        ("set policy.malware.audit", "{}", &["policy.malware.audit", "ignore"], r#"{"config":{"policy":{"malware":{"audit":"ignore"}}}}"#),
        ("set policy.abandoned.block true", "{}", &["policy.abandoned.block", "true"], r#"{"config":{"policy":{"abandoned":{"block":true}}}}"#),
        ("set policy.abandoned.audit", "{}", &["policy.abandoned.audit", "fail"], r#"{"config":{"policy":{"abandoned":{"audit":"fail"}}}}"#),
        (
            "set policy.ignore-unreachable bool true",
            "{}",
            &["policy.ignore-unreachable", "true"],
            r#"{"config":{"policy":{"ignore-unreachable":true}}}"#,
        ),
        (
            "set policy.ignore-unreachable bool false",
            "{}",
            &["policy.ignore-unreachable", "false"],
            r#"{"config":{"policy":{"ignore-unreachable":false}}}"#,
        ),
        (
            "set policy.ignore-unreachable positional single enum value",
            "{}",
            &["policy.ignore-unreachable", "update"],
            r#"{"config":{"policy":{"ignore-unreachable":["update"]}}}"#,
        ),
        (
            "set policy.ignore-unreachable positional multi enum values",
            "{}",
            &["policy.ignore-unreachable", "update", "install"],
            r#"{"config":{"policy":{"ignore-unreachable":["update","install"]}}}"#,
        ),
        (
            "set policy.ignore-unreachable via --json array form",
            "{}",
            &["policy.ignore-unreachable", r#"["update","install"]"#, "--json"],
            r#"{"config":{"policy":{"ignore-unreachable":["update","install"]}}}"#,
        ),
        (
            "set policy.advisories.block alongside existing audit setting",
            r#"{"config":{"policy":{"advisories":{"audit":"report"}}}}"#,
            &["policy.advisories.block", "false"],
            r#"{"config":{"policy":{"advisories":{"audit":"report","block":false}}}}"#,
        ),
        (
            "set policy.malware.block alongside existing advisories",
            r#"{"config":{"policy":{"advisories":{"block":false}}}}"#,
            &["policy.malware.block", "true"],
            r#"{"config":{"policy":{"advisories":{"block":false},"malware":{"block":true}}}}"#,
        ),
        ("set custom policy list block", "{}", &["policy.my-list.block", "true"], r#"{"config":{"policy":{"my-list":{"block":true}}}}"#),
        ("set custom policy list audit", "{}", &["policy.my-list.audit", "report"], r#"{"config":{"policy":{"my-list":{"audit":"report"}}}}"#),
        (
            "unset policy.advisories.block leaves siblings",
            r#"{"config":{"policy":{"advisories":{"block":false,"audit":"fail"}}}}"#,
            &["policy.advisories.block", "--unset"],
            r#"{"config":{"policy":{"advisories":{"audit":"fail"}}}}"#,
        ),
        (
            "unset policy.ignore-unreachable leaves siblings",
            r#"{"config":{"policy":{"ignore-unreachable":true,"advisories":{"block":true}}}}"#,
            &["policy.ignore-unreachable", "--unset"],
            r#"{"config":{"policy":{"advisories":{"block":true}}}}"#,
        ),
        (
            "unset last policy list sub-key removes the list",
            r#"{"config":{"policy":{"advisories":{"block":false}}}}"#,
            &["policy.advisories.block", "--unset"],
            r#"{"config":{"policy":[]}}"#,
        ),
        (
            "unset last sub-key of list keeps sibling lists",
            r#"{"config":{"policy":{"advisories":{"block":false},"malware":{"block":true}}}}"#,
            &["policy.advisories.block", "--unset"],
            r#"{"config":{"policy":{"malware":{"block":true}}}}"#,
        ),
        (
            "unset only policy.ignore-unreachable",
            r#"{"config":{"policy":{"ignore-unreachable":true}}}"#,
            &["policy.ignore-unreachable", "--unset"],
            r#"{"config":{"policy":[]}}"#,
        ),
        (
            "set policy.advisories.ignore as array",
            "{}",
            &["policy.advisories.ignore", r#"["CVE-2024-1234"]"#, "--json"],
            r#"{"config":{"policy":{"advisories":{"ignore":["CVE-2024-1234"]}}}}"#,
        ),
        (
            "set policy.advisories.ignore as object",
            "{}",
            &["policy.advisories.ignore", r#"{"CVE-2024-1234":"False positive"}"#, "--json"],
            r#"{"config":{"policy":{"advisories":{"ignore":{"CVE-2024-1234":"False positive"}}}}}"#,
        ),
        (
            "merge policy.advisories.ignore array",
            r#"{"config":{"policy":{"advisories":{"ignore":["CVE-2024-1234"]}}}}"#,
            &["policy.advisories.ignore", r#"["CVE-2024-5678"]"#, "--json", "--merge"],
            r#"{"config":{"policy":{"advisories":{"ignore":["CVE-2024-1234","CVE-2024-5678"]}}}}"#,
        ),
        (
            "merge policy.advisories.ignore object",
            r#"{"config":{"policy":{"advisories":{"ignore":{"CVE-2024-1234":"Old reason"}}}}}"#,
            &["policy.advisories.ignore", r#"{"CVE-2024-5678":"New advisory"}"#, "--json", "--merge"],
            r#"{"config":{"policy":{"advisories":{"ignore":{"CVE-2024-5678":"New advisory","CVE-2024-1234":"Old reason"}}}}}"#,
        ),
        (
            "set policy.advisories.ignore-severity",
            "{}",
            &["policy.advisories.ignore-severity", "low", "medium"],
            r#"{"config":{"policy":{"advisories":{"ignore-severity":["low","medium"]}}}}"#,
        ),
        (
            "set policy.advisories.ignore-id as array",
            "{}",
            &["policy.advisories.ignore-id", r#"["CVE-2024-1234","GHSA-xxxx-yyyy"]"#, "--json"],
            r#"{"config":{"policy":{"advisories":{"ignore-id":["CVE-2024-1234","GHSA-xxxx-yyyy"]}}}}"#,
        ),
        (
            "set policy.malware.ignore as array",
            "{}",
            &["policy.malware.ignore", r#"["vendor/pkg"]"#, "--json"],
            r#"{"config":{"policy":{"malware":{"ignore":["vendor/pkg"]}}}}"#,
        ),
        (
            "set policy.malware.ignore-source",
            "{}",
            &["policy.malware.ignore-source", "source-a", "source-b"],
            r#"{"config":{"policy":{"malware":{"ignore-source":["source-a","source-b"]}}}}"#,
        ),
        (
            "set policy.abandoned.ignore as array",
            "{}",
            &["policy.abandoned.ignore", r#"["vendor/pkg"]"#, "--json"],
            r#"{"config":{"policy":{"abandoned":{"ignore":["vendor/pkg"]}}}}"#,
        ),
        (
            "set policy.ignore-unreachable as array via json",
            "{}",
            &["policy.ignore-unreachable", r#"["install","update"]"#, "--json"],
            r#"{"config":{"policy":{"ignore-unreachable":["install","update"]}}}"#,
        ),
        (
            "set custom policy list ignore",
            "{}",
            &["policy.my-list.ignore", r#"["vendor/pkg"]"#, "--json"],
            r#"{"config":{"policy":{"my-list":{"ignore":["vendor/pkg"]}}}}"#,
        ),
        ("set policy.malware false disables whole list", "{}", &["policy.malware", "false"], r#"{"config":{"policy":{"malware":false}}}"#),
        ("set policy.advisories true via 1 enables whole list", "{}", &["policy.advisories", "1"], r#"{"config":{"policy":{"advisories":true}}}"#),
        ("set policy.abandoned false", "{}", &["policy.abandoned", "false"], r#"{"config":{"policy":{"abandoned":false}}}"#),
        (
            "set policy.<list> false overwrites existing object",
            r#"{"config":{"policy":{"malware":{"block":true,"audit":"fail"}}}}"#,
            &["policy.malware", "false"],
            r#"{"config":{"policy":{"malware":false}}}"#,
        ),
        (
            "set policy.<builtin> false preserves sibling lists",
            r#"{"config":{"policy":{"advisories":{"block":true,"audit":"fail"},"malware":{"block":true},"ignore-unreachable":true}}}"#,
            &["policy.advisories", "false"],
            r#"{"config":{"policy":{"advisories":false,"malware":{"block":true},"ignore-unreachable":true}}}"#,
        ),
        ("set custom policy list false", "{}", &["policy.my-custom", "false"], r#"{"config":{"policy":{"my-custom":false}}}"#),
        (
            "unset policy.<list> removes the list entry",
            r#"{"config":{"policy":{"malware":false,"advisories":{"block":true}}}}"#,
            &["policy.malware", "--unset"],
            r#"{"config":{"policy":{"advisories":{"block":true}}}}"#,
        ),
        ("unset only policy.<list>", r#"{"config":{"policy":{"malware":false}}}"#, &["policy.malware", "--unset"], r#"{"config":{"policy":[]}}"#),
    ];

    /// `ConfigCommandTest::provideConfigReads()`: (label, composer.json, arguments, expected output)
    const READS: &[(&str, &str, &[&str], &str)] = &[
        ("read description", r#"{"description":"foo bar"}"#, &["description"], "foo bar"),
        ("read vendor-dir with source", r#"{"config":{"vendor-dir":"lala"}}"#, &["vendor-dir", "--source"], "lala (./composer.json)"),
        ("read default vendor-dir", "{}", &["vendor-dir"], "vendor"),
        (
            "read repos by named key",
            r#"{"repositories":{"foo":{"type":"vcs","url":"https://example.org"},"packagist.org":{"type":"composer","url":"https://repo.packagist.org"}}}"#,
            &["repositories.foo"],
            r#"{"type":"vcs","url":"https://example.org"}"#,
        ),
        (
            "read repos by numeric index",
            r#"{"repositories":{"0":{"type":"vcs","url":"https://example.org"},"packagist.org":{"type":"composer","url":"https://repo.packagist.org"}}}"#,
            &["repos.0"],
            r#"{"type":"vcs","url":"https://example.org"}"#,
        ),
        (
            "read all repos includes the default packagist",
            r#"{"repositories":{"foo":{"type":"vcs","url":"https://example.org"},"packagist.org":{"type":"composer","url":"https://repo.packagist.org"}}}"#,
            &["repos"],
            r#"{"foo":{"type":"vcs","url":"https://example.org"},"packagist.org":{"type":"composer","url":"https://repo.packagist.org"}}"#,
        ),
        (
            "read all repos does not include the disabled packagist",
            r#"{"repositories":{"foo":{"type":"vcs","url":"https://example.org"},"packagist.org":false}}"#,
            &["repos"],
            r#"{"foo":{"type":"vcs","url":"https://example.org"}}"#,
        ),
    ];

    #[test]
    fn config_updates() {
        check_cases(UPDATES, |&(label, before, argv, expected)| {
            let p = Project::new(before);
            p.config(argv).map_err(|e| format!("{label}: {e:#}"))?;
            let expected = json::parse(expected).unwrap();
            if p.json() != expected { Err(format!("{label}: got {} expected {expected}", p.json())) } else { Ok(()) }
        });
    }

    #[test]
    fn config_reads() {
        check_cases(READS, |&(label, before, argv, expected)| {
            let p = Project::new(before);
            let original = std::fs::read_to_string(p.file()).unwrap();
            let out = p.config(argv).map_err(|e| format!("{label}: {e:#}"))?;
            let expected = expected.replace("./composer.json", &p.file().display().to_string());
            match () {
                _ if out.trim() != expected => Err(format!("{label}: got {out:?} expected {expected:?}")),
                _ if std::fs::read_to_string(p.file()).unwrap() != original => Err(format!("{label}: composer.json was modified")),
                _ => Ok(()),
            }
        });
    }

    /// `ConfigCommandTest::testConfigThrows*()`: (composer.json, arguments, expected message part)
    const THROWS: &[(&str, &[&str], &str)] = &[
        ("{}", &["--global"], "--file and --global can not be combined"),
        ("{}", &["audit.ignore-severity", "low", "invalid"], "valid severities include: low, medium, high, critical"),
        (
            r#"{"config":{"audit":{"ignore":["CVE-2024-1234"]}}}"#,
            &["audit.ignore", r#"{"CVE-2024-5678":"reason"}"#, "--json", "--merge"],
            "Cannot merge array and object",
        ),
        (
            r#"{"config":{"policy":{"advisories":{"ignore":["CVE-2024-1234"]}}}}"#,
            &["policy.advisories.ignore", r#"{"CVE-2024-5678":"reason"}"#, "--json", "--merge"],
            "Cannot merge array and object",
        ),
        ("{}", &["policy.advisories.audit", "bogus"], "\"bogus\" is an invalid value"),
        ("{}", &["policy.malware.block-scope", "bogus"], "\"bogus\" is an invalid value"),
        ("{}", &["policy.advisories.ignore-severity", "low", "bogus"], "valid severities include: low, medium, high, critical"),
        (
            "{}",
            &["policy.ignore-unreachable", r#"["bogus"]"#, "--json"],
            "valid values for policy.ignore-unreachable include: audit, install, update",
        ),
        ("{}", &["policy.malware", "bogus"], "expected a boolean"),
        ("{}", &["policy.ignore-foo", "true"], "reserved prefix \"ignore\""),
        ("{}", &["policy.ignore-foo.block", "true"], "reserved prefix \"ignore\""),
        ("{}", &["policy.support.audit", "fail"], "reserved for future use"),
        ("{}", &["policy.ignore-foo.ignore", r#"["CVE-2024-1234"]"#], "reserved prefix \"ignore\""),
        ("{}", &["name", "Foo/Bar"], "name : Does not match the regex pattern"),
        ("{}", &["minimum-stability", "bogus"], "Invalid stability string \"bogus\""),
    ];

    #[test]
    fn config_throws() {
        check_cases(THROWS, |&(before, argv, message)| {
            let p = Project::new(before);
            let original = std::fs::read_to_string(p.file()).unwrap();
            match p.config(argv) {
                Ok(_) => Err(format!("{argv:?}: no error")),
                Err(e) if !format!("{e:#}").contains(message) => Err(format!("{argv:?}: {e:#}")),
                Err(_) if std::fs::read_to_string(p.file()).unwrap() != original => Err(format!("{argv:?}: composer.json was modified")),
                Err(_) => Ok(()),
            }
        });
    }

    #[test]
    fn lists_with_sources() {
        let p = Project::new(
            r#"{"config":{"vendor-dir":"lib","allow-plugins":{"a/b":true}},"repositories":[{"type":"vcs","url":"https://example.org/x"}]}"#,
        );
        let out = p.config(&["--list", "--source"]).unwrap();
        let file = p.file().display().to_string();
        for line in [
            format!("[repositories.0.type] vcs ({file})"),
            "[repositories.packagist.org.url] https://repo.packagist.org (default)".into(),
            format!("[allow-plugins.a/b] true ({file})"),
            "[audit.abandoned] fail (default)".into(),
            "[github-protocols] [https, ssh] (default)".into(),
            format!("[vendor-dir] lib ({}/lib) ({file})", p.0.display()),
            "[cache-files-maxsize] 300MiB (314572800) (default)".into(),
        ] {
            assert!(out.lines().any(|l| l == line), "missing {line:?} in\n{out}");
        }
        assert!(!out.lines().any(|l| l.starts_with("[platform]") || l.starts_with("[github-oauth]")), "empty arrays are not listed");
    }

    #[test]
    fn auth_settings_go_to_auth_json() {
        let p = Project::new(r#"{"config":{"github-oauth":{"github.com":"old"}}}"#);
        p.config(&["github-oauth.github.com", "tok"]).unwrap();
        p.config(&["http-basic.repo.example.org", "user", "pass"]).unwrap();
        assert_eq!(p.json(), json!({"config": {"github-oauth": []}}));
        let auth = std::fs::read_to_string(p.0.join("auth.json")).unwrap();
        assert_eq!(
            auth,
            "{\n    \"github-oauth\": {\n        \"github.com\": \"tok\"\n    },\n    \"http-basic\": {\n        \"repo.example.org\": {\n            \"username\": \"user\",\n            \"password\": \"pass\"\n        }\n    }\n}\n"
        );
        p.config(&["--unset", "github-oauth.github.com"]).unwrap();
        assert!(std::fs::read_to_string(p.0.join("auth.json")).unwrap().starts_with("{\n    \"github-oauth\": {\n    },"));
    }

    #[test]
    fn global_creates_config_and_auth_files() {
        let p = Project::new("{}");
        let home = p.0.join("home");
        let cli = Cli::try_parse_from(["komposer", "config", "--global", "sort-packages", "true"]).unwrap();
        let Some(Command::Config(args)) = cli.command else { unreachable!() };
        run(&Io::null(), &args, &p.0, &home.display().to_string()).unwrap();
        // Composer quirk: adding to the inline `{}` leaves the closing brace on the same line
        assert_eq!(std::fs::read_to_string(home.join("config.json")).unwrap(), "{\n    \"config\": {\n        \"sort-packages\": true}\n}\n");
        assert!(std::fs::read_to_string(home.join("auth.json")).unwrap().contains("\"forgejo-token\": {}"));
    }

    /// The `Config/Fixtures` files `JsonConfigSourceTest` uses.
    const REPOSITORIES: &str = "{\n    \"name\": \"my-vend/my-app\",\n    \"license\": \"MIT\",\n    \"repositories\": {\n    }\n}\n";
    const EMPTY: &str = "{\n    \"name\": \"my-vend/my-app\",\n    \"license\": \"MIT\"\n}\n";
    const WITH_EXAMPLE_TLD: &str = "{\n    \"name\": \"my-vend/my-app\",\n    \"license\": \"MIT\",\n    \"repositories\": [\n        {\n            \"name\": \"example_tld\",\n            \"type\": \"git\",\n            \"url\": \"example.tld\"\n        }\n    ]\n}\n";
    const WITH_PACKAGIST_FALSE: &str = "{\n    \"name\": \"my-vend/my-app\",\n    \"license\": \"MIT\",\n    \"repositories\": [\n        {\n            \"packagist\": false\n        }\n    ]\n}\n";

    /// Applies `edit` to a composer.json with `contents`, returns the result.
    fn edit_source(contents: &str, edit: impl FnOnce(&ConfigSource) -> Result<()>) -> String {
        let p = Project::new("{}");
        std::fs::write(p.file(), contents).unwrap();
        edit(&ConfigSource { file: p.file(), auth: false }).unwrap();
        std::fs::read_to_string(p.file()).unwrap()
    }

    #[test]
    fn json_config_source_repositories() {
        let tld = json!({"type": "git", "url": "example.tld"});
        assert_eq!(edit_source(REPOSITORIES, |s| s.add_repository("example_tld", &tld, true)), WITH_EXAMPLE_TLD);
        assert_eq!(
            edit_source(REPOSITORIES, |s| s.add_repository("", &tld, true)),
            "{\n    \"name\": \"my-vend/my-app\",\n    \"license\": \"MIT\",\n    \"repositories\": [\n        {\n            \"type\": \"git\",\n            \"url\": \"example.tld\"\n        }\n    ]\n}\n"
        );
        let with_options =
            json!({"type": "composer", "url": "https://example.tld", "options": {"ssl": {"local_cert": "/home/composer/.ssl/composer.pem"}}});
        assert_eq!(
            edit_source(REPOSITORIES, |s| s.add_repository("example_tld", &with_options, true)),
            "{\n    \"name\": \"my-vend/my-app\",\n    \"license\": \"MIT\",\n    \"repositories\": [\n        {\n            \"name\": \"example_tld\",\n            \"type\": \"composer\",\n            \"url\": \"https://example.tld\",\n            \"options\": {\n                \"ssl\": {\n                    \"local_cert\": \"/home/composer/.ssl/composer.pem\"\n                }\n            }\n        }\n    ]\n}\n"
        );
        assert_eq!(edit_source(WITH_EXAMPLE_TLD, |s| s.remove_repository("example_tld")), EMPTY);
        assert_eq!(edit_source(REPOSITORIES, |s| s.add_repository("packagist", &Value::Bool(false), true)), WITH_PACKAGIST_FALSE);
        assert_eq!(edit_source(WITH_PACKAGIST_FALSE, |s| s.remove_repository("packagist")), EMPTY);
    }

    #[test]
    fn json_config_source_policy_list_fields() {
        let before = "{\n    \"name\": \"vendor/pkg\", \"config\": {\n        \"vendor-dir\": \"vendor\", \"sort-packages\": true,\n        \"policy\": {\n            \"advisories\": {\n                \"audit\": \"report\"\n            },\n            \"abandoned\": {\n                \"block\": true\n            }\n        }\n    }\n}\n";
        assert_eq!(
            edit_source(before, |s| s.add_config_setting("policy.advisories.block", &json!(true))),
            "{\n    \"name\": \"vendor/pkg\", \"config\": {\n        \"vendor-dir\": \"vendor\", \"sort-packages\": true,\n        \"policy\": {\n            \"advisories\": {\n                \"audit\": \"report\",\n                \"block\": true\n            },\n            \"abandoned\": {\n                \"block\": true\n            }\n        }\n    }\n}\n"
        );
        let before = "{\n    \"name\": \"vendor/pkg\", \"config\": {\n        \"vendor-dir\": \"vendor\", \"sort-packages\": true,\n        \"policy\": {\n            \"advisories\": {\n                \"audit\": \"report\",\n                \"block\": true\n            }\n        }\n    }\n}\n";
        assert_eq!(
            edit_source(before, |s| s.remove_config_setting("policy.advisories.audit")),
            "{\n    \"name\": \"vendor/pkg\", \"config\": {\n        \"vendor-dir\": \"vendor\", \"sort-packages\": true,\n        \"policy\": {\n            \"advisories\": {\n                \"block\": true\n            }\n        }\n    }\n}\n"
        );
        let before = "{\n    \"name\": \"vendor/pkg\", \"config\": {\n        \"vendor-dir\": \"vendor\", \"sort-packages\": true,\n        \"policy\": {\n            \"advisories\": {\n                \"block\": true\n            }\n        }\n    }\n}\n";
        assert_eq!(
            edit_source(before, |s| s.remove_config_setting("policy.advisories.block")),
            "{\n    \"name\": \"vendor/pkg\", \"config\": {\n        \"vendor-dir\": \"vendor\",\n        \"sort-packages\": true,\n        \"policy\": {\n        }\n    }\n}\n"
        );
    }

    #[test]
    fn full_rewrite_fallback() {
        // `"extra": []` is not an object the manipulator can edit, so the file is rewritten
        // (with Composer's quirk of nesting the value below extra.extra)
        let out = edit_source("{\"extra\": [], \"require\": {}}", |s| s.add_property("extra.foo", &json!("bar")));
        assert_eq!(out, "{\n    \"extra\": {\n        \"extra\": {\n            \"foo\": \"bar\"\n        }\n    },\n    \"require\": {}\n}\n");
    }
}
