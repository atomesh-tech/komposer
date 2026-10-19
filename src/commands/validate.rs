//! `validate`: `ValidateCommand`, `Util\ConfigValidator`, `Package\Loader\ValidatingArrayLoader`
//! and the JSON schema checks of `JsonFile::validateJsonSchema()`.

use crate::cli::ValidateArgs;
use crate::composer::{Composer, composer_file};
use anyhow::{Context, Result, bail};
use komposer_package::io::Io;
use komposer_package::is_platform_package;
use komposer_package::json::{self, Flags};
use komposer_package::loader::{package_naming_error, php_empty};
use komposer_package::repository::Repository;
use komposer_semver::{Constraint, Op, compact_constraint};
use regex::Regex;
use serde_json::{Map, Value, json};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, LazyLock};

const SCHEMA: &str = include_str!("../../res/composer-schema.json");
const LOCK_SCHEMA: &str = include_str!("../../res/composer-lock-schema.json");

/// `ValidatingArrayLoader::CHECK_UNBOUND_CONSTRAINTS`.
const CHECK_UNBOUND: u8 = 1;
/// `ValidatingArrayLoader::CHECK_STRICT_CONSTRAINTS`.
const CHECK_STRICT: u8 = 2;

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).unwrap()
}

/// `ValidateCommand::execute()`.
pub fn validate(io: &Arc<Io>, args: &ValidateArgs, run_scripts: bool) -> Result<i32> {
    let _ = run_scripts;
    let file = args.file.clone().unwrap_or(composer_file()?);
    let name = file.display().to_string();
    if !file.exists() {
        io.write_error(&format!("<error>{name} not found.</error>"));
        return Ok(3);
    }
    let Ok(contents) = std::fs::read_to_string(&file) else {
        io.write_error(&format!("<error>{name} is not readable.</error>"));
        return Ok(3);
    };
    let flags = if args.no_check_all { 0 } else { CHECK_UNBOUND | CHECK_STRICT };
    let mut result = validate_file(&file, flags, !args.no_check_version);

    let c = load_composer(io, &file, contents)?;
    let check_lock = (!args.no_check_lock && c.config.get_bool("lock")) || args.check_lock;
    let mut lock_errors = vec![];
    if c.locker.is_locked()? {
        if !c.locker.is_fresh()? {
            lock_errors.push("- The lock file is not up to date with the latest changes in composer.json, it is recommended that you run `composer update` or `composer update <package name>`.".to_string());
        }
        lock_errors.extend(c.locker.missing_requirement_info(&c.root, true)?);
    }
    let check_publish = !args.no_check_publish;
    let exit = |(errors, warnings): (bool, bool)| {
        if errors {
            2
        } else if args.strict && warnings {
            1
        } else {
            0
        }
    };
    let mut code = exit(output_result(io, &name, &mut result, check_publish, Some((check_lock, lock_errors))));

    if args.with_dependencies {
        for package in c.installed()?.packages()? {
            let Some(path) = c.install_path(&package) else { continue };
            let file = path.join("composer.json");
            if path.is_dir() && file.exists() {
                let mut result = validate_file(&file, flags, !args.no_check_version);
                code = code.max(exit(output_result(io, &package.pretty_name, &mut result, check_publish, None)));
            }
        }
    }
    Ok(code)
}

/// `createComposerInstance($input, $io, $file)`: a project loads only when its composer.json
/// parses and matches the lax schema.
fn load_composer(io: &Arc<Io>, file: &Path, contents: String) -> Result<Composer> {
    let raw: Value = match serde_json::from_str(contents.strip_prefix('\u{feff}').unwrap_or(&contents)) {
        Ok(v) => v,
        Err(e) => bail!("\"{}\" does not contain valid JSON\n{e}", file.display()),
    };
    let errors = schema_errors(&raw, false);
    if !errors.is_empty() {
        bail!("\"{}\" does not match the expected JSON schema:\n - {}", file.display(), errors.join("\n - "));
    }
    if file == composer_file()? {
        return Composer::load(io.clone());
    }
    let dir = file.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let dir = dir.canonicalize()?;
    let local = json::parse_object(&contents).with_context(|| format!("\"{}\" does not contain valid JSON", file.display()))?;
    Composer::from_parts(io.clone(), dir, file.to_path_buf(), contents, local)
}

/// What `ConfigValidator::validate()` found.
#[derive(Debug, Default)]
pub struct Validation {
    pub errors: Vec<String>,
    pub publish_errors: Vec<String>,
    pub warnings: Vec<String>,
}

/// `ValidateCommand::outputResult()`. Returns whether there were errors (including publish and
/// lock errors that count as such) and general warnings. `lock` is `(check_lock, lock_errors)`.
fn output_result(io: &Io, name: &str, r: &mut Validation, check_publish: bool, lock: Option<(bool, Vec<String>)>) -> (bool, bool) {
    let (check_lock, lock_errors) = lock.clone().unwrap_or_default();
    let mut schema_url = false;
    if !r.errors.is_empty() {
        io.write_error(&format!("<error>{name} is invalid, the following errors/warnings were found:</error>"));
    } else if !r.publish_errors.is_empty() && check_publish {
        io.write_error(&format!("<info>{name} is valid for simple usage with Composer but has</info>"));
        io.write_error("<info>strict errors that make it unable to be published as a package</info>");
        schema_url = lock.is_some();
    } else if !r.warnings.is_empty() {
        io.write_error(&format!("<info>{name} is valid, but with a few warnings</info>"));
        schema_url = lock.is_some();
    } else if !lock_errors.is_empty() {
        io.write(&format!("<info>{name} is valid but your composer.lock has some {}</info>", if check_lock { "errors" } else { "warnings" }));
    } else {
        io.write(&format!("<info>{name} is valid</info>"));
    }
    if schema_url {
        io.write_error("<warning>See https://getcomposer.org/doc/04-schema.md for details on the schema</warning>");
    }

    let section = |title: &str, list: &[String]| -> Vec<String> {
        if list.is_empty() {
            return vec![];
        }
        std::iter::once(title.to_string()).chain(list.iter().map(|m| format!("- {m}"))).collect()
    };
    let mut errors = section("# General errors", &r.errors);
    let warnings = section("# General warnings", &r.warnings);
    if check_publish {
        errors.extend(section("# Publish errors", &r.publish_errors));
    }
    let mut extra_warnings = vec![];
    if !lock_errors.is_empty() {
        let (title, target) = if check_lock { ("# Lock file errors", &mut errors) } else { ("# Lock file warnings", &mut extra_warnings) };
        target.push(title.to_string());
        target.extend(lock_errors);
    }
    for (style, messages) in [("error", &errors), ("warning", &warnings), ("warning", &extra_warnings)] {
        for msg in messages {
            io.write_error(&if msg.starts_with('#') { format!("<{style}>{msg}</{style}>") } else { msg.clone() });
        }
    }
    (!errors.is_empty(), !warnings.is_empty())
}

/// `ConfigValidator::validate($file, $arrayLoaderValidationFlags, $flags)`.
pub fn validate_file(file: &Path, loader_flags: u8, check_version: bool) -> Validation {
    let mut r = Validation::default();
    let read = std::fs::read_to_string(file).map_err(|e| format!("Could not read {}\n\n{e}", file.display()));
    let contents = match read {
        Ok(c) => c,
        Err(e) => {
            r.errors.push(e);
            return r;
        }
    };
    let text = contents.strip_prefix('\u{feff}').unwrap_or(&contents);
    let raw: Value = match serde_json::from_str(text) {
        Ok(raw) => raw,
        Err(e) => {
            r.errors.push(format!("\"{}\" does not contain valid JSON\n{e}", file.display()));
            return r;
        }
    };
    let manifest = json::php_normalize(raw.clone());
    let lax = schema_errors(&raw, false);
    if lax.is_empty() {
        r.publish_errors = schema_errors(&raw, true);
    } else {
        r.errors = lax;
    }
    let Value::Object(mut manifest) = manifest else { return r };
    if let Some((key, line)) = duplicate_key(text) {
        r.warnings.push(format!("Key {key} is a duplicate in {} at line {line}", file.display()));
    }
    manifest_warnings(&manifest, check_version, &mut r);

    manifest.entry("version").or_insert_with(|| "1.0.0".into());
    manifest.entry("name").or_insert_with(|| "dummy/dummy".into());
    let mut loader = ValidatingLoader { cfg: manifest, flags: loader_flags, errors: vec![], warnings: vec![] };
    loader.load();
    r.errors.extend(loader.errors);
    r.warnings.extend(loader.warnings);
    r
}

/// The data checks of `ConfigValidator::validate()`, before `ValidatingArrayLoader` runs.
fn manifest_warnings(m: &Map<String, Value>, check_version: bool, r: &mut Validation) {
    let w = &mut r.warnings;
    match m.get("license").filter(|l| !php_empty(l)) {
        None => w.push("No license specified, it is recommended to do so. For closed-source software you may use \"proprietary\" as license.".into()),
        Some(license) => {
            for license in php_array_values(license).iter().filter_map(|l| l.as_str()).filter(|l| *l != "proprietary") {
                if spdx::license(license) != Some(true) {
                    continue;
                }
                if re(r"(?i)^[AL]?GPL-[123](\.[01])?\+$").is_match(license) {
                    w.push(format!(
                        "License \"{license}\" is a deprecated SPDX license identifier, use \"{}-or-later\" instead",
                        license.replace('+', "")
                    ));
                } else if re(r"(?i)^[AL]?GPL-[123](\.[01])?$").is_match(license) {
                    w.push(format!(
                        "License \"{license}\" is a deprecated SPDX license identifier, use \"{license}-only\" or \"{license}-or-later\" instead"
                    ));
                } else {
                    w.push(format!("License \"{license}\" is a deprecated SPDX license identifier, see https://spdx.org/licenses/"));
                }
            }
        }
    }
    if check_version && m.get("version").is_some_and(|v| !v.is_null()) {
        w.push("The version field is present, it is recommended to leave it out if the package is published on Packagist.".into());
    }
    if let Some(name) = m.get("name").and_then(Value::as_str).filter(|n| !n.is_empty() && *n != "0")
        && name.chars().any(|c| c.is_ascii_uppercase())
    {
        let suggest = re(r"([a-z])([A-Z])|([A-Z])([A-Z][a-z])").replace_all(name, "$1$3-$2$4").to_lowercase();
        r.publish_errors.push(format!(
            "Name \"{name}\" does not match the best practice (e.g. lower-cased/with-dashes). We suggest using \"{suggest}\" instead. As such you will not be able to submit it to Packagist."
        ));
    }
    if m.get("type").and_then(Value::as_str) == Some("composer-installer") {
        w.push("The package type 'composer-installer' is deprecated. Please distribute your custom installers as plugins from now on. See https://getcomposer.org/doc/articles/plugins.md for plugin documentation.".into());
    }
    let keys = |key: &str| -> Vec<String> { m.get(key).and_then(Value::as_object).map(|o| o.keys().cloned().collect()).unwrap_or_default() };
    let (require, require_dev) = (keys("require"), keys("require-dev"));
    if m.contains_key("require") && m.contains_key("require-dev") {
        let both: Vec<&String> = require.iter().filter(|k| require_dev.contains(k)).collect();
        if !both.is_empty() {
            let verb = if both.len() > 1 { "are" } else { "is" };
            w.push(format!(
                "{} {verb} required both in require and require-dev, this can lead to unexpected behavior",
                both.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
            ));
        }
    }
    for link_type in ["provide", "replace"] {
        for (require_type, required) in [("require", &require), ("require-dev", &require_dev)] {
            for provided in keys(link_type).iter().filter(|p| required.contains(p)) {
                w.push(format!(
                    "The package {provided} in {require_type} is also listed in {link_type} which satisfies the requirement. Remove it from {link_type} if you wish to install it."
                ));
            }
        }
    }
    let mut packages: Vec<(&String, &Value)> = vec![];
    for (name, version) in ["require", "require-dev"].iter().filter_map(|k| m.get(*k)?.as_object()).flatten() {
        match packages.iter_mut().find(|(n, _)| *n == name) {
            Some(entry) => entry.1 = version,
            None => packages.push((name, version)),
        }
    }
    for (package, _) in packages.iter().filter(|(_, v)| v.as_str().is_some_and(|v| v.contains('#'))) {
        w.push(format!("The package \"{package}\" is pointing to a commit-ref, this is bad practice and can cause unforeseen issues."));
    }
    let scripts = keys("scripts");
    for (key, what) in [("scripts-descriptions", "Description"), ("scripts-aliases", "Aliases")] {
        for script in keys(key).iter().filter(|s| !scripts.contains(s)) {
            w.push(format!("{what} for non-existent script \"{script}\" found in \"{key}\""));
        }
    }
    for psr in ["psr-0", "psr-4"] {
        if m.get("autoload").and_then(|a| a.get(psr)).and_then(Value::as_object).is_some_and(|p| p.contains_key("")) {
            w.push(format!("Defining autoload.{psr} with an empty namespace prefix is a bad idea for performance"));
        }
    }
}

/// `(array) $value` values.
fn php_array_values(v: &Value) -> Vec<Value> {
    match v {
        Value::Array(a) => a.clone(),
        Value::Object(o) => o.values().cloned().collect(),
        Value::Null => vec![],
        v => vec![v.clone()],
    }
}

/// `get_debug_type()` of a decoded JSON value.
fn debug_type(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "string",
        Value::Array(_) | Value::Object(_) => "array",
    }
}

/// PHP's string conversion of a scalar, for messages.
fn scalar(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Bool(true) => "1".into(),
        Value::Bool(false) | Value::Null => String::new(),
        v => v.to_string(),
    }
}

/// Entries of a PHP array (list indexes become keys).
fn entries(v: &Value) -> Vec<(String, Value)> {
    match v {
        Value::Array(a) => a.iter().enumerate().map(|(i, v)| (i.to_string(), v.clone())).collect(),
        Value::Object(o) => o.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        _ => vec![],
    }
}

fn is_array(v: &Value) -> bool {
    v.is_array() || v.is_object()
}

/// `unset($array[$key])` on a PHP array value.
fn unset(v: &mut Value, key: &str) {
    match v {
        Value::Object(o) => {
            o.shift_remove(key);
        }
        Value::Array(a) => {
            // ponytail: list entries are replaced by null rather than renumbered, only messages depend on them
            if let Some(slot) = key.parse::<usize>().ok().and_then(|i| a.get_mut(i)) {
                *slot = Value::Null;
            }
        }
        _ => {}
    }
}

/// `ValidatingArrayLoader::load()`: errors and warnings for a package's composer.json data.
struct ValidatingLoader {
    cfg: Map<String, Value>,
    flags: u8,
    errors: Vec<String>,
    warnings: Vec<String>,
}

impl ValidatingLoader {
    fn isset(&self, key: &str) -> bool {
        self.cfg.get(key).is_some_and(|v| !v.is_null())
    }

    fn str(&self, key: &str) -> Option<String> {
        self.cfg.get(key).and_then(Value::as_str).map(String::from)
    }

    fn load(&mut self) {
        let original_name = self.str("name");
        self.validate_string("name", true);
        if let Some(err) = original_name.as_deref().and_then(|n| package_naming_error(n, false)) {
            self.errors.push(format!("name : {err}"));
        }
        if let Some(version) = self.cfg.get("version").filter(|v| !v.is_null()).cloned() {
            if is_array(&version) {
                self.validate_string("version", false);
            } else if let Err(e) = komposer_semver::normalize(&scalar(&version)) {
                self.errors.push(format!("version : invalid value ({}): {e}", scalar(&version)));
                self.cfg.remove("version");
            }
        }
        if let Some(platform) = self.cfg.get("config").and_then(|c| c.get("platform")).filter(|p| !p.is_null()) {
            let mut errors = vec![];
            for (key, value) in entries(platform).into_iter().chain((!is_array(platform)).then(|| ("0".into(), platform.clone()))) {
                match &value {
                    Value::Bool(false) => {}
                    Value::String(v) => {
                        if let Err(e) = komposer_semver::normalize(v) {
                            errors.push(format!("config.platform.{key} : invalid value ({v}): {e}"));
                        }
                    }
                    v => {
                        errors.push(format!("config.platform.{key} : invalid value ({} {}): expected string or false", debug_type(v), var_export(v)))
                    }
                }
            }
            self.errors.extend(errors);
        }

        self.validate_regex("type", "[A-Za-z0-9-]+");
        self.validate_string("target-dir", false);
        self.validate_array("extra", false);
        self.validate_bin();
        self.validate_array("scripts", false);
        self.validate_string("description", false);
        self.validate_url("homepage");
        self.validate_flat_array("keywords", Some(r"[\p{N}\p{L} ._-]+"));

        let mut release_date = None;
        self.validate_string("time", false);
        if let Some(time) = self.str("time") {
            match komposer_package::loader::format_date(&time).and_then(|d| chrono::DateTime::parse_from_rfc3339(&d).ok()) {
                Some(d) => release_date = Some(d.timestamp()),
                None => {
                    // ponytail: PHP's DateTime parser message is not reproduced
                    self.errors.push(format!("time : invalid value ({time}): Failed to parse time string ({time})"));
                    self.cfg.remove("time");
                }
            }
        }
        self.validate_license(release_date);
        self.validate_authors();
        self.validate_support();
        self.validate_funding();
        self.validate_php_ext();
        self.validate_links();

        if self.validate_array("suggest", false) {
            let invalid: Vec<String> = entries(&self.cfg["suggest"]).into_iter().filter(|(_, v)| !v.is_string()).map(|(k, _)| k).collect();
            for package in invalid {
                self.errors.push(format!("suggest.{package} : invalid value, must be a string describing why the package is suggested"));
                unset(self.cfg.get_mut("suggest").unwrap(), &package);
            }
        }
        if self.validate_string("minimum-stability", false) {
            let stability = self.str("minimum-stability").unwrap_or_default();
            if !["stable", "beta", "alpha", "dev"].contains(&stability.to_lowercase().as_str()) && stability != "RC" {
                self.errors.push(format!("minimum-stability : invalid value ({stability}), must be one of stable, RC, beta, alpha, dev"));
                self.cfg.remove("minimum-stability");
            }
        }
        if self.validate_array("autoload", false) {
            const TYPES: [&str; 5] = ["psr-0", "psr-4", "classmap", "files", "exclude-from-classmap"];
            for (kind, config) in entries(&self.cfg["autoload"]) {
                if !TYPES.contains(&kind.as_str()) {
                    self.errors.push(format!("autoload : invalid value ({kind}), must be one of {}", TYPES.join(", ")));
                    unset(self.cfg.get_mut("autoload").unwrap(), &kind);
                }
                if kind == "psr-4" {
                    for (namespace, _) in entries(&config).into_iter().filter(|(ns, _)| !ns.is_empty() && !ns.ends_with('\\')) {
                        self.errors.push(format!(
                            "autoload.psr-4 : invalid value ({namespace}), namespaces must end with a namespace separator, should be {namespace}\\\\"
                        ));
                    }
                }
            }
        }
        if self.cfg.get("autoload").and_then(|a| a.get("psr-4")).is_some_and(|p| !p.is_null()) && self.isset("target-dir") {
            self.errors
                .push("target-dir : this can not be used together with the autoload.psr-4 setting, remove target-dir to upgrade to psr-4".into());
            if let Some(autoload) = self.cfg.get_mut("autoload") {
                unset(autoload, "psr-4");
            }
        }
        for kind in ["source", "dist"] {
            if self.validate_array(kind, false) {
                let errors = source_errors(kind, &self.cfg[kind]);
                self.errors.extend(errors);
            }
        }
        self.validate_flat_array("include-path", None);
        self.validate_array("transport-options", false);
        self.validate_branch_aliases();
    }

    fn validate_bin(&mut self) {
        if !self.isset("bin") {
            return;
        }
        if self.cfg["bin"].is_string() {
            self.validate_string("bin", false);
        } else {
            self.validate_flat_array("bin", None);
        }
        let escapes = |b: &str| re(r"(?:^|[\\/])\.\.(?:[\\/]|$)").is_match(b);
        match self.cfg.get("bin").cloned() {
            Some(Value::String(bin)) if escapes(&bin) => {
                self.errors.push(format!("bin : invalid value ({bin}), must not contain a \"..\" path component"));
                self.cfg.remove("bin");
            }
            Some(bins) if is_array(&bins) => {
                for (key, bin) in entries(&bins) {
                    if let Some(bin) = bin.as_str().filter(|b| escapes(b)) {
                        self.errors.push(format!("bin.{key} : invalid value ({bin}), must not contain a \"..\" path component"));
                        unset(self.cfg.get_mut("bin").unwrap(), &key);
                    }
                }
            }
            _ => {}
        }
    }

    fn validate_license(&mut self, release_date: Option<i64>) {
        let Some(license) = self.cfg.get("license").filter(|v| !v.is_null()).cloned() else { return };
        if !is_array(&license) && !license.is_string() {
            self.warnings.push(format!("License must be a string or array of strings, got {}.", json::encode(&license, Flags::PLAIN)));
            self.cfg.remove("license");
            return;
        }
        let mut licenses = vec![];
        for license in php_array_values(&license) {
            match license {
                Value::String(s) => licenses.push(s),
                other => self.warnings.push(format!("License {} should be a string.", json::encode(&other, Flags::PLAIN))),
            }
        }
        // licenses are only checked on recently released versions
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
        if release_date.is_none_or(|t| t >= now - 8 * 86400) {
            for license in licenses.iter().filter(|l| *l != "proprietary") {
                let candidate = license.replace("proprietary", "MIT");
                if spdx::validate(&candidate) {
                    continue;
                }
                let encoded = json::encode(&Value::String(license.clone()), Flags::PLAIN);
                self.warnings.push(if spdx::validate(candidate.trim()) {
                    format!("License {encoded} must not contain extra spaces, make sure to trim it.")
                } else {
                    format!("License {encoded} is not a valid SPDX license identifier, see https://spdx.org/licenses/ if you use an open license.\nIf the software is closed-source, you may use \"proprietary\" as license.")
                });
            }
        }
        self.cfg.insert("license".into(), json!(licenses));
    }

    fn validate_authors(&mut self) {
        if !self.validate_array("authors", false) {
            return;
        }
        let mut authors = self.cfg["authors"].clone();
        for (key, author) in entries(&authors.clone()) {
            if !is_array(&author) {
                self.errors.push(format!("authors.{key} : should be an array, {} given", debug_type(&author)));
                unset(&mut authors, &key);
                continue;
            }
            let mut kept = author.clone();
            for field in ["homepage", "email", "name", "role"] {
                if author.get(field).is_some_and(|v| !v.is_null() && !v.is_string()) {
                    self.errors.push(format!("authors.{key}.{field} : invalid value, must be a string"));
                    unset(&mut kept, field);
                }
            }
            if let Some(homepage) = author.get("homepage").and_then(Value::as_str).filter(|h| !filter_url(h, &["http", "https"])) {
                self.warnings.push(format!("authors.{key}.homepage : invalid value ({homepage}), must be an http/https URL"));
                unset(&mut kept, "homepage");
            }
            if let Some(email) = author.get("email").and_then(Value::as_str).filter(|e| !is_email(e)) {
                self.warnings.push(format!("authors.{key}.email : invalid value ({email}), must be a valid email address"));
                unset(&mut kept, "email");
            }
            match &mut authors {
                Value::Object(o) => {
                    o.insert(key, kept);
                }
                Value::Array(a) => a[key.parse::<usize>().unwrap()] = kept,
                _ => {}
            }
        }
        self.cfg.insert("authors".into(), authors);
    }

    fn validate_support(&mut self) {
        if !self.validate_array("support", false) {
            return;
        }
        let mut support = self.cfg["support"].clone();
        for key in ["issues", "forum", "wiki", "source", "email", "irc", "docs", "rss", "chat", "security"] {
            if support.get(key).is_some_and(|v| !v.is_null() && !v.is_string()) {
                self.errors.push(format!("support.{key} : invalid value, must be a string"));
                unset(&mut support, key);
            }
        }
        let get = |s: &Value, key: &str| s.get(key).and_then(Value::as_str).map(String::from);
        if let Some(email) = get(&support, "email").filter(|e| !is_email(e)) {
            self.warnings.push(format!("support.email : invalid value ({email}), must be a valid email address"));
            unset(&mut support, "email");
        }
        if let Some(irc) = get(&support, "irc").filter(|i| !filter_url(i, &["irc", "ircs"])) {
            self.warnings.push(format!("support.irc : invalid value ({irc}), must be a irc://<server>/<channel> or ircs:// URL"));
            unset(&mut support, "irc");
        }
        for key in ["issues", "forum", "wiki", "source", "docs", "chat", "security"] {
            if let Some(url) = get(&support, key).filter(|u| !filter_url(u, &["http", "https"])) {
                self.warnings.push(format!("support.{key} : invalid value ({url}), must be an http/https URL"));
                unset(&mut support, key);
            }
        }
        self.cfg.insert("support".into(), support);
    }

    fn validate_funding(&mut self) {
        if !self.validate_array("funding", false) {
            return;
        }
        for (key, option) in entries(&self.cfg["funding"].clone()) {
            if !is_array(&option) {
                self.errors.push(format!("funding.{key} : should be an array, {} given", debug_type(&option)));
                continue;
            }
            for field in ["type", "url"] {
                if option.get(field).is_some_and(|v| !v.is_null() && !v.is_string()) {
                    self.errors.push(format!("funding.{key}.{field} : invalid value, must be a string"));
                }
            }
            if let Some(url) = option.get("url").and_then(Value::as_str).filter(|u| !filter_url(u, &["http", "https"])) {
                self.warnings.push(format!("funding.{key}.url : invalid value ({url}), must be an http/https URL"));
            }
        }
    }

    fn validate_php_ext(&mut self) {
        if !self.isset("php-ext") || !self.validate_array("php-ext", false) {
            return;
        }
        let e = &mut self.errors;
        if !matches!(self.cfg.get("type").and_then(Value::as_str), Some("php-ext" | "php-ext-zend")) {
            e.push("php-ext can only be set by packages of type \"php-ext\" or \"php-ext-zend\" which must be C extensions".into());
        }
        let ext = &self.cfg["php-ext"];
        let present = |k: &str| ext.get(k).filter(|v| !v.is_null());
        let given = |v: &Value| debug_type(v);
        if let Some(v) = present("extension-name").filter(|v| !v.is_string()) {
            e.push(format!("php-ext.extension-name : should be a string, {} given", given(v)));
        }
        if let Some(v) = present("priority").filter(|v| !v.is_i64() && !v.is_u64()) {
            e.push(format!("php-ext.priority : should be an integer, {} given", given(v)));
        }
        for key in ["support-zts", "support-nts"] {
            if let Some(v) = present(key).filter(|v| !v.is_boolean()) {
                e.push(format!("php-ext.{key} : should be a boolean, {} given", given(v)));
            }
        }
        if let Some(v) = present("build-path").filter(|v| !v.is_string()) {
            e.push(format!("php-ext.build-path : should be a string or null, {} given", given(v)));
        }
        if let Some(methods) = present("download-url-method") {
            const VALID: [&str; 3] = ["composer-default", "pre-packaged-source", "pre-packaged-binary"];
            if !is_array(methods) && !methods.is_string() {
                e.push(format!("php-ext.download-url-method : should be an array or a string, {} given", given(methods)));
            } else if is_array(methods) && entries(methods).is_empty() {
                e.push("php-ext.download-url-method : must contain at least one element".into());
            } else {
                let list = if methods.is_string() { vec![("0".to_string(), methods.clone())] } else { entries(methods) };
                for (key, method) in list {
                    match method.as_str() {
                        None => e.push(format!("php-ext.download-url-method.{key} : should be a string, {} given", given(&method))),
                        Some(m) if !VALID.contains(&m) => {
                            e.push(format!("php-ext.download-url-method.{key} : invalid value ({m}), must be one of {}", VALID.join(", ")))
                        }
                        Some(_) => {}
                    }
                }
            }
        }
        if present("os-families").is_some() && present("os-families-exclude").is_some() {
            e.push("php-ext : os-families and os-families-exclude cannot both be specified".into());
        } else {
            const FAMILIES: [&str; 6] = ["windows", "bsd", "darwin", "solaris", "linux", "unknown"];
            for field in ["os-families", "os-families-exclude"] {
                let Some(list) = present(field) else { continue };
                if !is_array(list) {
                    e.push(format!("php-ext.{field} : should be an array, {} given", given(list)));
                } else if entries(list).is_empty() {
                    e.push(format!("php-ext.{field} : must contain at least one element"));
                } else {
                    for (key, family) in entries(list) {
                        match family.as_str() {
                            None => e.push(format!("php-ext.{field}.{key} : should be a string, {} given", given(&family))),
                            Some(f) if !FAMILIES.contains(&f) => {
                                e.push(format!("php-ext.{field}.{key} : invalid value ({f}), must be one of {}", FAMILIES.join(", ")))
                            }
                            Some(_) => {}
                        }
                    }
                }
            }
        }
        if let Some(options) = present("configure-options") {
            if !is_array(options) {
                e.push(format!("php-ext.configure-options : should be an array, {} given", given(options)));
            }
            for (key, option) in entries(options) {
                if !is_array(&option) {
                    e.push(format!("php-ext.configure-options.{key} : should be an array, {} given", given(&option)));
                    continue;
                }
                match option.get("name").filter(|v| !v.is_null()) {
                    None => {
                        e.push(format!("php-ext.configure-options.{key}.name : must be present"));
                        continue;
                    }
                    Some(n) if !n.is_string() => {
                        e.push(format!("php-ext.configure-options.{key}.name : should be a string, {} given", given(n)));
                        continue;
                    }
                    Some(_) => {}
                }
                if let Some(v) = option.get("needs-value").filter(|v| !v.is_null() && !v.is_boolean()) {
                    e.push(format!("php-ext.configure-options.{key}.needs-value : should be a boolean, {} given", given(v)));
                }
                if let Some(v) = option.get("description").filter(|v| !v.is_null() && !v.is_string()) {
                    e.push(format!("php-ext.configure-options.{key}.description : should be a string, {} given", given(v)));
                }
            }
        }
    }

    fn validate_links(&mut self) {
        let unbound = Constraint::single(Op::Eq, "10000000-dev");
        let at_least_one = Constraint::single(Op::Ge, "1.0.0.0-dev");
        for link_type in ["require", "conflict", "provide", "replace", "require-dev"] {
            if !self.validate_array(link_type, false) {
                continue;
            }
            for (package, constraint) in entries(&self.cfg[link_type].clone()) {
                if self.str("name").is_some_and(|n| n.eq_ignore_ascii_case(&package)) {
                    self.errors.push(format!("{link_type}.{package} : a package cannot set a {link_type} on itself"));
                    unset(self.cfg.get_mut(link_type).unwrap(), &package);
                    continue;
                }
                if let Some(err) = package_naming_error(&package, true) {
                    self.warnings.push(format!("{link_type}.{err}"));
                } else if !re(r"^[A-Za-z0-9_./-]+$").is_match(&package) {
                    self.errors.push(format!("{link_type}.{package} : invalid key, package names must be strings containing only [A-Za-z0-9_./-]"));
                }
                match constraint.as_str() {
                    None => {
                        self.errors.push(format!("{link_type}.{package} : invalid value, must be a string containing a version constraint"));
                        unset(self.cfg.get_mut(link_type).unwrap(), &package);
                    }
                    Some("self.version") => {}
                    Some(pretty) => {
                        let parsed = match komposer_semver::parse_constraints(pretty) {
                            Ok(c) => c,
                            Err(e) => {
                                self.errors.push(format!("{link_type}.{package} : invalid version constraint ({e})"));
                                unset(self.cfg.get_mut(link_type).unwrap(), &package);
                                continue;
                            }
                        };
                        if self.flags & CHECK_UNBOUND != 0 && link_type == "require" && parsed.matches(&unbound) && !is_platform_package(&package) {
                            self.warnings.push(format!("{link_type}.{package} : unbound version constraints ({pretty}) should be avoided"));
                        } else if self.flags & CHECK_STRICT != 0
                            && link_type == "require"
                            && matches!(parsed, Constraint::Single { op: Op::Eq, .. })
                            && at_least_one.matches(&parsed)
                        {
                            self.warnings.push(format!(
                                "{link_type}.{package} : exact version constraints ({pretty}) should be avoided if the package follows semantic versioning"
                            ));
                        }
                        if compact_constraint(&parsed) == Constraint::None {
                            self.warnings.push(format!("{link_type}.{package} : this version constraint cannot possibly match anything ({pretty})"));
                        }
                    }
                }
                // any replaced package that is also a conflict flags every conflict (Composer quirk)
                let key_set =
                    |k: &str| self.cfg.get(k).and_then(Value::as_object).map(|o| o.keys().cloned().collect::<HashSet<_>>()).unwrap_or_default();
                if link_type == "conflict" && self.isset("replace") && !key_set("replace").is_disjoint(&key_set("conflict")) {
                    self.errors.push(format!(
                        "{link_type}.{package} : you cannot conflict with a package that is also replaced, as replace already creates an implicit conflict rule"
                    ));
                    unset(self.cfg.get_mut(link_type).unwrap(), &package);
                }
            }
        }
    }

    fn validate_branch_aliases(&mut self) {
        let Some(aliases) = self.cfg.get("extra").and_then(|e| e.get("branch-alias")).filter(|v| !v.is_null()).cloned() else { return };
        if !is_array(&aliases) {
            self.errors.push("extra.branch-alias : must be an array of versions => aliases".into());
            return;
        }
        for (source, target) in entries(&aliases) {
            let w = &mut self.warnings;
            let Some(target) = target.as_str() else {
                w.push(format!(
                    "extra.branch-alias.{source} : the target branch ({}) must be a string, \"{}\" received.",
                    json::encode(&target, Flags::PLAIN),
                    debug_type(&target)
                ));
                continue;
            };
            let Some(base) = target.strip_suffix("-dev") else {
                w.push(format!("extra.branch-alias.{source} : the target branch ({target}) must end in -dev"));
                continue;
            };
            if !komposer_semver::normalize_branch(base).ends_with("-dev") {
                w.push(format!("extra.branch-alias.{source} : the target branch ({target}) must be a parseable number like 2.0-dev"));
                continue;
            }
            if let (Some(src), Some(dst)) =
                (komposer_semver::parse_numeric_alias_prefix(&source), komposer_semver::parse_numeric_alias_prefix(target))
                && !dst.to_lowercase().starts_with(&src.to_lowercase())
            {
                w.push(format!("extra.branch-alias.{source} : the target branch ({target}) is not a valid numeric alias for this version"));
            }
        }
    }

    /// `validateString()`.
    fn validate_string(&mut self, prop: &str, mandatory: bool) -> bool {
        match self.cfg.get(prop) {
            Some(v) if !v.is_null() && !v.is_string() => {
                self.errors.push(format!("{prop} : should be a string, {} given", debug_type(v)));
                self.cfg.remove(prop);
                false
            }
            Some(Value::String(s)) if !s.trim_matches([' ', '\t', '\n', '\r', '\0', '\x0B']).is_empty() => true,
            _ => {
                if mandatory {
                    self.errors.push(format!("{prop} : must be present"));
                }
                self.cfg.remove(prop);
                false
            }
        }
    }

    /// `validateRegex()` (never mandatory here, so mismatches are warnings).
    fn validate_regex(&mut self, prop: &str, regex: &str) {
        if !self.validate_string(prop, false) {
            return;
        }
        let value = self.str(prop).unwrap_or_default();
        if !re(&format!("^(?:{regex})$")).is_match(&value) {
            self.warnings.push(format!("{prop} : invalid value ({value}), must match {regex}"));
            self.cfg.remove(prop);
        }
    }

    /// `validateArray()`.
    fn validate_array(&mut self, prop: &str, mandatory: bool) -> bool {
        match self.cfg.get(prop) {
            Some(v) if !v.is_null() && !is_array(v) => {
                self.errors.push(format!("{prop} : should be an array, {} given", debug_type(v)));
                self.cfg.remove(prop);
                false
            }
            Some(v) if !entries(v).is_empty() => true,
            _ => {
                if mandatory {
                    self.errors.push(format!("{prop} : must be present and contain at least one element"));
                }
                self.cfg.remove(prop);
                false
            }
        }
    }

    /// `validateFlatArray()`.
    fn validate_flat_array(&mut self, prop: &str, regex: Option<&str>) {
        if !self.validate_array(prop, false) {
            return;
        }
        for (key, value) in entries(&self.cfg[prop]) {
            if !value.is_string() && !value.is_number() {
                self.errors.push(format!("{prop}.{key} : must be a string or int, {} given", debug_type(&value)));
                unset(self.cfg.get_mut(prop).unwrap(), &key);
                continue;
            }
            if let Some(regex) = regex
                && !re(&format!("^(?:{regex})$")).is_match(&scalar(&value))
            {
                self.warnings.push(format!("{prop}.{key} : invalid value ({}), must match {regex}", scalar(&value)));
                unset(self.cfg.get_mut(prop).unwrap(), &key);
            }
        }
    }

    /// `validateUrl()`.
    fn validate_url(&mut self, prop: &str) {
        if !self.validate_string(prop, false) {
            return;
        }
        let value = self.str(prop).unwrap_or_default();
        if !filter_url(&value, &["http", "https"]) {
            self.warnings.push(format!("{prop} : invalid value ({value}), must be an http/https URL"));
            self.cfg.remove(prop);
        }
    }
}

/// The `source`/`dist` checks of `ValidatingArrayLoader::load()`.
fn source_errors(kind: &str, src: &Value) -> Vec<String> {
    let mut e = vec![];
    let present = |k: &str| src.get(k).filter(|v| !v.is_null());
    for field in ["type", "url"] {
        if present(field).is_none() {
            e.push(format!("{kind}.{field} : must be present"));
        }
    }
    if kind == "source" && present("reference").is_none() {
        e.push(format!("{kind}.reference : must be present"));
    }
    for field in ["type", "url"] {
        if let Some(v) = present(field).filter(|v| !v.is_string()) {
            e.push(format!("{kind}.{field} : should be a string, {} given", debug_type(v)));
        }
    }
    if let Some(v) = present("reference").filter(|v| !v.is_string() && !v.is_i64() && !v.is_u64()) {
        e.push(format!("{kind}.reference : should be a string or int, {} given", debug_type(v)));
    }
    for field in ["reference", "url"] {
        if let Some(v) = present(field).map(scalar).filter(|v| v.trim_start().starts_with('-')) {
            e.push(format!("{kind}.{field} : must not start with a \"-\", \"{v}\" given"));
        }
    }
    if kind == "source"
        && present("type").and_then(Value::as_str) == Some("perforce")
        && let Some(url) = present("url").and_then(Value::as_str).filter(|u| !is_perforce_port(u))
    {
        e.push(format!("{kind}.url : invalid Perforce port (\"{url}\"), it must be of the form [tcp|ssl:][host:]port"));
    }
    e
}

/// `Perforce::isValidPort()`.
fn is_perforce_port(url: &str) -> bool {
    !re(r"(?i)^\s*(?:rsh|jsh)\s*:").is_match(url)
        && re(r"(?i)^(?:(?:tcp|ssl)(?:4|6|46|64)?:)?(?:\[[0-9a-f:.]+\]|[a-z0-9._][a-z0-9._-]*)(?::[a-z0-9._][a-z0-9._-]*)?$").is_match(url)
}

/// `ValidatingArrayLoader::filterUrl()`: a URL with one of `schemes` and a host.
fn filter_url(value: &str, schemes: &[&str]) -> bool {
    if value.is_empty() {
        return true;
    }
    let Some(c) = re(r"^([A-Za-z][A-Za-z0-9+.-]*)://([^/?#]*)").captures(value) else { return false };
    let host = c[2].rsplit('@').next().unwrap_or("");
    let host = if host.starts_with('[') { host } else { host.split(':').next().unwrap_or("") };
    !host.is_empty() && schemes.contains(&&c[1])
}

/// `filter_var($email, FILTER_VALIDATE_EMAIL)`.
// ponytail: an approximation of PHP's email regex (dot-atom local part, dotted domain with a letter TLD)
fn is_email(email: &str) -> bool {
    static RE: LazyLock<Regex> = LazyLock::new(|| {
        let atom = r"[A-Za-z0-9!#$%&'*+/=?^_`{|}~-]+";
        let label = r"[A-Za-z0-9]+(?:-+[A-Za-z0-9]+)*";
        Regex::new(&format!(r"^{atom}(?:\.{atom})*@(?:{label}\.)+[A-Za-z][A-Za-z0-9]*(?:-+[A-Za-z0-9]+)*$")).unwrap()
    });
    email.len() <= 320 && email.split('@').next().is_some_and(|l| l.len() <= 64) && RE.is_match(email)
}

/// `var_export($v, true)` for scalars.
fn var_export(v: &Value) -> String {
    match v {
        Value::Null => "NULL".into(),
        Value::Bool(b) => b.to_string(),
        Value::String(s) => format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'")),
        Value::Number(n) => n.to_string(),
        _ => "array (\n)".into(),
    }
}

/// The first `JsonParser::DETECT_KEY_CONFLICTS` duplicate: the key and the line its value ends on.
fn duplicate_key(text: &str) -> Option<(String, usize)> {
    // one frame per open object (its keys) or array (None)
    let mut stack: Vec<Option<HashSet<String>>> = vec![];
    let mut expecting_key = false;
    let mut pending: Option<(String, usize)> = None;
    let (mut line, mut last_line) = (1, 1);
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\n' => {
                line += 1;
                continue;
            }
            c if c.is_whitespace() => continue,
            ',' | '}' | ']' if pending.as_ref().is_some_and(|(_, depth)| *depth == stack.len()) => {
                return pending.map(|(key, _)| (key, last_line));
            }
            '{' => {
                stack.push(Some(HashSet::new()));
                expecting_key = true;
            }
            '[' => stack.push(None),
            '}' | ']' => {
                stack.pop();
            }
            ',' => expecting_key = matches!(stack.last(), Some(Some(_))),
            '"' => {
                let mut s = String::new();
                while let Some(c) = chars.next() {
                    match c {
                        '\\' => s.extend(chars.next()),
                        '"' => break,
                        c => s.push(c),
                    }
                }
                if expecting_key && let Some(Some(keys)) = stack.last_mut() {
                    if !keys.insert(s.clone()) && pending.is_none() {
                        pending = Some((s, stack.len()));
                    }
                    expecting_key = false;
                }
            }
            _ => {}
        }
        last_line = line;
    }
    None
}

static COMPOSER_SCHEMA: LazyLock<Value> = LazyLock::new(|| serde_json::from_str(SCHEMA).unwrap());

/// `JsonFile::validateJsonSchema()` against composer-schema.json (`strict` = `STRICT_SCHEMA`).
pub fn schema_errors(data: &Value, strict: bool) -> Vec<String> {
    static STRICT: LazyLock<Value> = LazyLock::new(|| {
        let mut schema = COMPOSER_SCHEMA.clone();
        schema["additionalProperties"] = false.into();
        schema["required"] = json!(["name", "description"]);
        schema
    });
    crate::json_schema::validate(data, if strict { &STRICT } else { &COMPOSER_SCHEMA }, &COMPOSER_SCHEMA)
}

/// `JsonFile::validateJsonSchema()` against composer-lock-schema.json.
pub fn lock_schema_errors(data: &Value) -> Vec<String> {
    static LOCK: LazyLock<Value> = LazyLock::new(|| serde_json::from_str(LOCK_SCHEMA).unwrap());
    crate::json_schema::validate(data, &LOCK, &COMPOSER_SCHEMA)
}

/// `Composer\Spdx\SpdxLicenses` with the license list of composer/spdx-licenses 1.6.0.
mod spdx {
    use super::*;

    const LICENSES: &str = include_str!("../../res/spdx-licenses.json");
    const EXCEPTIONS: &str = include_str!("../../res/spdx-exceptions.json");

    /// lowercased identifier => deprecated
    fn licenses() -> &'static HashMap<String, bool> {
        static MAP: LazyLock<HashMap<String, bool>> = LazyLock::new(|| {
            let data: Map<String, Value> = serde_json::from_str(LICENSES).unwrap();
            data.into_iter().map(|(id, v)| (id.to_lowercase(), v[2].as_bool().unwrap_or(false))).collect()
        });
        &MAP
    }

    fn exceptions() -> &'static HashSet<String> {
        static SET: LazyLock<HashSet<String>> = LazyLock::new(|| {
            let data: Map<String, Value> = serde_json::from_str(EXCEPTIONS).unwrap();
            data.keys().map(|id| id.to_lowercase()).collect()
        });
        &SET
    }

    /// `getLicenseByIdentifier()`: whether the identifier is deprecated, if known.
    pub fn license(id: &str) -> Option<bool> {
        licenses().get(&id.to_lowercase()).copied()
    }

    /// `validate()`: an SPDX license expression (`MIT`, `(MIT or GPL-3.0-only)`, `LicenseRef-x`...).
    pub fn validate(expression: &str) -> bool {
        if licenses().contains_key(&expression.to_lowercase()) {
            return true;
        }
        // tokens with whether whitespace preceded them
        let mut tokens: Vec<(String, bool)> = vec![];
        let mut space = false;
        for c in expression.chars() {
            match c {
                c if c.is_whitespace() => space = true,
                '(' | ')' => {
                    tokens.push((c.to_string(), space));
                    space = false;
                }
                c => match tokens.last_mut() {
                    Some((t, _)) if !space && t != "(" && t != ")" => t.push(c),
                    _ => {
                        tokens.push((c.to_string(), space));
                        space = false;
                    }
                },
            }
        }
        if expression.starts_with(char::is_whitespace) || expression.ends_with(char::is_whitespace) {
            return false;
        }
        if let [(word, _)] = tokens.as_slice()
            && ["none", "noassertion"].contains(&word.to_lowercase().as_str())
        {
            return true;
        }
        let mut pos = 0;
        compound(&tokens, &mut pos) && pos == tokens.len()
    }

    fn keyword(tokens: &[(String, bool)], pos: usize, words: &[&str]) -> bool {
        tokens.get(pos).is_some_and(|(t, space)| *space && words.contains(&t.to_lowercase().as_str()))
            && tokens.get(pos + 1).is_some_and(|(_, space)| *space)
    }

    fn compound(tokens: &[(String, bool)], pos: &mut usize) -> bool {
        if !head(tokens, pos) {
            return false;
        }
        if keyword(tokens, *pos, &["and", "or"]) {
            *pos += 1;
            return compound(tokens, pos);
        }
        true
    }

    fn head(tokens: &[(String, bool)], pos: &mut usize) -> bool {
        let Some((token, _)) = tokens.get(*pos) else { return false };
        *pos += 1;
        if token == "(" {
            return compound(tokens, pos) && tokens.get(*pos).is_some_and(|(t, _)| t == ")") && {
                *pos += 1;
                true
            };
        }
        if !simple(token) {
            return false;
        }
        if keyword(tokens, *pos, &["with"]) {
            *pos += 2;
            return exceptions().contains(&tokens[*pos - 1].0.to_lowercase());
        }
        true
    }

    fn simple(token: &str) -> bool {
        static REF: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^(?:DocumentRef-[\pL\pN.-]+:)?LicenseRef-[\pL\pN.-]+$").unwrap());
        let id = token.strip_suffix('+').unwrap_or(token).to_lowercase();
        licenses().contains_key(&id) || REF.is_match(token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loader(data: Value, flags: u8) -> (Vec<String>, Vec<String>) {
        let mut l = ValidatingLoader { cfg: data.as_object().unwrap().clone(), flags, errors: vec![], warnings: vec![] };
        l.load();
        (l.errors, l.warnings)
    }

    /// Documents with the errors Composer 2.10.3's own `JsonFile::validateSchema()` reports for them.
    #[test]
    fn schema_errors_match_composer() {
        let cases: Map<String, Value> = serde_json::from_str(include_str!("../../tests/fixtures/schema-errors.json")).unwrap();
        let mut failures = vec![];
        for (name, case) in &cases {
            let doc = &case["doc"];
            let actual: Vec<(&str, Vec<String>)> = if name.starts_with("lock-") {
                vec![("lock", lock_schema_errors(doc))]
            } else {
                vec![("lax", schema_errors(doc, false)), ("strict", schema_errors(doc, true))]
            };
            for (mode, errors) in actual {
                let expected: Vec<String> = serde_json::from_value(case[mode].clone()).unwrap();
                if errors != expected {
                    failures.push(format!("{name} ({mode})\n  expected {expected:#?}\n  actual {errors:#?}"));
                }
            }
        }
        assert!(failures.is_empty(), "{} mismatches:\n{}", failures.len(), failures.join("\n"));
    }

    #[test]
    fn spdx_expressions() {
        for valid in [
            "MIT",
            "mit",
            "GPL-2.0+",
            "(MIT OR GPL-3.0-only)",
            "LGPL-2.1-only AND MIT",
            "GPL-2.0-only WITH Classpath-exception-2.0",
            "LicenseRef-foo",
            "NONE",
            "(MIT)",
        ] {
            assert!(spdx::validate(valid), "{valid}");
        }
        for invalid in ["MITT", "MIT AND", "(MIT", "MIT WITH Nope", " MIT", "MIT OR(GPL-3.0-only)", ""] {
            assert!(!spdx::validate(invalid), "{invalid}");
        }
        assert_eq!(spdx::license("GPL-2.0+"), Some(true));
        assert_eq!(spdx::license("MIT"), Some(false));
    }

    #[test]
    fn duplicate_keys_report_the_line_of_the_value() {
        assert_eq!(duplicate_key("{\n\"a\": {\n},\n\"a\": {\n}\n}"), Some(("a".into(), 5)));
        assert_eq!(duplicate_key("{\"x\": {\"a\": 1,\n \"a\": 2\n, \"b\": 3}}"), Some(("a".into(), 2)));
        assert_eq!(duplicate_key("{\"a\": [{\"a\": 1}, {\"a\": 2}]}"), None);
    }

    #[test]
    fn schema_messages_match_justinrainbow() {
        let data = json!({"name": "Acme/Foo", "require": {"a/b": 1}, "homepage": "nope", "minimum-stability": "x"});
        let mut errors = schema_errors(&data, false);
        errors.sort();
        assert_eq!(
            errors,
            vec![
                "homepage : Invalid URL format",
                "minimum-stability : Does not have a value in the enumeration [\"dev\",\"alpha\",\"beta\",\"rc\",\"RC\",\"stable\"]",
                "name : Does not match the regex pattern ^[a-z0-9]([_.-]?[a-z0-9]+)*/[a-z0-9](([_.]|-{1,2})?[a-z0-9]+)*$",
                "require.a/b : Integer value found, but a string is required",
            ]
        );
        let strict = schema_errors(&json!({"$schema": "x", "foo": 1}), true);
        assert_eq!(
            strict,
            vec![
                "name : The property name is required",
                "description : The property description is required",
                "The property foo is not defined and the definition does not allow additional properties",
            ]
        );
    }

    #[test]
    fn lock_schema_resolves_the_composer_schema() {
        let lock = include_str!("../../tests/fixtures/laravel-13/composer.lock");
        assert_eq!(lock_schema_errors(&serde_json::from_str(lock).unwrap()), Vec::<String>::new());
        // verified against Composer 2.10.3's diagnose
        assert_eq!(
            lock_schema_errors(&json!({"packages": [{"name": "x/y", "version": 1}], "packages-dev": [], "aliases": "x"})),
            vec![
                "content-hash : The property content-hash is required",
                "packages[0].version : Integer value found, but a string is required",
                "aliases : String value found, but an array is required",
            ]
        );
    }

    #[test]
    fn config_validator_checks() {
        // ConfigValidatorTest
        let dir = std::env::temp_dir().join(format!("komposer-validate-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("composer.json");
        std::fs::write(&file, r#"{"name": "a/b", "description": "x", "license": "GPL-2.0+", "require": {"c/d": "*", "e/f": "1.0.0", "g/h": "dev-main#abc"}, "require-dev": {"c/d": "^1"}, "scripts-aliases": {"x": ["y"]}}"#).unwrap();
        let r = validate_file(&file, CHECK_UNBOUND | CHECK_STRICT, true);
        assert_eq!(r.errors, Vec::<String>::new());
        assert_eq!(
            r.warnings,
            vec![
                "License \"GPL-2.0+\" is a deprecated SPDX license identifier, use \"GPL-2.0-or-later\" instead",
                "c/d is required both in require and require-dev, this can lead to unexpected behavior",
                "The package \"g/h\" is pointing to a commit-ref, this is bad practice and can cause unforeseen issues.",
                "Aliases for non-existent script \"x\" found in \"scripts-aliases\"",
                "require.c/d : unbound version constraints (*) should be avoided",
                "require.e/f : exact version constraints (1.0.0) should be avoided if the package follows semantic versioning",
            ]
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn validating_loader_errors() {
        // ValidatingArrayLoaderTest
        let (errors, warnings) = loader(
            json!({
                "name": "foo/bar", "version": "1.0.0", "type": "foo bar", "homepage": "foo:bar", "license": ["MIT", 1],
                "authors": [{"name": "x", "email": "nope", "homepage": "x://y"}],
                "support": {"source": "foo:bar", "irc": "http://x"},
                "require": {"foo/bar": "1", "Bar/Baz": "^1", "x/y": "foo"},
                "conflict": {"a/a": "*"}, "replace": {"a/a": "*"},
                "autoload": {"psr-4": {"Foo": "src/"}, "psr-1": []},
                "source": {"type": "git", "url": "--evil"},
                "minimum-stability": "rc",
                "extra": {"branch-alias": {"dev-master": "1.0"}},
            }),
            CHECK_UNBOUND | CHECK_STRICT,
        );
        assert_eq!(
            errors,
            vec![
                "require.foo/bar : a package cannot set a require on itself",
                "require.x/y : invalid version constraint (Could not parse version constraint foo: Invalid version string \"foo\")",
                "conflict.a/a : you cannot conflict with a package that is also replaced, as replace already creates an implicit conflict rule",
                "minimum-stability : invalid value (rc), must be one of stable, RC, beta, alpha, dev",
                "autoload.psr-4 : invalid value (Foo), namespaces must end with a namespace separator, should be Foo\\\\",
                "autoload : invalid value (psr-1), must be one of psr-0, psr-4, classmap, files, exclude-from-classmap",
                "source.reference : must be present",
                "source.url : must not start with a \"-\", \"--evil\" given",
            ]
        );
        assert_eq!(
            warnings,
            vec![
                "type : invalid value (foo bar), must match [A-Za-z0-9-]+",
                "homepage : invalid value (foo:bar), must be an http/https URL",
                "License 1 should be a string.",
                "authors.0.homepage : invalid value (x://y), must be an http/https URL",
                "authors.0.email : invalid value (nope), must be a valid email address",
                "support.irc : invalid value (http://x), must be a irc://<server>/<channel> or ircs:// URL",
                "support.source : invalid value (foo:bar), must be an http/https URL",
                "require.Bar/Baz is invalid, it should not contain uppercase characters. Please use bar/baz instead.",
                "extra.branch-alias.dev-master : the target branch (1.0) must end in -dev",
            ]
        );
    }

    #[test]
    fn emails_and_urls() {
        assert!(is_email("j.boggiano@seld.be") && !is_email("foo@localhost") && !is_email("a@@b.c"));
        assert!(filter_url("https://github.com/x", &["http", "https"]) && filter_url("", &["http"]) && !filter_url("github.com", &["http"]));
    }
}
