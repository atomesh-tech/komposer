//! `EventDispatcher` for the root package's `scripts`.
//!
//! Shell commands, `@php`, `@composer`, `@putenv` and `@other-script` references run like in Composer. PHP
//! callbacks (`Class::method`) and Command classes run with the installed PHP on komposer's own implementation of
//! the Composer API they use (`res/script-runtime.php`); without PHP they are skipped with a warning, like `@php`.

use crate::composer::Composer;
use anyhow::{Context, Result, bail};
use komposer_package::io::{Io, Verbosity};
use komposer_package::operation::Operation;
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::sync::{LazyLock, OnceLock};
use std::time::{Duration, Instant};

pub struct Scripts<'a> {
    pub io: &'a Io,
    /// the root package's `scripts`
    pub scripts: Map<String, Value>,
    /// the root package's `bin`: scripts naming one run through its interpreter
    pub binaries: Vec<String>,
    pub bin_dir: PathBuf,
    pub cwd: PathBuf,
    pub enabled: bool,
    /// the project PHP listeners get to see; without it they are skipped
    pub composer: Option<&'a Composer>,
}

/// What a listener's event is besides its name: a `Composer\Script\Event` unless it is one of the others.
#[derive(Clone, Debug)]
pub enum EventKind {
    Script,
    /// `PackageEvent`: the operation being run and all of them, as [`operation_json`] makes them
    Package {
        operation: Value,
        operations: Vec<Value>,
    },
    /// `InstallerEvent` (`pre-operations-exec`)
    Installer {
        operations: Vec<Value>,
        execute_operations: bool,
    },
}

/// An install, update or uninstall the way the PHP runtime builds its `*Operation` from it.
pub fn operation_json(op: &Operation) -> Option<Value> {
    use komposer_package::dumper::dump;
    Some(match op {
        Operation::Install(p) => json!({"type": "install", "package": dump(p)}),
        Operation::Update { initial, target } => json!({"type": "update", "initial": dump(initial), "target": dump(target)}),
        Operation::Uninstall(p) => json!({"type": "uninstall", "package": dump(p)}),
        Operation::MarkAliasInstalled(_) | Operation::MarkAliasUninstalled(_) => return None,
    })
}

/// How many scripts were skipped because they need PHP.
pub static SKIPPED: AtomicUsize = AtomicUsize::new(0);

/// `ProcessExecutor::$timeout` in seconds, 0 for none: `process-timeout`, `run-script --timeout` and
/// `Composer\Config::disableProcessTimeout` set it for every script run after.
pub static TIMEOUT: AtomicU64 = AtomicU64::new(300);

/// Points at a way to run the skipped scripts, once, after a command that skipped some.
pub fn skipped_hint(io: &Io) {
    if SKIPPED.load(Relaxed) > 0 {
        io.write_error("<comment>Some scripts were skipped because they need PHP. Install PHP and run them with `komposer run-script <event>`, or run them with Composer.</comment>");
    }
}

/// `ScriptEvents::*` and friends, for `run-script --list` and reserved names.
pub const EVENTS: [&str; 19] = [
    "pre-install-cmd",
    "post-install-cmd",
    "pre-update-cmd",
    "post-update-cmd",
    "pre-status-cmd",
    "post-status-cmd",
    "pre-archive-cmd",
    "post-archive-cmd",
    "pre-autoload-dump",
    "post-autoload-dump",
    "post-root-package-install",
    "post-create-project-cmd",
    "pre-operations-exec",
    "pre-package-install",
    "post-package-install",
    "pre-package-update",
    "post-package-update",
    "pre-package-uninstall",
    "post-package-uninstall",
];

impl Scripts<'_> {
    pub fn has(&self, event: &str) -> bool {
        self.listeners(event).next().is_some()
    }

    fn listeners(&self, event: &str) -> impl Iterator<Item = &str> {
        let list = match self.scripts.get(event) {
            Some(Value::Array(a)) => a.as_slice(),
            Some(v @ Value::String(_)) => std::slice::from_ref(v),
            _ => &[],
        };
        list.iter().filter_map(Value::as_str)
    }

    /// `dispatchScript($event, $devMode, $additionalArgs)`: the highest listener return value (1 when a PHP
    /// callback returns false). Fails like Composer when a script exits non-zero.
    pub fn dispatch(&self, event: &str, args: &[String]) -> Result<i32> {
        self.dispatch_with(event, args, None, &EventKind::Script)
    }

    /// `komposer <script>`: listeners get the alias input as the `script-alias-input` flag, and commands are not
    /// echoed unless that input is empty.
    pub fn dispatch_alias(&self, event: &str, args: &[String], input: &str) -> Result<i32> {
        self.dispatch_with(event, args, Some(input), &EventKind::Script)
    }

    /// The `InstallationManager::execute()` hook running the package events of `operations`; `None` when none of
    /// them has a listener.
    pub fn package_events(&self, operations: &[Operation]) -> Option<impl FnMut(&str, &Operation) -> Result<()> + '_> {
        const PACKAGE_EVENTS: [&str; 6] = [
            "pre-package-install",
            "post-package-install",
            "pre-package-update",
            "post-package-update",
            "pre-package-uninstall",
            "post-package-uninstall",
        ];
        if !self.enabled || !PACKAGE_EVENTS.iter().any(|e| self.has(e)) {
            return None;
        }
        let operations: Vec<Value> = operations.iter().filter_map(operation_json).collect();
        Some(move |event: &str, op: &Operation| {
            let kind = EventKind::Package { operation: operation_json(op).unwrap_or_default(), operations: operations.clone() };
            self.dispatch_event(event, &kind).map(drop)
        })
    }

    /// `dispatchPackageEvent()` / `dispatchInstallerEvent()`.
    pub fn dispatch_event(&self, event: &str, kind: &EventKind) -> Result<i32> {
        self.dispatch_with(event, &[], None, kind)
    }

    fn dispatch_with(&self, event: &str, args: &[String], alias: Option<&str>, kind: &EventKind) -> Result<i32> {
        if !self.enabled {
            return Ok(0);
        }
        let skip = std::env::var("COMPOSER_SKIP_SCRIPTS").unwrap_or_default();
        if skip.split(',').map(str::trim).any(|s| s == event) {
            self.io
                .write_error_at(&format!("Skipped script listeners for <info>{event}</info> because of COMPOSER_SKIP_SCRIPTS"), Verbosity::Verbose);
            return Ok(0);
        }
        let listeners: Vec<String> = self.listeners(event).map(String::from).collect();
        let mut return_max = 0;
        for listener in listeners {
            self.ensure_bin_dir_in_path();
            let (callable, args) = match listener.contains("@no_additional_args") {
                true => (listener.replace(" @no_additional_args", "").replace("@no_additional_args", ""), vec![]),
                false => (listener.clone(), args.to_vec()),
            };
            let (returned, stopped) = self.run(event, &callable, &args, alias, kind)?;
            return_max = return_max.max(returned);
            if stopped {
                break;
            }
        }
        Ok(return_max)
    }

    /// One listener: its return value, and whether it stopped the event's propagation.
    fn run(&self, event: &str, callable: &str, args: &[String], alias: Option<&str>, kind: &EventKind) -> Result<(i32, bool)> {
        // `eventNeedsToOutput()`
        let echo = event != "__exec_command" && alias.is_none_or(str::is_empty);
        let event_with_args = if args.is_empty() { event.to_string() } else { format!("{event} ({})", args.join(", ")) };
        let is_composer_script = callable.starts_with('@') && !callable.starts_with("@php ") && !callable.starts_with("@putenv ");
        let is_php_callback = !callable.contains(' ') && callable.contains("::");
        let is_command_class = callable.contains('\\') && !callable.contains(' ') && callable.ends_with("Command");

        if is_composer_script {
            self.io.write_error_at(&format!("> {event_with_args}: {callable}"), Verbosity::Verbose);
            let mut parts = callable[1..].split(' ');
            let name = parts.next().unwrap_or_default();
            let own: Vec<String> = parts.map(String::from).collect();
            let (script_args, tokens) = match own.iter().position(|a| a == "@additional_args") {
                // like Composer: `array_splice()` returns what it removed (nothing), so the script gets no arguments,
                // and the additional ones land after the placeholder (the keys start at 1 once the name is unset)
                Some(i) => {
                    let mut tokens = own.clone();
                    let at = (i + 1).min(tokens.len());
                    tokens.splice(at..at, args.iter().cloned());
                    (vec![], tokens)
                }
                None => ([own.as_slice(), args].concat(), own),
            };
            if name == "composer" {
                let exe = std::env::current_exe()?;
                self.exec(event, callable, &format!("{} {}", shell_escape(&exe.to_string_lossy()), script_args.join(" ")))?;
                return Ok((0, false));
            }
            if !self.has(name) {
                self.io.write_error_at(&format!("<warning>You made a reference to a non-existent script {callable}</warning>"), Verbosity::Quiet);
            }
            // an alias' input follows the reference's own arguments
            let nested = alias.map(|input| format!("{} {input}", tokens.iter().map(|t| shell_escape(t)).collect::<Vec<_>>().join(" ")));
            let returned = self.dispatch_with(name, &script_args, nested.as_deref(), &EventKind::Script).inspect_err(|_| {
                self.io.write_error_at(&format!("<error>Script {callable} was called via {event}</error>"), Verbosity::Quiet);
            })?;
            return Ok((returned, false));
        }

        if is_php_callback || is_command_class {
            return self.run_php(event, callable, args, alias, echo, kind);
        }

        let escaped_args = args.iter().map(|a| shell_escape(a)).collect::<Vec<_>>().join(" ");
        let mut exec = if callable.starts_with("@putenv ") {
            callable.to_string()
        } else if callable.contains("@additional_args") {
            callable.replace("@additional_args", &escaped_args)
        } else if escaped_args.is_empty() {
            callable.to_string()
        } else {
            format!("{callable} {escaped_args}")
        };

        let php = find_php();
        if exec.starts_with("@php ") && php.is_none() {
            self.skip(event, callable, "PHP is not installed.");
            return Ok((0, false));
        }
        if self.io.is_verbose() {
            self.io.write_error(&format!("> {event}: {exec}"));
        } else if echo {
            self.io.write_error(&format!("> {exec}"));
        }

        // a root package binary runs through the interpreter its shebang names
        if let Some(bin) = self.binaries.iter().find(|bin| ends_with_word(bin, callable))
            && let Some(rest) = exec.strip_prefix(callable)
        {
            exec = format!("{} {bin}{rest}", binary_caller(&self.cwd.join(bin)));
        }

        if let Some(assignment) = exec.strip_prefix("@putenv ") {
            // SAFETY: komposer is single-threaded while scripts run
            match assignment.split_once('=') {
                Some((k, v)) => unsafe { std::env::set_var(k, v) },
                None => unsafe { std::env::remove_var(assignment) },
            }
            return Ok((0, false));
        }
        if let (Some(rest), Some(php)) = (exec.strip_prefix("@php "), &php) {
            exec = format!("{} {}", php_exec_command(php), resolve_script_path(rest, &self.cwd));
        } else if let Some(php) = php {
            // SAFETY: as above
            unsafe { std::env::set_var("PHP_BINARY", php) };
        }
        // run the komposer binary for `composer ...`, even when bin-dir has its own composer
        if let Some(rest) = exec.strip_prefix("composer ") {
            exec = format!("{} {rest}", shell_escape(&std::env::current_exe()?.to_string_lossy()));
        }
        self.exec(event, callable, &exec)?;
        Ok((0, false))
    }

    /// A PHP callback or Command class: it gets a real `Event`, from komposer's own PHP implementation of the
    /// Composer API, in a PHP process.
    fn run_php(&self, event: &str, callable: &str, args: &[String], alias: Option<&str>, echo: bool, kind: &EventKind) -> Result<(i32, bool)> {
        if callable == "Composer\\Config::disableProcessTimeout" {
            // it changes Composer's own process timeout, which is komposer's here
            if self.io.is_verbose() {
                self.io.write_error(&format!("> {event}: {callable}"));
            } else if echo {
                self.io.write_error(&format!("> {callable}"));
            }
            TIMEOUT.store(0, Relaxed);
            return Ok((0, false));
        }
        let Some(php) = find_php() else {
            self.skip(event, callable, "PHP is not installed.");
            return Ok((0, false));
        };
        let Some(composer) = self.composer else {
            self.skip(event, callable, "there is no project to run it in.");
            return Ok((0, false));
        };
        let runtime = runtime_dir()?;
        let payload = php_payload(composer, self.io, event, callable, args, alias, kind)?;
        let files = std::env::temp_dir().join(format!("komposer-script-{}-{}", std::process::id(), COUNTER.fetch_add(1, Relaxed)));
        let (payload_file, result_file) = (files.with_extension("payload.json"), files.with_extension("result.json"));
        std::fs::write(&payload_file, payload.to_string()).with_context(|| format!("Could not write {}", payload_file.display()))?;
        let status = Command::new(&php).arg(runtime.join("script-runtime.php")).arg(&payload_file).arg(&result_file).current_dir(&self.cwd).status();
        let outcome: Option<Value> = std::fs::read_to_string(&result_file).ok().and_then(|s| serde_json::from_str(&s).ok());
        let _ = (std::fs::remove_file(&payload_file), std::fs::remove_file(&result_file));
        let status = status?;
        match outcome {
            Some(o) if o.get("exception").is_none() => Ok((o["return"].as_i64().unwrap_or(0) as i32, o["stopped"].as_bool().unwrap_or(false))),
            // Symfony's Application exit code for an exception
            Some(o) => bail!(ScriptFailed(o["exception"].as_i64().filter(|c| *c > 0).map_or(1, |c| c.min(255) as i32))),
            // PHP itself failed (a fatal error), and said so
            None => bail!(ScriptFailed(status.code().filter(|c| *c != 0).unwrap_or(255))),
        }
    }

    fn skip(&self, event: &str, callable: &str, reason: &str) {
        self.io.warning(&format!("Skipped {callable} ({event}): {reason}"));
        SKIPPED.fetch_add(1, Relaxed);
    }

    /// `executeTty()`: through the shell, with Composer's process timeout.
    fn exec(&self, event: &str, callable: &str, cmd: &str) -> Result<()> {
        let mut command = Command::new("sh");
        command.arg("-c").arg(cmd).current_dir(&self.cwd);
        if !self.io.is_interactive() {
            command.stdin(Stdio::null());
        }
        let timeout = TIMEOUT.load(Relaxed);
        let Some(status) = wait(command.spawn()?, timeout)? else {
            bail!(ProcessTimedOut(format!("The process \"{cmd}\" exceeded the timeout of {timeout} seconds.")));
        };
        if !status.success() {
            let code = status.code().unwrap_or(1);
            self.io.write_error_at(
                &format!("<error>Script {callable} handling the {event} event returned with error code {code}</error>"),
                Verbosity::Quiet,
            );
            bail!(ScriptFailed(code));
        }
        Ok(())
    }

    /// `ensureBinDirIsInPath()`.
    fn ensure_bin_dir_in_path(&self) {
        let Ok(bin) = self.bin_dir.canonicalize() else { return };
        let path = std::env::var("PATH").unwrap_or_default();
        if !std::env::split_paths(&path).any(|p| p == bin) {
            let joined = std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(&path))).unwrap_or_default();
            // SAFETY: komposer is single-threaded while scripts run
            unsafe { std::env::set_var("PATH", joined) };
        }
    }
}

/// Keeps the files of PHP listeners apart within one process.
static COUNTER: AtomicUsize = AtomicUsize::new(0);

const RUNTIME: &str = include_str!("../res/script-runtime.php");

/// The PHP runtime and Composer's `ClassLoader.php` next to it, written once per version of them.
fn runtime_dir() -> Result<PathBuf> {
    let version = crate::locker::md5_hex(format!("{RUNTIME}{}", komposer_autoload::CLASS_LOADER).as_bytes());
    let dir = std::env::temp_dir().join(format!("komposer-script-runtime-{}", &version[..12]));
    for (name, contents) in [("script-runtime.php", RUNTIME), ("ClassLoader.php", komposer_autoload::CLASS_LOADER)] {
        let path = dir.join(name);
        if std::fs::read_to_string(&path).ok().as_deref() != Some(contents) {
            std::fs::create_dir_all(&dir).with_context(|| format!("Could not create {}", dir.display()))?;
            let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
            std::fs::write(&tmp, contents)
                .and_then(|()| std::fs::rename(&tmp, &path))
                .with_context(|| format!("Could not write {}", path.display()))?;
        }
    }
    Ok(dir)
}

/// Everything the PHP runtime builds the event, the `Composer` instance and the class loader from.
fn php_payload(c: &Composer, io: &Io, event: &str, listener: &str, args: &[String], alias: Option<&str>, kind: &EventKind) -> Result<Value> {
    let dev_mode = std::env::var("COMPOSER_DEV_MODE").is_ok_and(|v| v == "1");
    let installed = komposer_package::json::read(&c.installed_json()).unwrap_or(Value::Null);
    let (packages, installed_dev, dev_names) = match &installed {
        Value::Object(m) => (
            m.get("packages").cloned().unwrap_or(json!([])),
            m.get("dev").cloned().unwrap_or(Value::Null),
            m.get("dev-package-names").cloned().unwrap_or(json!([])),
        ),
        Value::Array(list) => (Value::Array(list.clone()), Value::Null, json!([])),
        _ => (json!([]), Value::Null, json!([])),
    };
    let canonical: Vec<_> = komposer_package::repository::Repository::packages(&c.installed()?)?
        .into_iter()
        .filter(|p| !p.is_alias())
        .map(|p| {
            let path = c.install_path(&p);
            (p, path)
        })
        .collect();
    let loader = komposer_autoload::loader_map(&c.config, &c.root, &canonical, &c.dir, dev_mode, io)?;
    let pairs = |map: Vec<(String, Vec<String>)>| Value::Array(map.into_iter().map(|(prefix, paths)| json!([prefix, paths])).collect());
    let config = |relative| -> Map<String, Value> {
        c.config.all().keys().map(|k| (k.clone(), crate::commands::config::config_value(&c.config, k, relative))).collect()
    };
    let mut root = c.root.raw.clone();
    root.insert("name".into(), c.root.package.pretty_name.clone().into());
    root.insert("version".into(), c.root.package.pretty_version.clone().into());
    root.insert("version_normalized".into(), c.root.package.version.clone().into());
    let flags = alias.map_or_else(|| json!({}), |input| json!({"script-alias-input": input}));
    let (kind_name, operation, operations, execute_operations) = match kind {
        EventKind::Script => ("script", Value::Null, vec![], false),
        EventKind::Package { operation, operations } => ("package", operation.clone(), operations.clone(), true),
        EventKind::Installer { operations, execute_operations } => ("installer", Value::Null, operations.clone(), *execute_operations),
    };
    let verbosity = match io.verbosity() {
        Verbosity::Quiet => 1,
        Verbosity::Normal => 2,
        Verbosity::Verbose => 4,
        Verbosity::VeryVerbose => 8,
        Verbosity::Debug => 16,
    };
    Ok(json!({
        "listener": listener,
        "io": {"verbosity": verbosity, "decorated": io.is_decorated(), "interactive": io.is_interactive()},
        "event": {
            "name": event, "kind": kind_name, "args": args, "flags": flags, "devMode": dev_mode,
            "operation": operation, "operations": operations, "executeOperations": execute_operations,
        },
        "project": {
            "root": root,
            "packages": packages,
            "devMode": installed_dev,
            "devPackageNames": dev_names,
            "config": config(false),
            "configRelative": config(true),
            "repositories": c.config.repositories(),
            "lockFile": c.locker.path(),
            "lockFresh": c.locker.is_fresh().unwrap_or(false),
            "loader": {"psr-0": pairs(loader.psr0), "psr-4": pairs(loader.psr4), "classmap": loader.classmap},
        },
    }))
}

/// Waits for `child`, killing it after `timeout` seconds (0 = never): `None` when it was killed.
fn wait(mut child: Child, timeout: u64) -> Result<Option<ExitStatus>> {
    if timeout == 0 {
        return Ok(Some(child.wait()?));
    }
    let deadline = Instant::now() + Duration::from_secs(timeout);
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// `getPhpExecCommand()`: the PHP binary with the settings Composer runs under, `memory_limit` raised the way
/// `bin/composer` raises it.
fn php_exec_command(php: &Path) -> String {
    static INI: OnceLock<[String; 3]> = OnceLock::new();
    let [allow_url_fopen, disable_functions, memory_limit] = INI.get_or_init(|| {
        let code = r#"echo ini_get("allow_url_fopen"), "\n", ini_get("disable_functions"), "\n", ini_get("memory_limit");"#;
        let out = Command::new(php).args(["-r", code]).output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
        let mut lines = out.lines().map(String::from);
        [(); 3].map(|()| lines.next().unwrap_or_default())
    });
    let env = std::env::var("COMPOSER_MEMORY_LIMIT").ok();
    format!(
        "{} -d allow_url_fopen={} -d disable_functions={} -d memory_limit={}",
        shell_escape(&php.to_string_lossy()),
        shell_escape(allow_url_fopen),
        shell_escape(disable_functions),
        shell_escape(&composer_memory_limit(memory_limit, env.as_deref()))
    )
}

/// `bin/composer`: `COMPOSER_MEMORY_LIMIT` if set, else at least 1536M unless unlimited.
fn composer_memory_limit(ini: &str, env: Option<&str>) -> String {
    if let Some(env) = env.filter(|e| !e.is_empty() && *e != "0") {
        return env.to_string();
    }
    let ini = ini.trim();
    let digits: String = ini.chars().take_while(|c| c.is_ascii_digit() || *c == '-').collect();
    let mut bytes: i64 = digits.parse().unwrap_or(0);
    for (unit, power) in [('g', 3), ('m', 2), ('k', 1)] {
        if ini.to_ascii_lowercase().ends_with(unit) {
            bytes = bytes.saturating_mul(1024i64.pow(power));
        }
    }
    if ini != "-1" && bytes < 1024 * 1024 * 1536 { "1536M".into() } else { ini.into() }
}

/// `@php foo`: a bare name that is no file here is looked up on PATH.
fn resolve_script_path(path_and_args: &str, cwd: &Path) -> String {
    let name: String = path_and_args.chars().take_while(|c| !matches!(c, '\'' | '"' | '/' | '\\') && !c.is_whitespace()).collect();
    if name.is_empty() || cwd.join(&name).exists() {
        return path_and_args.to_string();
    }
    match find_executable(&name) {
        Some(found) => format!("{}{}", found.display(), &path_and_args[name.len()..]),
        None => path_and_args.to_string(),
    }
}

/// `preg_match('{\b'.preg_quote($callable).'$}', $bin)`.
fn ends_with_word(bin: &str, callable: &str) -> bool {
    let Some(before) = bin.strip_suffix(callable) else { return false };
    let word = |c: Option<char>| c.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
    word(before.chars().next_back()) != word(callable.chars().next())
}

/// `BinaryInstaller::determineBinaryCaller()` on POSIX: the shebang's interpreter, else `php`.
fn binary_caller(bin: &Path) -> String {
    static SHEBANG: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"(?m)^#!/(?:usr/bin/env )?(?:[^/]+/)*(.+)$").unwrap());
    let first =
        std::fs::read(bin).ok().and_then(|b| b.split(|&c| c == b'\n').next().map(|l| String::from_utf8_lossy(l).into_owned())).unwrap_or_default();
    SHEBANG.captures(&first).map_or_else(|| "php".into(), |m| m[1].trim().to_string())
}

/// `ArgvInput::__toString()`'s quoting of one token.
pub fn alias_token(token: &str) -> String {
    let escape = |t: &str| if t.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') { t.to_string() } else { shell_escape(t) };
    match token.split_once('=').filter(|(opt, _)| opt.starts_with('-') && opt.len() > 1) {
        Some((opt, value)) if !value.is_empty() => format!("{opt}={}", escape(value)),
        _ if token.is_empty() || token.starts_with('-') => token.to_string(),
        _ => escape(token),
    }
}

/// Symfony's `ProcessTimedOutException`: a script ran longer than the process timeout.
#[derive(Debug)]
pub struct ProcessTimedOut(pub String);

impl std::fmt::Display for ProcessTimedOut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ProcessTimedOut {}

/// A script exited with this code; the process exits with it too.
#[derive(Debug)]
pub struct ScriptFailed(pub i32);

impl std::fmt::Display for ScriptFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Script returned with error code {}", self.0)
    }
}

impl std::error::Error for ScriptFailed {}

/// `PhpExecutableFinder::find()`: `PHP_BINARY` or `php` on PATH.
pub fn find_php() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("PHP_BINARY").map(PathBuf::from).filter(|p| p.is_file()) {
        return Some(p);
    }
    find_executable("php")
}

pub fn find_executable(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|dir| dir.join(name)).find(|p| is_executable(p))
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// `ProcessExecutor::escape()` (POSIX: always single-quoted).
pub fn shell_escape(arg: &str) -> String {
    format!("'{}'", arg.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runner(scripts: Value, dir: &Path) -> Scripts<'static> {
        let io: &'static Io = Box::leak(Box::new(Io::new(Verbosity::Quiet, Some(false), false)));
        Scripts {
            io,
            scripts: scripts.as_object().unwrap().clone(),
            binaries: vec!["bin/tool".into()],
            bin_dir: dir.join("vendor/bin"),
            cwd: dir.to_path_buf(),
            enabled: true,
            composer: None,
        }
    }

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("komposer-scripts-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn runs_shell_binaries_and_referenced_scripts() {
        let dir = tmp("run");
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(dir.join("bin/tool"), "#!/usr/bin/env sh\necho tool \"$@\" >> out.txt\n").unwrap();
        let s = runner(
            json!({
                "post-install-cmd": ["@other one", "echo b >> out.txt", "bin/tool", "@putenv KOMPOSER_TEST=x", "echo $KOMPOSER_TEST >> out.txt"],
                "other": "echo a >> out.txt",
                "fail": "exit 3",
            }),
            &dir,
        );
        assert_eq!(s.dispatch("post-install-cmd", &[]).unwrap(), 0);
        // bin/tool is not executable: it ran through `sh`, its shebang's interpreter
        assert_eq!(std::fs::read_to_string(dir.join("out.txt")).unwrap(), "a one\nb\ntool\nx\n");
        let err = s.dispatch("fail", &[]).unwrap_err();
        assert_eq!(err.downcast_ref::<ScriptFailed>().unwrap().0, 3);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// One test, as the timeout is process-wide like Composer's.
    #[test]
    fn process_timeout_and_disabling_it() {
        let dir = tmp("timeout");
        let s = runner(json!({"slow": "sleep 3", "unbounded": ["Composer\\Config::disableProcessTimeout", "sleep 2"]}), &dir);
        TIMEOUT.store(1, Relaxed);
        let started = Instant::now();
        let err = s.dispatch("slow", &[]).unwrap_err().to_string();
        assert_eq!(err, "The process \"sleep 3\" exceeded the timeout of 1 seconds.");
        assert!(started.elapsed() < Duration::from_secs(3));
        s.dispatch("unbounded", &[]).unwrap();
        assert_eq!(TIMEOUT.load(Relaxed), 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn php_callbacks_without_php_are_skipped() {
        let dir = tmp("callback");
        let s = runner(json!({"post-autoload-dump": ["Vendor\\Scripts::dump", "echo ran >> out.txt"]}), &dir);
        if find_php().is_none() {
            assert_eq!(s.dispatch("post-autoload-dump", &[]).unwrap(), 0);
            assert_eq!(std::fs::read_to_string(dir.join("out.txt")).unwrap(), "ran\n");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn memory_limit_like_bin_composer() {
        assert_eq!(composer_memory_limit("128M", None), "1536M");
        assert_eq!(composer_memory_limit("2G", None), "2G");
        assert_eq!(composer_memory_limit("-1", None), "-1");
        assert_eq!(composer_memory_limit("1610612736", None), "1610612736");
        assert_eq!(composer_memory_limit("128M", Some("4G")), "4G");
        assert_eq!(composer_memory_limit("128M", Some("0")), "1536M");
    }

    #[test]
    fn binary_callers_and_names() {
        let dir = tmp("caller");
        for (name, contents, caller) in [("a", "#!/usr/bin/env bash\n", "bash"), ("b", "#!/bin/sh -e\n", "sh -e"), ("c", "<?php\n", "php")] {
            std::fs::write(dir.join(name), contents).unwrap();
            assert_eq!(binary_caller(&dir.join(name)), caller, "{name}");
        }
        assert!(ends_with_word("bin/tool", "tool") && ends_with_word("bin/tool", "bin/tool"));
        assert!(!ends_with_word("bin/mytool", "tool") && !ends_with_word("bin/tool", "tool --x"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn escapes_like_composer() {
        assert_eq!(shell_escape("--ansi"), "'--ansi'");
        assert_eq!(shell_escape("it's"), "'it'\\''s'");
        assert_eq!(alias_token("--filter=a b"), "--filter='a b'");
        assert_eq!(alias_token("plain-word"), "plain-word");
    }
}
