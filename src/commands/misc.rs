//! Small commands: `run-script`, script aliases, `exec`, `clear-cache`, `about`.

use crate::cli::{ClearCacheArgs, ExecArgs, RunScriptArgs};
use crate::composer::Composer;
use crate::scripts::EVENTS;
use anyhow::{Result, bail};
use komposer_package::config::Config;
use komposer_package::io::Io;
use komposer_repository::cache::Cache;
use serde_json::Value;
use std::sync::Arc;

/// `run-script`.
pub fn run_script(c: &Composer, args: &RunScriptArgs, run_scripts: bool) -> Result<i32> {
    if args.list {
        return list_scripts(c);
    }
    let Some(script) = &args.script else { bail!("Missing required argument \"script\"") };
    if EVENTS.contains(&script.as_str()) && !c.scripts(true).has(script) {
        bail!("Script \"{script}\" cannot be run with this command");
    }
    if !c.scripts(true).has(script) {
        bail!("Script \"{script}\" is not defined in this package");
    }
    if let Some(timeout) = &args.timeout {
        if timeout.is_empty() || !timeout.bytes().all(|b| b.is_ascii_digit()) {
            bail!("Timeout value must be numeric and positive if defined, or 0 for forever");
        }
        // overrides the timeout from the environment or config
        crate::scripts::TIMEOUT.store(timeout.parse().unwrap_or(u64::MAX), std::sync::atomic::Ordering::Relaxed);
    }
    set_dev_mode(args.dev || !args.no_dev);
    c.scripts(run_scripts).dispatch(script, &args.args)
}

/// `ScriptAliasCommand`: `komposer <script> [args]`.
pub fn script_alias(c: &Composer, argv: &[String], run_scripts: bool) -> Result<i32> {
    let (name, tokens) = argv.split_first().expect("clap passes the subcommand name");
    if !c.scripts(true).has(name) || EVENTS.contains(&name.as_str()) {
        bail!("Command \"{name}\" is not defined.");
    }
    let options = tokens.iter().take_while(|t| *t != "--");
    let dev = options.clone().any(|t| t == "--dev") || !options.clone().any(|t| t == "--no-dev");
    // the arguments: options (theirs, the global ones, unknown ones) are not
    let mut args = vec![];
    let mut rest = tokens.iter();
    while let Some(token) = rest.next() {
        match token.as_str() {
            "--" => {
                args.extend(rest.cloned());
                break;
            }
            "-d" | "--working-dir" => {
                rest.next();
            }
            t if t.starts_with('-') && t.len() > 1 => {}
            _ => args.push(token.clone()),
        }
    }
    // `$input->__toString()` without the leading script name
    let raw: Vec<String> = std::env::args().skip(1).map(|t| crate::scripts::alias_token(&t)).collect();
    let input = raw.join(" ");
    let input = regex::Regex::new(&format!(r"^\S*{} ?", regex::escape(name))).unwrap().replace(&input, "").into_owned();
    set_dev_mode(dev);
    c.scripts(run_scripts).dispatch_alias(name, &args, &input)
}

fn set_dev_mode(dev: bool) {
    // SAFETY: single-threaded before running scripts
    unsafe { std::env::set_var("COMPOSER_DEV_MODE", if dev { "1" } else { "0" }) };
}

fn list_scripts(c: &Composer) -> Result<i32> {
    let scripts = c.root.package.meta.get("scripts").and_then(Value::as_object).cloned().unwrap_or_default();
    if scripts.is_empty() {
        return Ok(0);
    }
    let descriptions = c.root.raw.get("scripts-descriptions").and_then(Value::as_object);
    c.io.write_error("<info>scripts:</info>");
    let rows: Vec<(String, String)> = scripts
        .keys()
        .map(|name| {
            let desc = match descriptions.and_then(|d| d.get(name)).and_then(Value::as_str) {
                Some(d) => d.to_string(),
                None if EVENTS.contains(&name.as_str()) => String::new(),
                None => format!("Runs the {name} script as defined in composer.json"),
            };
            (format!("  {name}"), desc)
        })
        .collect();
    let width = rows.iter().map(|(n, _)| n.len()).max().unwrap_or(0);
    for (name, desc) in rows {
        c.io.write(format!("{name:width$} {desc}").trim_end());
    }
    Ok(0)
}

/// `exec`.
pub fn exec(c: &Composer, args: &ExecArgs) -> Result<i32> {
    let bin_dir = c.config.get_path("bin-dir");
    let Some(binary) = args.binary.as_ref().filter(|_| !args.list) else {
        let mut bins: Vec<String> = std::fs::read_dir(&bin_dir)
            .map(|d| d.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect())
            .unwrap_or_default();
        bins.sort();
        // skip .bat copies
        let names = bins.clone();
        bins.retain(|b| b.strip_suffix(".bat").is_none_or(|base| !names.iter().any(|n| n == base)));
        bins.extend(c.root.package.bin.iter().map(|b| format!("{} (local)", b.rsplit('/').next().unwrap_or(b))));
        if bins.is_empty() {
            bail!("No binaries found in composer.json or in bin-dir ({})", bin_dir.display());
        }
        c.io.write("<comment>Available binaries:</comment>");
        for bin in bins {
            c.io.write(&format!("<info>- {bin}</info>"));
        }
        return Ok(0);
    };
    let mut scripts = c.scripts(true);
    scripts.scripts.insert("__exec_command".into(), Value::String(binary.clone()));
    scripts.dispatch("__exec_command", &args.args)
}

/// `clear-cache`.
pub fn clear_cache(io: &Arc<Io>, config: &Config, args: &ClearCacheArgs) -> Result<i32> {
    for key in ["cache-vcs-dir", "cache-repo-dir", "cache-files-dir", "cache-dir"] {
        // only individual dirs get garbage collected
        if key == "cache-dir" && args.gc {
            continue;
        }
        let Ok(path) = config.get_path(key).canonicalize() else {
            io.write_error(&format!("<info>Cache directory does not exist ({key}): </info>"));
            continue;
        };
        let cache = Cache::new(io.clone(), &path, "a-z0-9._").read_only(config.get_bool("cache-read-only"));
        if !cache.is_enabled() {
            io.write_error(&format!("<info>Cache is not enabled ({key}): {}</info>", path.display()));
            continue;
        }
        if args.gc {
            io.write_error(&format!("<info>Garbage-collecting cache ({key}): {}</info>", path.display()));
            match key {
                "cache-files-dir" => {
                    cache.gc(config.cache_ttl("cache-files-ttl"), config.cache_files_maxsize());
                }
                "cache-repo-dir" => {
                    cache.gc(config.cache_ttl("cache-ttl"), 1024 * 1024 * 1024);
                }
                _ => {
                    cache.gc_vcs_cache(config.cache_ttl("cache-ttl"));
                }
            }
        } else {
            io.write_error(&format!("<info>Clearing cache ({key}): {}</info>", path.display()));
            cache.clear();
        }
    }
    io.write_error(if args.gc { "<info>All caches garbage-collected.</info>" } else { "<info>All caches cleared.</info>" });
    Ok(0)
}

/// `about`.
pub fn about(io: &Io) -> Result<i32> {
    io.write(&format!(
        "<info>Komposer - Dependency Manager for PHP - version {} (Composer {} compatible)</info>\n<comment>Komposer is a Rust implementation of Composer, a dependency manager tracking local dependencies of your projects and libraries.\nSee https://getcomposer.org/ for more information.</comment>",
        env!("CARGO_PKG_VERSION"),
        komposer_repository::COMPOSER_VERSION
    ));
    Ok(0)
}

/// `status`.
pub fn status(io: &Arc<Io>, run_scripts: bool) -> Result<i32> {
    maintenance::status(io, run_scripts)
}

/// `diagnose`.
pub fn diagnose(io: &Arc<Io>) -> Result<i32> {
    maintenance::diagnose(io)
}

/// `status` and `diagnose`.
mod maintenance {
    use crate::commands::validate::{lock_schema_errors, validate_file};
    use crate::composer::{Composer, composer_file};
    use crate::installer::fetcher;
    use anyhow::{Result, bail};
    use komposer_installer::{InstallationManager, InstalledRepo};
    use komposer_package::config::Config;
    use komposer_package::io::{Io, Verbosity};
    use komposer_package::operation::Operation;
    use komposer_package::repository::Repository;
    use komposer_package::{Package, PackageRef, dumper};
    use komposer_repository::http::{HttpClient, RequestOptions, error_status};
    use komposer_repository::platform::{PlatformRepository, detected_runtime};
    use komposer_repository::{git, version_guesser};
    use md5::{Digest, Md5};
    use serde_json::{Value, json};
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    /// `StatusCommand::execute()`: 1 for local changes, +2 for unpushed changes, +4 for version
    /// variations.
    pub fn status(io: &Arc<Io>, run_scripts: bool) -> Result<i32> {
        let c = Composer::load(io.clone())?;
        c.scripts(run_scripts).dispatch("pre-status-cmd", &[])?;
        let code = local_changes(&c)?;
        c.scripts(run_scripts).dispatch("post-status-cmd", &[])?;
        Ok(code)
    }

    /// A PHP array keyed by install path: later values replace earlier ones in place.
    fn set(list: &mut Vec<(String, String)>, key: &str, value: String) {
        match list.iter_mut().find(|(k, _)| k == key) {
            Some(entry) => entry.1 = value,
            None => list.push((key.to_string(), value)),
        }
    }

    /// `StatusCommand::doExecute()`.
    fn local_changes(c: &Composer) -> Result<i32> {
        let io = &c.io;
        let (mut errors, mut unpushed, mut versions) = (vec![], vec![], vec![]);
        let packages = InstalledRepo::load(&c.installed_json().to_string_lossy(), None)?.packages.canonical();
        for p in &packages {
            let Some(target) = c.install_path(p) else { continue };
            let dir = target.display().to_string();
            let (from_source, kind) = match p.installation_source.as_deref() {
                Some("dist") => (false, p.dist.as_ref().map_or("", |d| d.kind.as_str())),
                Some("source") => (true, p.source.as_ref().map_or("", |s| s.kind.as_str())),
                _ => bail!("Package {p} does not have an installation source set"),
            };
            let vcs = matches!(kind, "git" | "svn" | "hg" | "fossil" | "perforce");
            if target.is_symlink() {
                set(&mut errors, &dir, format!("{dir} is a symbolic link."));
            }
            // ponytail: only git checkouts and archives are compared; svn/hg/fossil/perforce and path installs report nothing
            let changes = match kind {
                "git" => git_local_changes(&target)?,
                "zip" | "rar" | "tar" | "gzip" | "xz" | "phar" | "file" => dist_changes(c, p, &target),
                _ => None,
            };
            if let Some(changes) = changes {
                set(&mut errors, &dir, changes);
            }
            if vcs || kind == "path" {
                let guessed = version_guesser::guess_version(&dumper::dump(p), &target, io).filter(|g| g.commit.is_some());
                let previous = if from_source { p.source_reference() } else { p.dist_reference() };
                if let (Some(current), Some(previous)) = (guessed, previous.filter(|r| !r.is_empty()))
                    && current.commit.as_deref() != Some(previous)
                    && current.pretty_version != previous
                {
                    versions.push((
                        dir.clone(),
                        (p.pretty_version.clone(), previous.to_string()),
                        (current.pretty_version, current.commit.unwrap_or_default()),
                    ));
                }
            }
            if kind == "git"
                && let Some(changes) = git_unpushed_changes(&target)?
            {
                set(&mut unpushed, &dir, changes);
            }
        }

        if errors.is_empty() && unpushed.is_empty() && versions.is_empty() {
            io.write_error("<info>No local changes</info>");
            return Ok(0);
        }
        let verbose = io.is_verbose();
        let list = |entries: &[(String, String)]| {
            for (path, changes) in entries {
                if verbose {
                    io.write(&format!("<info>{path}</info>:"));
                    io.write(&changes.split('\n').map(|l| format!("    {}", l.trim_start())).collect::<Vec<_>>().join("\n"));
                } else {
                    io.write(path);
                }
            }
        };
        if !errors.is_empty() {
            io.write_error("<error>You have changes in the following dependencies:</error>");
            list(&errors);
        }
        if !unpushed.is_empty() {
            io.write_error("<warning>You have unpushed changes on the current branch in the following dependencies:</warning>");
            list(&unpushed);
        }
        if !versions.is_empty() {
            io.write_error("<warning>You have version variations in the following dependencies:</warning>");
            for (path, (prev_version, prev_ref), (cur_version, cur_ref)) in &versions {
                if !verbose {
                    io.write(path);
                    continue;
                }
                // without a version, the ref is shown instead
                let mut current = if cur_version.is_empty() { cur_ref.clone() } else { cur_version.clone() };
                let mut previous = if prev_version.is_empty() { prev_ref.clone() } else { prev_version.clone() };
                if io.is_very_verbose() {
                    current.push_str(&format!(" ({cur_ref})"));
                    previous.push_str(&format!(" ({prev_ref})"));
                }
                io.write(&format!("<info>{path}</info>:"));
                io.write(&format!("    From <comment>{previous}</comment> to <comment>{current}</comment>"));
            }
        }
        if !verbose {
            io.write_error("Use --verbose (-v) to see a list of files");
        }
        Ok(i32::from(!errors.is_empty()) + 2 * i32::from(!unpushed.is_empty()) + 4 * i32::from(!versions.is_empty()))
    }

    fn git_ok(args: &[&str], path: &Path) -> Result<String> {
        let out = git::run(args, Some(path));
        if !out.ok() {
            bail!("Failed to execute git {}\n\n{}", args.join(" "), out.stderr);
        }
        Ok(out.stdout)
    }

    /// `GitDownloader::getLocalChanges()`.
    fn git_local_changes(path: &Path) -> Result<Option<String>> {
        if !path.join(".git").is_dir() {
            return Ok(None);
        }
        let out = git_ok(&["status", "--porcelain", "--untracked-files=no"], path)?;
        Ok(Some(out.trim().to_string()).filter(|s| !s.is_empty()))
    }

    /// `GitDownloader::getUnpushedChanges()`.
    fn git_unpushed_changes(path: &Path) -> Result<Option<String>> {
        if !path.join(".git").is_dir() {
            return Ok(None);
        }
        komposer_installer::unpushed_changes(&|args| git_ok(args, path))
    }

    /// `FileDownloader::getLocalChanges()`: extracts the dist again (from the cache when it can) next
    /// to the install and compares the two trees.
    fn dist_changes(c: &Composer, p: &Package, target: &Path) -> Option<String> {
        let compare = PathBuf::from(format!("{}_compare", target.display().to_string().trim_end_matches('/')));
        let _ = std::fs::remove_dir_all(&compare);
        let extract = || -> Result<String> {
            let vendor = compare.display().to_string();
            let mut config = c.config.clone();
            config.merge_config(&json!({"vendor-dir": vendor, "bin-dir": format!("{vendor}/bin")}));
            let quiet = Io::buffered(Verbosity::Quiet);
            let fetch = fetcher(c.http.clone());
            let mut im = InstallationManager::new(&config, &quiet, &fetch);
            im.prefer_dist = true;
            let package: PackageRef = Arc::new(p.clone());
            im.execute(&mut InstalledRepo::in_memory(), &[Operation::Install(package.clone())], true, false, None)?;
            let fresh = im.install_path(&package).unwrap_or(vendor);
            Ok(compare_trees(Path::new(&fresh), target))
        };
        let result = extract();
        let _ = std::fs::remove_dir_all(&compare);
        match result {
            Ok(changes) => (!changes.is_empty()).then_some(changes),
            Err(e) => Some(format!("Failed to detect changes: [RuntimeException] {e}")),
        }
    }

    /// `Comparer::doCompare()` and `getChangedAsString()`: the changed and removed files (in the
    /// order their section first shows up), then the added ones.
    fn compare_trees(source: &Path, update: &Path) -> String {
        let (from, to) = (tree(source), tree(update));
        let mut sections: Vec<Vec<&String>> = vec![];
        let mut kinds = vec![];
        for (path, hash) in &from {
            let kind = match to.get(path) {
                Some(h) if h == hash => continue,
                Some(_) => "changed",
                None => "removed",
            };
            match kinds.iter().position(|k| *k == kind) {
                Some(i) => sections[i].push(path),
                None => {
                    kinds.push(kind);
                    sections.push(vec![path]);
                }
            }
        }
        sections.push(to.keys().filter(|path| !from.contains_key(*path)).collect());
        // Composer separates entries with "\r\n\r\n"
        let entries: Vec<String> = sections.into_iter().flatten().map(|p| format!("{p}\r\n")).collect();
        entries.join("\r\n").trim().to_string()
    }

    /// `./relative/path` => content hash (or link target) of every non-empty file below `root`.
    // ponytail: sorted paths instead of PHP's readdir order
    fn tree(root: &Path) -> BTreeMap<String, String> {
        fn walk(root: &Path, rel: &str, out: &mut BTreeMap<String, String>) {
            let Ok(dir) = std::fs::read_dir(root.join(rel)) else { return };
            for entry in dir.flatten() {
                let path = format!("{rel}/{}", entry.file_name().to_string_lossy());
                let full = root.join(&path);
                let Ok(meta) = std::fs::symlink_metadata(&full) else { continue };
                if meta.is_symlink() {
                    out.insert(path, format!("link:{}", std::fs::read_link(&full).map(|l| l.display().to_string()).unwrap_or_default()));
                } else if meta.is_dir() {
                    walk(root, &path, out);
                } else if meta.len() > 0
                    && let Ok(bytes) = std::fs::read(&full)
                {
                    out.insert(path, Md5::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect());
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(root, ".", &mut out);
        out
    }

    /// `DiagnoseCommand::execute()`, with the checks that apply to komposer (no PHP extension,
    /// phar or self-update checks). Exit code 2 for failures, 1 for warnings.
    pub fn diagnose(io: &Arc<Io>) -> Result<i32> {
        let composer = Composer::load(io.clone()).ok();
        let mut config = match &composer {
            Some(c) => c.config.clone(),
            None => Config::create(&std::env::current_dir()?)?,
        };
        config.merge_config(&json!({"secure-http": false}));
        // Composer pre-marks http://repo.packagist.org as warned about; this client stays quiet instead
        let http = HttpClient::new(Arc::new(Io::buffered(Verbosity::Quiet)), &config)?;
        let mut d = Diagnose { io, exit: 0 };

        io.write(&format!(
            "Komposer version: <comment>{} (Composer {} compatible)</comment>",
            env!("CARGO_PKG_VERSION"),
            komposer_repository::COMPOSER_VERSION
        ));
        let platform = PlatformRepository::new(&config.get("platform").unwrap_or(json!([])), detected_runtime())?;
        match platform.find_package("php", &komposer_semver::Constraint::Any)? {
            Some(php) => {
                let description = php.meta.get("description").and_then(Value::as_str).unwrap_or("");
                let suffix = if description.contains("overridden") { format!(" - {description}") } else { String::new() };
                io.write(&format!("PHP version: <comment>{}{suffix}</comment>", php.pretty_version));
            }
            None => io.write("PHP version: <comment>not found (optional, komposer only needs PHP to run PHP scripts and to check php/ext-* requirements)</comment>"),
        }
        if let Some(binary) = detected_runtime().and_then(|rt| rt.constants.get("PHP_BINARY")).and_then(Value::as_str) {
            io.write(&format!("PHP binary path: <comment>{binary}</comment>"));
        }

        if let Some(c) = &composer {
            d.check("composer.json", composer_schema());
            if c.locker.is_locked().unwrap_or(false) {
                d.check("composer.lock", lock_schema(c));
            }
        }
        d.check("git settings", check_git());
        d.check("http connectivity to packagist", check_url(&http, &config, "http://repo.packagist.org/packages.json"));
        d.check("https connectivity to packagist", check_url(&http, &config, "https://repo.packagist.org/packages.json"));
        for repo in config.repositories() {
            let (Some("composer"), Some(url)) = (repo.get("type").and_then(Value::as_str), repo.get("url").and_then(Value::as_str)) else { continue };
            let packages_json = if url.ends_with(".json") { url.to_string() } else { format!("{}/packages.json", url.trim_end_matches('/')) };
            if packages_json.starts_with("http") && !packages_json.starts_with("https://repo.packagist.org") {
                d.check(&format!("connectivity to {url}"), check_url(&http, &config, &packages_json));
            }
        }
        match config.get("github-oauth").and_then(|o| o.as_object().cloned()).filter(|o| !o.is_empty()) {
            Some(tokens) => {
                for domain in tokens.keys() {
                    d.check(&format!("{domain} oauth access"), check_github_oauth(&http, domain));
                }
            }
            None => d.github_rate_limit(&http),
        }
        d.check("disk free space", check_disk_space(&config));
        for (label, key) in [("home directory is writable", "home"), ("cache directory is writable", "cache-dir")] {
            d.check(label, check_writable(&config.get_path(key)));
        }
        Ok(d.exit)
    }

    /// A check result: `Ok` prints OK, messages decide FAIL (`<error>`) or WARNING (`<warning>`).
    type Check = Result<(), Vec<String>>;

    struct Diagnose<'a> {
        io: &'a Io,
        exit: i32,
    }

    impl Diagnose<'_> {
        /// `outputResult()`, on the `Checking {label}: ` line.
        fn check(&mut self, label: &str, result: Check) {
            let mut lines = vec![];
            match result {
                Ok(()) => lines.push("<info>OK</info>".to_string()),
                Err(messages) => {
                    if messages.is_empty() || messages.iter().any(|m| m.contains("<error>")) {
                        lines.push("<error>FAIL</error>".into());
                        self.exit = self.exit.max(2);
                    } else if messages.iter().any(|m| m.contains("<warning>")) {
                        lines.push("<warning>WARNING</warning>".into());
                        self.exit = self.exit.max(1);
                    }
                    lines.extend(messages.iter().map(|m| m.trim().to_string()));
                }
            }
            self.io.write(&format!("Checking {label}: {}", lines[0]));
            for line in &lines[1..] {
                self.io.write(line);
            }
        }

        /// The `github.com rate limit` check, which warns without changing the exit code.
        fn github_rate_limit(&mut self, http: &HttpClient) {
            if let Err(skip) = network_enabled() {
                return self.check("github.com rate limit", Err(skip));
            }
            let opts = RequestOptions { retry_auth_failure: false, ..RequestOptions::default() };
            let rate = http.get("https://api.github.com/rate_limit", &opts).and_then(|r| r.json());
            let core = match rate {
                Ok(data) => data["resources"]["core"].clone(),
                Err(e) if error_status(&e) == Some(401) => {
                    return self.check("github.com rate limit", Err(vec![
                        "<comment>The oauth token for github.com seems invalid, run \"composer config --global --unset github-oauth.github.com\" to remove it</comment>".into(),
                    ]));
                }
                Err(e) => return self.check("github.com rate limit", Err(vec![transport_error(&e)])),
            };
            let remaining = core["remaining"].as_u64().unwrap_or(0);
            if remaining >= 10 {
                return self.check("github.com rate limit", Ok(()));
            }
            self.io.write("Checking github.com rate limit: <warning>WARNING</warning>");
            self.io.write(&format!(
                "<comment>GitHub has a rate limit on their API. You currently have {remaining} out of {} requests left.\nSee https://developer.github.com/v3/#rate-limiting and also\n    https://getcomposer.org/doc/articles/troubleshooting.md#api-rate-limit-and-oauth-tokens</comment>",
                core["limit"].as_u64().unwrap_or(0)
            ));
        }
    }

    fn transport_error(e: &anyhow::Error) -> String {
        format!("<error>[Composer\\Downloader\\TransportException] {e}</error>")
    }

    /// `checkConnectivityAndComposerNetworkHttpEnablement()`.
    fn network_enabled() -> Check {
        if std::env::var("COMPOSER_DISABLE_NETWORK").is_ok_and(|v| !v.is_empty() && v != "0") {
            return Err(vec!["<info>SKIP</> <comment>Network is disabled by COMPOSER_DISABLE_NETWORK.</>".into()]);
        }
        Ok(())
    }

    /// `checkComposerSchema()`: errors and warnings of `ConfigValidator::validate()`.
    fn composer_schema() -> Check {
        let file = composer_file().map_err(|e| vec![format!("<error>{e}</error>")])?;
        let r = validate_file(&file, 3, true);
        if r.errors.is_empty() && r.warnings.is_empty() {
            return Ok(());
        }
        let lines: Vec<String> =
            r.errors.iter().map(|e| format!("<error>{e}</error>")).chain(r.warnings.iter().map(|w| format!("<warning>{w}</warning>"))).collect();
        Err(vec![lines.join("\n")])
    }

    /// `checkComposerLockSchema()`.
    fn lock_schema(c: &Composer) -> Check {
        let data = std::fs::read_to_string(c.locker.path()).ok().and_then(|s| serde_json::from_str::<Value>(&s).ok()).unwrap_or(Value::Null);
        let errors = lock_schema_errors(&data);
        if errors.is_empty() {
            return Ok(());
        }
        Err(vec![errors.iter().map(|e| format!("<error>{e}</error>")).collect::<Vec<_>>().join("\n")])
    }

    /// `checkGit()`.
    fn check_git() -> Check {
        let color = git::run(&["config", "color.ui"], None);
        if color.stdout.trim().eq_ignore_ascii_case("always") {
            return Err(vec!["<comment>Your git color.ui setting is set to always, this is known to create issues. Use \"git config --global color.ui true\" to set it correctly.</comment>".into()]);
        }
        let Some(version) = git::version() else { return Err(vec!["<comment>No git process found</>".into()]) };
        if komposer_semver::version_compare("2.24.0", &version) == std::cmp::Ordering::Greater {
            return Err(vec![format!(
                "<warning>Your git version ({version}) is too old and possibly will cause issues. Please upgrade to git 2.24 or above</>"
            )]);
        }
        Err(vec![format!("<info>OK</> <comment>git version {version}</>")])
    }

    /// `checkHttp()` / `checkComposerRepo()`.
    fn check_url(http: &HttpClient, config: &Config, url: &str) -> Check {
        network_enabled()?;
        let mut result = vec![];
        if let Err(e) = http.get(url, &RequestOptions::default()) {
            result.push(transport_error(&e));
        }
        if url.starts_with("https://") && config.get_bool("disable-tls") {
            result.push("<warning>Composer is configured to disable SSL/TLS protection. This will leave remote HTTPS requests vulnerable to Man-In-The-Middle attacks.</warning>".into());
        }
        if result.is_empty() { Ok(()) } else { Err(result) }
    }

    /// `checkGithubOauth()`.
    fn check_github_oauth(http: &HttpClient, domain: &str) -> Check {
        network_enabled()?;
        let url = if domain == "github.com" { "https://api.github.com/".to_string() } else { format!("https://{domain}/api/v3/") };
        match http.get(&url, &RequestOptions { retry_auth_failure: false, ..RequestOptions::default() }) {
            Ok(r) => Err(vec![match r.header("github-authentication-token-expiration") {
                Some(expiration) => format!("<info>OK</> <comment>expires on {expiration}</>"),
                None => "<info>OK</> <comment>does not expire</>".into(),
            }]),
            Err(e) if error_status(&e) == Some(401) => Err(vec![format!(
                "<comment>The oauth token for {domain} seems invalid, run \"composer config --global --unset github-oauth.{domain}\" to remove it</comment>"
            )]),
            Err(e) => Err(vec![transport_error(&e)]),
        }
    }

    /// `checkDiskSpace()`: less than 1MiB free where home or vendor-dir live.
    fn check_disk_space(config: &Config) -> Check {
        for dir in [config.home(), config.get_path("vendor-dir")] {
            // ponytail: `df -Pk` stands in for disk_free_space(); unavailable on Windows, which then passes
            let out = std::process::Command::new("df").arg("-Pk").arg(&dir).output();
            let free_kb = out.ok().filter(|o| o.status.success()).and_then(|o| {
                let text = String::from_utf8_lossy(&o.stdout).into_owned();
                text.lines().nth(1)?.split_whitespace().nth(3)?.parse::<u64>().ok()
            });
            if free_kb.is_some_and(|kb| kb * 1024 < 1024 * 1024) {
                return Err(vec![format!("<error>The disk hosting {} is full</error>", dir.display())]);
            }
        }
        Ok(())
    }

    /// Whether files can be created in `dir` (or where it would be created).
    fn check_writable(dir: &Path) -> Check {
        if dir == Path::new("/dev/null") {
            return Ok(());
        }
        let existing = dir.ancestors().find(|d| d.is_dir()).unwrap_or(dir);
        let probe = existing.join(format!(".komposer-diagnose-{}", std::process::id()));
        match std::fs::write(&probe, b"") {
            Ok(()) => {
                let _ = std::fs::remove_file(&probe);
                Ok(())
            }
            Err(e) => Err(vec![format!("<error>{} is not writable: {e}</error>", existing.display())]),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn trees_compare_like_composer() {
            let root = std::env::temp_dir().join(format!("komposer-status-{}", std::process::id()));
            let (a, b) = (root.join("a"), root.join("b"));
            for dir in [&a, &b] {
                std::fs::create_dir_all(dir.join("src")).unwrap();
                std::fs::write(dir.join("src/same.php"), "x").unwrap();
                std::fs::write(dir.join("empty.txt"), "").unwrap();
            }
            std::fs::write(a.join("src/changed.php"), "1").unwrap();
            std::fs::write(b.join("src/changed.php"), "2").unwrap();
            std::fs::write(a.join("removed.php"), "r").unwrap();
            std::fs::write(b.join("added.php"), "a").unwrap();
            std::fs::write(b.join("empty-added.txt"), "").unwrap();
            assert_eq!(compare_trees(&a, &b), "./removed.php\r\n\r\n./src/changed.php\r\n\r\n./added.php");
            assert_eq!(compare_trees(&a, &a), "");
            std::fs::remove_dir_all(&root).unwrap();
        }
    }
}
