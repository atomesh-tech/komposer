//! `init` (`InitCommand`): creates a composer.json from options, or by asking.

use crate::cli::{Cli, InitArgs};
use crate::commands::config::{STABILITIES, schema_error};
use crate::composer::composer_file;
use anyhow::{Result, anyhow, bail};
use clap::Parser;
use komposer_package::config::Config;
use komposer_package::io::Io;
use komposer_package::json::{self, Flags};
use komposer_semver::Stability;
use regex::Regex;
use serde_json::{Map, Value, json};
use std::cell::LazyCell;
use std::path::Path;
use std::sync::{Arc, LazyLock};

static PACKAGE_NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-z0-9]([_.-]?[a-z0-9]+)*/[a-z0-9](([_.]|-{1,2})?[a-z0-9]+)*$").unwrap());

pub fn init(io: &Arc<Io>, args: &InitArgs, run_scripts: bool) -> Result<i32> {
    // the options, completed by the answers
    let mut a = args.clone();
    let git = LazyCell::new(git_config);
    if io.is_interactive() {
        interact(io, &mut a, &git)?;
    } else {
        a.name = a.name.or_else(|| Some(default_package_name(&git)));
        a.author = a.author.or_else(|| default_author(&git));
    }
    execute(io, a, run_scripts)
}

/// `InitCommand::execute()`.
fn execute(io: &Arc<Io>, a: InitArgs, run_scripts: bool) -> Result<i32> {
    let mut options = Map::new();
    let list = |v: &[String]| (!v.is_empty()).then(|| json!(v));
    for (key, value) in [
        ("name", a.name.clone().map(Value::String)),
        ("description", a.description.map(Value::String)),
        ("author", a.author.map(Value::String)),
        ("type", a.package_type.map(Value::String)),
        ("homepage", a.homepage.map(Value::String)),
        ("require", list(&a.require)),
        ("require-dev", list(&a.require_dev)),
        ("stability", a.stability.map(Value::String)),
        ("license", a.license.map(Value::String)),
        ("autoload", a.autoload.clone().map(Value::String)),
    ] {
        if let Some(v) = value {
            options.insert(key.into(), v);
        }
    }

    if let Some(name) = &a.name
        && !PACKAGE_NAME.is_match(name)
    {
        bail!(invalid_name(name));
    }
    if let Some(Value::String(author)) = options.shift_remove("author") {
        let (name, email) = parse_author(&author)?;
        let mut author = Map::from_iter([("name".to_string(), Value::String(name))]);
        if let Some(email) = email {
            author.insert("email".into(), email.into());
        }
        options.insert("authors".into(), json!([author]));
    }
    if !a.repository.is_empty() {
        let repos = a.repository.iter().map(|r| komposer_repository::factory::config_from_string(r)).collect::<Result<Vec<_>>>()?;
        options.insert("repositories".into(), Value::Array(repos));
    }
    if let Some(stability) = options.shift_remove("stability") {
        options.insert("minimum-stability".into(), stability);
    }
    let require = format_requirements(&a.require)?;
    match options.get_mut("require") {
        Some(slot) => *slot = require,
        None => _ = options.insert("require".into(), require),
    }
    if let Some(slot) = options.get_mut("require-dev") {
        *slot = format_requirements(&a.require_dev)?;
    }
    let namespace = namespace_from_package_name(a.name.as_deref().unwrap_or_default());
    if let (Some(path), Some(slot)) = (&a.autoload, options.get_mut("autoload")) {
        *slot = json!({"psr-4": {format!("{}\\", namespace.as_deref().unwrap_or_default()): path}});
    }

    let file = composer_file()?;
    if io.is_interactive() {
        io.write_error(&format!("\n{}\n", json::encode(&Value::Object(options.clone()), Flags::FILE)));
        if !io.ask_confirmation("Do you confirm generation [<comment>yes</comment>]? ", true) {
            io.write_error("<error>Command aborted</error>");
            return Ok(1);
        }
    } else {
        io.write_error(&format!("Writing {}", file.display()));
    }
    json::write(&file, &Value::Object(options.clone()))?;
    if let Some(errors) = schema_errors(&options) {
        io.write_error("<error>Schema validation error, aborting</error>");
        io.write_error(&format!("\"{}\" does not match the expected JSON schema:\n - {}", file.display(), errors.join("\n - ")));
        let _ = std::fs::remove_file(&file);
        return Ok(1);
    }

    let has_dependencies = [&a.require, &a.require_dev].iter().any(|r| !r.is_empty());
    if let Some(path) = &a.autoload {
        std::fs::create_dir_all(path)?;
        if !has_dependencies && run_command(io, &["dump-autoload"], run_scripts).is_err() {
            io.write_error("Could not run dump-autoload.");
        }
    }
    if io.is_interactive() && Path::new(".git").is_dir() {
        let ignore = std::fs::canonicalize(".gitignore").unwrap_or_else(|_| std::env::current_dir().unwrap_or_default().join(".gitignore"));
        if !has_vendor_ignore(&ignore)
            && io.ask_confirmation(
                "Would you like the <info>vendor</info> directory added to your <info>.gitignore</info> [<comment>yes</comment>]? ",
                true,
            )
        {
            add_vendor_ignore(&ignore)?;
        }
    }
    if io.is_interactive()
        && has_dependencies
        && io.ask_confirmation("Would you like to install dependencies now [<comment>yes</comment>]? ", true)
        && run_command(io, &["update"], run_scripts).is_err()
    {
        io.write_error("Could not update dependencies. Run `composer update` to see more information.");
    }
    if let Some(path) = &a.autoload {
        io.write_error(&format!("PSR-4 autoloading configured. Use \"<comment>namespace {};</comment>\" in {path}", namespace.unwrap_or_default()));
        io.write_error("Include the Composer autoloader with: <comment>require 'vendor/autoload.php';</comment>");
    }
    Ok(0)
}

fn invalid_name(name: &str) -> String {
    format!(
        "The package name {name} is invalid, it should be lowercase and have a vendor name, a forward slash, and a package name, matching: [a-z0-9_.-]+/[a-z0-9_.-]+"
    )
}

/// Runs another command in-process (`$this->getApplication()->find(...)->run()`).
fn run_command(io: &Arc<Io>, command: &[&str], run_scripts: bool) -> Result<i32> {
    let mut argv = vec!["komposer"];
    argv.extend(command);
    if !run_scripts {
        argv.push("--no-scripts");
    }
    crate::run(Cli::try_parse_from(argv)?, io)
}

/// `BaseCommand::formatRequirements()`: `{name: constraint}`, or `{}` when empty.
fn format_requirements(requirements: &[String]) -> Result<Value> {
    let mut out = Map::new();
    for (name, version) in komposer_semver::parse_name_version_pairs(requirements, komposer_package::is_platform_package) {
        let Some(version) = version else { bail!("Option {name} is missing a version constraint, use e.g. {name}:^1.0") };
        out.insert(name, version.into());
    }
    Ok(Value::Object(out))
}

/// The part of `JsonFile::validateSchema(LAX_SCHEMA)` that options can violate.
fn schema_errors(options: &Map<String, Value>) -> Option<Vec<String>> {
    let errors: Vec<String> =
        ["type", "homepage", "minimum-stability"].into_iter().filter_map(|k| options.get(k).and_then(|v| schema_error(k, v))).collect();
    (!errors.is_empty()).then_some(errors)
}

/// `InitCommand::interact()`.
fn interact(io: &Arc<Io>, a: &mut InitArgs, git: &Map<String, Value>) -> Result<()> {
    let block = "  Welcome to the Composer config generator  ";
    let pad = " ".repeat(block.len());
    io.write_error(&format!("\n{pad}\n{block}\n{pad}\n"));
    io.write_error("\nThis command will guide you through creating your composer.json config.\n");

    let name = a.name.clone().unwrap_or_else(|| default_package_name(git));
    a.name = ask_and_validate(io, &format!("Package name (<vendor>/<name>) [<comment>{name}</comment>]: "), Some(&name), |v| match v {
        Some(v) if !PACKAGE_NAME.is_match(&v) => Err(invalid_name(&v)),
        v => Ok(v.or(Some(name.clone()))),
    })?;

    let description = a.description.clone().filter(|d| !d.is_empty());
    a.description = io.ask(&format!("Description [<comment>{}</comment>]: ", description.as_deref().unwrap_or_default()), description.as_deref());

    let author = a.author.clone().or_else(|| default_author(git));
    let question = format!("Author [{}n to skip]: ", author.as_ref().map(|a| format!("<comment>{a}</comment>, ")).unwrap_or_default());
    a.author = ask_and_validate(io, &question, author.as_deref(), |v| {
        if matches!(v.as_deref(), Some("n" | "no")) {
            return Ok(None);
        }
        let (name, email) = parse_author(&v.or(author.clone()).unwrap_or_default()).map_err(|e| e.to_string())?;
        Ok(Some(email.map_or(name.clone(), |e| format!("{name} <{e}>"))))
    })?;

    let stability = a.stability.clone().filter(|s| !s.is_empty());
    a.stability = ask_and_validate(
        io,
        &format!("Minimum Stability [<comment>{}</comment>]: ", stability.as_deref().unwrap_or_default()),
        stability.as_deref(),
        |v| match v {
            Some(v) if !STABILITIES.contains(&v.as_str()) => {
                Err(format!("Invalid minimum stability \"{v}\". Must be empty or one of: {}", STABILITIES.join(", ")))
            }
            v => Ok(v),
        },
    )?;

    let package_type = a.package_type.clone();
    a.package_type = io
        .ask(
            &format!(
                "Package Type (e.g. library, project, metapackage, composer-plugin) [<comment>{}</comment>]: ",
                package_type.as_deref().unwrap_or_default()
            ),
            package_type.as_deref(),
        )
        .filter(|t| !t.is_empty());

    let license = a.license.clone().or_else(|| std::env::var("COMPOSER_DEFAULT_LICENSE").ok().filter(|l| !l.is_empty()));
    // ponytail: not checked against the SPDX license list
    a.license = io.ask(&format!("License [<comment>{}</comment>]: ", license.as_deref().unwrap_or_default()), license.as_deref());

    io.write_error("\nDefine your dependencies.\n");
    let preferred = a.stability.as_deref().and_then(Stability::parse).unwrap_or(Stability::Stable);
    let mut finder = VersionFinder { io, repositories: &a.repository, minimum: preferred, repos: None };
    if !a.require.is_empty()
        || io.ask_confirmation("Would you like to define your dependencies (require) interactively [<comment>yes</comment>]? ", true)
    {
        a.require = determine_requirements(io, &a.require, &mut finder)?;
    }
    if !a.require_dev.is_empty()
        || io.ask_confirmation("Would you like to define your dev dependencies (require-dev) interactively [<comment>yes</comment>]? ", true)
    {
        a.require_dev = determine_requirements(io, &a.require_dev, &mut finder)?;
    }

    let autoload = a.autoload.clone().filter(|p| !p.is_empty()).unwrap_or_else(|| "src/".into());
    let namespace = namespace_from_package_name(a.name.as_deref().unwrap_or_default()).unwrap_or_default();
    let question = format!(
        "Add PSR-4 autoload mapping? Maps namespace \"{namespace}\" to the entered relative path. [<comment>{autoload}</comment>, n to skip]: "
    );
    a.autoload = ask_and_validate(io, &question, Some(&autoload), |v| {
        static DIR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[^/][A-Za-z0-9\-_/]+/$").unwrap());
        match v.as_deref() {
            Some("n" | "no") => Ok(None),
            Some(v) if !DIR.is_match(v) => {
                Err(format!("The src folder name \"{v}\" is invalid. Please add a relative path with tailing forward slash. [A-Za-z0-9_-/]+/"))
            }
            _ => Ok(v.or(Some(autoload.clone()))),
        }
    })?;
    Ok(())
}

/// `askAndValidate()`: the validator gets the answer (or the default) until it accepts one.
fn ask_and_validate(
    io: &Io,
    question: &str,
    default: Option<&str>,
    validate: impl Fn(Option<String>) -> Result<Option<String>, String>,
) -> Result<Option<String>> {
    // ponytail: Composer asks forever and aborts on end of input; this gives up after 10 tries
    for _ in 0..10 {
        match validate(io.ask(question, default)) {
            Ok(v) => return Ok(v),
            Err(e) => io.write_error(&format!("<error>{e}</error>")),
        }
    }
    bail!("Aborted.")
}

/// `determineRequirements()`: the given requirements with missing versions resolved, or the
/// packages entered one by one (a simplified flow: no search result listing).
fn determine_requirements(io: &Io, given: &[String], finder: &mut VersionFinder) -> Result<Vec<String>> {
    let mut result = vec![];
    if !given.is_empty() {
        for (name, version) in komposer_semver::parse_name_version_pairs(given, komposer_package::is_platform_package) {
            let version = match version {
                Some(v) => {
                    static TOO_STRICT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d+(\.\d+)?$").unwrap());
                    if TOO_STRICT.is_match(&v) {
                        io.warning(&format!(
                            "The \"{v}\" constraint for \"{name}\" appears too strict and will likely not match what you want. See https://getcomposer.org/constraints"
                        ));
                    }
                    v
                }
                None => {
                    let v = finder.best_version(&name)?;
                    io.write_error(&format!("Using version <info>{v}</info> for <info>{name}</info>"));
                    v
                }
            };
            result.push(format!("{name} {version}"));
        }
        return Ok(result);
    }
    while let Some(package) = io.ask("Search for a package: ", None) {
        let (name, version) = komposer_semver::parse_name_version_pairs(&[package], komposer_package::is_platform_package).remove(0);
        let version = match version {
            Some(v) => v,
            None => match io.ask("Enter the version constraint to require (or leave blank to use the latest version): ", None) {
                Some(v) => v,
                None => {
                    let v = finder.best_version(&name)?;
                    io.write_error(&format!("Using version <info>{v}</info> for <info>{name}</info>"));
                    v
                }
            },
        };
        result.push(format!("{name} {version}"));
    }
    Ok(result)
}

/// `findBestVersionAndNameForPackage()` against the `--repository` repositories (and
/// packagist.org unless disabled) or the configured ones.
struct VersionFinder<'a> {
    io: &'a Arc<Io>,
    repositories: &'a [String],
    minimum: Stability,
    repos: Option<Vec<Arc<dyn komposer_package::repository::Repository>>>,
}

impl VersionFinder<'_> {
    fn best_version(&mut self, name: &str) -> Result<String> {
        use komposer_solver::{PlatformRequirementFilter, RepositorySet, VersionSelector};
        let config = Config::create(&std::env::current_dir()?)?;
        if self.repos.is_none() {
            let http = komposer_repository::http::HttpClient::new(self.io.clone(), &config)?;
            let defs = if self.repositories.is_empty() {
                config.repositories()
            } else {
                let mut defs: Vec<Value> =
                    self.repositories.iter().map(|r| komposer_repository::factory::config_from_string(r)).collect::<Result<_>>()?;
                let disabled = [json!({"packagist": false}), json!({"packagist.org": false})];
                if !defs.iter().any(|d| disabled.contains(d)) {
                    defs.push(json!({"type": "composer", "url": "https://repo.packagist.org"}));
                }
                defs.retain(|d| !disabled.contains(d));
                defs
            };
            self.repos = Some(komposer_repository::factory::create_repositories(&http, &defs)?);
        }
        let mut set = RepositorySet::new(self.minimum);
        for repo in self.repos.iter().flatten() {
            set.add_repository(repo.clone());
        }
        let platform = komposer_repository::platform::PlatformRepository::from_config(&config)?;
        let platform: Option<&dyn komposer_package::repository::Repository> = komposer_repository::platform::php_detected().then_some(&platform);
        let selector = VersionSelector::new(&set, platform)?;
        let best = selector.find_best_candidate(name, None, self.minimum, &PlatformRequirementFilter::IgnoreNothing, 0, Some(self.io), &|_| true)?;
        best.map(|p| selector.find_recommended_require_version(&p)).ok_or_else(|| {
            anyhow!(
                "Could not find a matching version of package {name}. Check the package spelling, your version constraint and that the package is available in a stability which matches your minimum-stability ({}).",
                self.minimum
            )
        })
    }
}

/// `parseAuthorString()`: (name, email).
fn parse_author(author: &str) -> Result<(String, Option<String>)> {
    static AUTHOR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"^(?P<name>[- .,\p{L}\p{N}\p{Mn}'’"()]+)(?:\s+<(?P<email>.+?)>)?$"#).unwrap());
    let Some(c) = AUTHOR.captures(author) else {
        bail!("Invalid author string.  Must be in the formats: Jane Doe or John Smith <john@example.com>");
    };
    let email = c.name("email").map(|m| m.as_str().to_string());
    if let Some(email) = email.as_deref().filter(|e| !is_valid_email(e)) {
        bail!("Invalid email \"{email}\"");
    }
    Ok((c["name"].trim().to_string(), email))
}

/// ponytail: an approximation of `FILTER_VALIDATE_EMAIL` (a dotted domain is required too)
fn is_valid_email(email: &str) -> bool {
    static EMAIL: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^[A-Za-z0-9.!#$%&'*+/=?^_`{|}~-]+@(?:[A-Za-z0-9](?:[A-Za-z0-9-]*[A-Za-z0-9])?\.)+[A-Za-z0-9](?:[A-Za-z0-9-]*[A-Za-z0-9])?$")
            .unwrap()
    });
    EMAIL.is_match(email) && !email.starts_with('.') && !email.contains("..")
}

/// `namespaceFromPackageName()`: `new_projects.acme-extra/package-name` is `NewProjectsAcmeExtra\PackageName`.
fn namespace_from_package_name(name: &str) -> Option<String> {
    if !name.contains('/') {
        return None;
    }
    let part = |p: &str| p.split(|c: char| !c.is_ascii_alphanumeric()).map(ucfirst).collect::<String>();
    Some(name.split('/').map(part).collect::<Vec<_>>().join("\\"))
}

fn ucfirst(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_ascii_uppercase().to_string() + c.as_str()).unwrap_or_default()
}

/// `getGitConfig()`: `git config -l` as a map.
fn git_config() -> Map<String, Value> {
    let Ok(out) = std::process::Command::new("git").args(["config", "-l"]).output() else { return Map::new() };
    if !out.status.success() {
        return Map::new();
    }
    String::from_utf8_lossy(&out.stdout).lines().filter_map(|l| l.split_once('=')).map(|(k, v)| (k.to_string(), v.into())).collect()
}

fn non_empty_env(var: &str) -> Option<String> {
    std::env::var(var).ok().filter(|v| !v.is_empty())
}

/// `sanitizePackageNameComponent()`.
fn sanitize_package_name_component(name: &str) -> String {
    static CAMEL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?:([a-z])([A-Z])|([A-Z])([A-Z][a-z]))").unwrap());
    static EDGES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[_.-]+|[_.-]+$|[^a-z0-9_.-]").unwrap());
    static RUNS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"([_.-]){2,}").unwrap());
    let name = CAMEL.replace_all(name, "${1}${3}-${2}${4}").to_lowercase();
    let name = EDGES.replace_all(&name, "");
    RUNS.replace_all(&name, "$1").into_owned()
}

/// `getDefaultPackageName()`: `<vendor>/<directory name>`.
fn default_package_name(git: &Map<String, Value>) -> String {
    let cwd = std::fs::canonicalize(".").unwrap_or_default();
    let name = sanitize_package_name_component(&cwd.file_name().unwrap_or_default().to_string_lossy());
    let vendor = non_empty_env("COMPOSER_DEFAULT_VENDOR")
        .or_else(|| git.get("github.user").and_then(Value::as_str).map(String::from))
        .or_else(|| non_empty_env("USERNAME"))
        .or_else(|| non_empty_env("USER"))
        .unwrap_or_else(|| name.clone());
    format!("{}/{name}", sanitize_package_name_component(&vendor))
}

/// `getDefaultAuthor()`: `Name <email>` when both are known.
fn default_author(git: &Map<String, Value>) -> Option<String> {
    let from_git = |k: &str| git.get(k).and_then(Value::as_str).map(String::from);
    let name = non_empty_env("COMPOSER_DEFAULT_AUTHOR").or_else(|| from_git("user.name"))?;
    let email = non_empty_env("COMPOSER_DEFAULT_EMAIL").or_else(|| from_git("user.email"))?;
    Some(format!("{name} <{email}>"))
}

/// `hasVendorIgnore()`.
fn has_vendor_ignore(file: &Path) -> bool {
    static VENDOR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^/?vendor(/\*?)?$").unwrap());
    std::fs::read_to_string(file).is_ok_and(|c| c.lines().any(|l| VENDOR.is_match(l)))
}

/// `addVendorIgnore()`.
fn add_vendor_ignore(file: &Path) -> Result<()> {
    let mut contents = std::fs::read_to_string(file).unwrap_or_default();
    // Composer quirk: checks for a leading newline, so this always appends one to existing files
    if file.exists() && !contents.starts_with('\n') {
        contents.push('\n');
    }
    std::fs::write(file, contents + "/vendor/\n")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_author_strings() {
        for (name, email, input) in [
            ("John Smith", Some("john@example.com"), "John Smith <john@example.com>"),
            ("John Smith", None, "John Smith"),
            ("Matti Meikäläinen", Some("matti@example.com"), "Matti Meikäläinen <matti@example.com>"),
            ("Matti Meika\u{308}la\u{308}inen", Some("matti@example.com"), "Matti Meika\u{308}la\u{308}inen <matti@example.com>"),
            ("h4x0r", Some("h4x@example.com"), "h4x0r <h4x@example.com>"),
            ("Johnathon \"Johnny\" Smith", Some("john@example.com"), "Johnathon \"Johnny\" Smith <john@example.com>"),
            ("Johnathon (Johnny) Smith", Some("john@example.com"), "Johnathon (Johnny) Smith <john@example.com>"),
        ] {
            assert_eq!(parse_author(input).unwrap(), (name.to_string(), email.map(String::from)), "{input}");
        }
        assert!(parse_author("").is_err());
        assert!(parse_author("John Smith <john>").is_err());
    }

    #[test]
    fn namespace_from_names() {
        assert_eq!(namespace_from_package_name("new_projects.acme-extra/package-name").as_deref(), Some("NewProjectsAcmeExtra\\PackageName"));
        assert_eq!(namespace_from_package_name("invalid-package-name"), None);
        assert_eq!(namespace_from_package_name(""), None);
    }

    #[test]
    fn sanitizes_name_components() {
        assert_eq!(sanitize_package_name_component("_foo_--bar__baz.--..qux__"), "foo-bar_baz.qux");
        assert_eq!(sanitize_package_name_component(".vendorName"), "vendor-name");
        assert_eq!(sanitize_package_name_component("MyHTTPApp"), "my-http-app");
    }

    #[test]
    fn formats_requirements() {
        let reqs = ["first/pkg:1.0.0".to_string(), "second/pkg ^3.4".into(), "third/pkg=2.0".into()];
        assert_eq!(format_requirements(&reqs).unwrap(), json!({"first/pkg": "1.0.0", "second/pkg": "^3.4", "third/pkg": "2.0"}));
        assert_eq!(format_requirements(&[]).unwrap(), json!({}));
        let err = format_requirements(&["first".into()]).unwrap_err().to_string();
        assert_eq!(err, "Option first is missing a version constraint, use e.g. first:^1.0");
    }

    #[test]
    fn vendor_ignore() {
        let dir = std::env::temp_dir().join(format!("komposer-init-ignore-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("ignore");
        assert!(!has_vendor_ignore(&file));
        add_vendor_ignore(&file).unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "/vendor/\n");
        assert!(has_vendor_ignore(&file));
        std::fs::write(&file, "node_modules").unwrap();
        add_vendor_ignore(&file).unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "node_modules\n/vendor/\n");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn validates_schema_bits() {
        let options = |k: &str, v: &str| Map::from_iter([(k.to_string(), Value::String(v.into()))]);
        assert!(schema_errors(&options("minimum-stability", "bogus")).is_some());
        assert!(schema_errors(&options("minimum-stability", "RC")).is_none());
        assert!(schema_errors(&options("homepage", "not-a-url")).is_some());
        assert!(schema_errors(&options("homepage", "https://example.org/")).is_none());
        assert!(schema_errors(&options("type", "")).is_some());
    }
}
