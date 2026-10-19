//! Composer's installer integration fixtures (`tests/Composer/Test/Fixtures/installer/*.test`) run
//! through komposer's `Installer` like `InstallerTest::doTestIntegration()` does: packagist disabled,
//! an imaginary PHP 8.3.30 platform, operations recorded instead of executed (the `trace` seam of
//! `InstallationManagerMock`), audits off. Every fixture runs with and without the pool optimizer.
//!
//! The fixtures are read from `KOMPOSER_INSTALLER_FIXTURES` (default: the reference checkout on the T9
//! drive); the test is skipped when they are missing. `FIXTURE=<prefix>` runs only matching fixtures.

use anyhow::{Result, bail};
use komposer::composer::Composer;
use komposer::installer::Installer;
use komposer_installer::InstalledRepo;
use komposer_package::io::{Io, Verbosity};
use komposer_package::json::{self, Flags};
use komposer_repository::platform::PhpRuntime;
use komposer_solver::{PlatformRequirementFilter, UpdateAllowTransitive};
use md5::{Digest, Md5};
use regex::Regex;
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};

/// `InstallerTest::readTestFile()`.
fn sections(text: &str) -> HashMap<String, String> {
    let re = Regex::new(r"(?:^|\n*)--([A-Z-]+)--\n").unwrap();
    let mut out = HashMap::new();
    let mut last: Option<(String, usize)> = None;
    for c in re.captures_iter(text) {
        let m = c.get(0).unwrap();
        if let Some((name, start)) = last.take() {
            out.insert(name, text[start..m.start()].to_string());
        }
        last = Some((c[1].to_string(), m.end()));
    }
    if let Some((name, start)) = last {
        out.insert(name, text[start..].to_string());
    }
    out
}

/// PHPUnit's `assertStringMatchesFormat()`.
fn matches_format(format: &str, actual: &str) -> bool {
    let mut re = String::from("(?s)^");
    let mut chars = format.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            re += &regex::escape(&c.to_string());
            continue;
        }
        re += match chars.next() {
            Some('s') => "[^\\r\\n]+",
            Some('S') => "[^\\r\\n]*",
            Some('a') => ".+",
            Some('A') => ".*",
            Some('w') => "\\s*",
            Some('i') => "[+-]?\\d+",
            Some('d') => "\\d+",
            Some('x') => "[0-9a-fA-F]+",
            Some('f') => "[+-]?\\.?\\d+\\.?\\d*(?:[Ee][+-]?\\d+)?",
            Some('c') => ".",
            Some('e') => "/",
            Some('%') | None => "%",
            Some(other) => {
                re += &regex::escape(&format!("%{other}"));
                continue;
            }
        };
    }
    re += "$";
    Regex::new(&re).unwrap().is_match(actual)
}

/// The PHP the fixtures run on: 8.3.30 with a handful of extensions.
fn php() -> PhpRuntime {
    let exts = ["Core", "ctype", "date", "filter", "intl", "json", "mbstring", "openssl", "pcre", "standard", "tokenizer", "xml"];
    PhpRuntime::from_json(&json!({
        "constants": {
            "PHP_VERSION": "8.3.30", "PHP_INT_SIZE": 8, "AF_INET6": 30, "INTL_ICU_VERSION": "74.2",
            "PCRE_VERSION": "10.42 2022-12-11", "OPENSSL_VERSION_TEXT": "OpenSSL 3.0.13 30 Jan 2024",
        },
        "extensions": exts.map(|e| [e, "8.3.30"]),
        "ipv6": true,
        "ini": ["/etc/php/8.3/cli/php.ini"],
    }))
}

/// Composer's test `--RUN--` commands (`install`/`update` with the options InstallerTest registers).
fn configure(installer: &mut Installer, run: &str) -> Result<()> {
    let mut args = run.split_whitespace();
    let update = match args.next() {
        Some("update") => true,
        Some("install") => false,
        other => bail!("The run command only supports install and update, got {other:?}"),
    };
    installer.audit = false;
    installer.update = update;
    let (mut packages, mut ignore): (Vec<String>, Vec<String>) = (vec![], vec![]);
    let (mut ignore_all, mut mirrors, mut with_all, mut with_deps) = (false, false, false, false);
    for arg in args {
        match arg {
            "-v" | "-vv" | "-vvv" => {}
            "--no-dev" => installer.dev_mode = false,
            "--dry-run" => installer.dry_run = true,
            "--ignore-platform-reqs" => ignore_all = true,
            a if a.starts_with("--ignore-platform-req=") => ignore.push(a["--ignore-platform-req=".len()..].to_string()),
            "--no-install" if update => installer.install = false,
            "--lock" if update => mirrors = true,
            "--with-all-dependencies" | "-W" if update => with_all = true,
            "--with-dependencies" | "-w" if update => with_deps = true,
            "--prefer-stable" if update => installer.prefer_stable = true,
            "--prefer-lowest" if update => installer.prefer_lowest = true,
            "--minimal-changes" if update => installer.minimal_update = true,
            a if a.starts_with('-') => bail!("The \"{a}\" option does not exist."),
            "lock" | "nothing" | "mirrors" if update => mirrors = true,
            name if update => {
                let name = name.to_lowercase();
                if !packages.contains(&name) {
                    packages.push(name);
                }
            }
            a => bail!("No arguments expected for \"install\" command, got \"{a}\"."),
        }
    }
    installer.update_mirrors = mirrors;
    installer.update_allow_list = (!packages.is_empty()).then_some(packages);
    installer.update_allow_transitive = match (with_all, with_deps) {
        (true, _) => UpdateAllowTransitive::WithDeps,
        (false, true) => UpdateAllowTransitive::WithDepsNoRootRequire,
        _ => UpdateAllowTransitive::OnlyListed,
    };
    installer.platform_filter = if ignore_all {
        PlatformRequirementFilter::IgnoreAll
    } else if ignore.is_empty() {
        PlatformRequirementFilter::IgnoreNothing
    } else {
        PlatformRequirementFilter::ignore_list(&ignore)
    };
    Ok(())
}

fn md5_hex(s: &str) -> String {
    Md5::digest(s.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

/// `ArrayDumper::dump()` of the local repository's canonical packages, without `version_normalized`.
fn installed_packages(file: &Path) -> Result<Value> {
    let repo = InstalledRepo::load(&file.to_string_lossy(), None)?;
    let mut packages: Vec<Map<String, Value>> = repo
        .packages
        .canonical()
        .iter()
        .map(|p| {
            let mut m = komposer_package::dumper::dump(p);
            m.shift_remove("version_normalized");
            m
        })
        .collect();
    packages.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    Ok(Value::Array(packages.into_iter().map(Value::Object).collect()))
}

enum Verdict {
    Pass,
    Skip(String),
    Fail(String),
}

/// `doTestIntegration()`.
fn run_fixture(file: &Path, scratch: &Path, optimize: bool) -> Result<Verdict> {
    let fixtures_dir = file.parent().unwrap();
    let s = sections(&std::fs::read_to_string(file)?);
    match s.get("CONDITION").map(|c| c.trim()) {
        None | Some("") | Some("!defined('HHVM_VERSION')") => {}
        Some(c) => match c.strip_prefix("putenv('").and_then(|c| c.strip_suffix("')")).and_then(|c| c.split_once('=')) {
            // SAFETY: the fixtures run sequentially on this thread
            Some((k, v)) => unsafe { std::env::set_var(k, v) },
            None => return Ok(Verdict::Skip(format!("CONDITION {c}"))),
        },
    }

    let mut composer = json::parse(&s["COMPOSER"])?;
    // InstallerTest points file:// composer repositories at the fixtures directory
    for repo in composer.get_mut("repositories").and_then(Value::as_array_mut).into_iter().flatten() {
        if repo["type"] == "composer"
            && let Some(url) = repo["url"].as_str().and_then(|u| u.strip_prefix("file://")).filter(|u| !u.starts_with('/'))
        {
            repo["url"] = format!("file://{}/{url}", fixtures_dir.display()).into();
        }
    }
    let contents = json::encode(&composer, Flags::PLAIN);

    let dir = scratch.join(format!("{}-{}", file.file_stem().unwrap().to_string_lossy(), if optimize { "optimized" } else { "raw" }));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("vendor/composer"))?;
    // path repositories are relative to InstallerTest's working directory (tests/Composer/Test)
    let test_dir = fixtures_dir.parent().and_then(Path::parent).unwrap();
    for entry in ["DependencyResolver", "Fixtures"] {
        std::os::unix::fs::symlink(test_dir.join(entry), dir.join(entry))?;
    }
    std::fs::write(dir.join("composer.json"), &contents)?;

    let lock_file = dir.join("composer.lock");
    if let Some(lock) = s.get("LOCK").filter(|l| !l.trim().is_empty()) {
        let mut lock = json::parse(lock)?;
        if lock.get("hash").is_none() {
            lock["hash"] = md5_hex(&contents).into();
        }
        // written compactly so a rewrite by the Locker is always visible
        std::fs::write(&lock_file, json::encode(&lock, Flags::PLAIN))?;
    }
    let lock_before = std::fs::read(&lock_file).ok();
    let installed_file = dir.join("vendor/composer/installed.json");
    std::fs::write(&installed_file, s.get("INSTALLED").map_or("[]", |i| i.as_str()))?;

    // SAFETY: the fixtures run sequentially on this thread
    unsafe { std::env::set_var("COMPOSER_POOL_OPTIMIZER", if optimize { "1" } else { "0" }) };
    let io = Arc::new(Io::buffer_io(Verbosity::Normal));
    let php = Arc::new(php());
    // `{}` decodes to an empty PHP array
    let local = composer.as_object().cloned().unwrap_or_default();
    let result = (|| -> Result<(i32, Vec<String>)> {
        let mut c = Composer::from_parts(io.clone(), dir.clone(), dir.join("composer.json"), contents.clone(), local)?;
        // FactoryMock::createConfig()
        c.config.merge(json!({"repositories": {"packagist": false}}).as_object().unwrap());
        let mut installer = Installer::new(&mut c);
        installer.php = Some(php.clone());
        installer.trace = Some(vec![]);
        configure(&mut installer, s["RUN"].trim())?;
        let code = installer.run()?;
        Ok((code, installer.trace.take().unwrap_or_default()))
    })();
    let output = io.output().replace('\r', "");
    let mut failures = vec![];

    if let Some(exception) = s.get("EXPECT-EXCEPTION") {
        return Ok(match result {
            Err(e) if format!("{e:#}").contains(s["EXPECT"].trim_end()) => Verdict::Pass,
            Err(e) => Verdict::Fail(format!("expected {} \"{}\", got error: {e:#}", exception.trim(), s["EXPECT"].trim_end())),
            Ok((code, _)) => Verdict::Fail(format!("expected {}, got exit code {code}", exception.trim())),
        });
    }
    let (code, trace) = match result {
        Ok(r) => r,
        Err(e) => return Ok(Verdict::Fail(format!("error: {e:#}\noutput:\n{output}"))),
    };

    let expected_code: i32 = s.get("EXPECT-EXIT-CODE").map(|c| c.trim().parse()).transpose()?.unwrap_or(0);
    if code != expected_code {
        failures.push(format!("exit code {code} instead of {expected_code}"));
    }

    let lock_written = std::fs::read(&lock_file).ok() != lock_before;
    match s.get("EXPECT-LOCK").map(|l| l.trim()).filter(|l| !l.is_empty()) {
        Some("false") if lock_written => failures.push("the lock file was written".into()),
        Some("false") | None => {}
        Some(_) if !lock_written => failures.push("the lock file was not written".into()),
        Some(expected) => {
            let mut actual = json::read(&lock_file)?;
            for key in ["hash", "content-hash", "_readme", "plugin-api-version"] {
                actual.as_object_mut().unwrap().shift_remove(key);
            }
            let expected = json::parse(expected)?;
            if actual != expected {
                failures.push(format!(
                    "lock differs\nexpected:\n{}\nactual:\n{}",
                    json::encode(&expected, Flags::FILE),
                    json::encode(&actual, Flags::FILE)
                ));
            }
        }
    }

    if let Some(expected) = s.get("EXPECT-INSTALLED").filter(|i| !i.trim().is_empty()) {
        let (expected, actual) = (json::parse(expected)?, installed_packages(&installed_file)?);
        // assertSame: key order matters too
        let (expected, actual) = (json::encode(&expected, Flags::FILE), json::encode(&actual, Flags::FILE));
        if expected != actual {
            failures.push(format!("installed packages differ\nexpected:\n{expected}\nactual:\n{actual}"));
        }
    }

    let (expected_trace, trace) = (s["EXPECT"].trim_end(), trace.join("\n"));
    if expected_trace != trace {
        failures.push(format!("operations differ\nexpected:\n{expected_trace}\nactual:\n{trace}"));
    }

    let expected_output = if optimize { s.get("EXPECT-OUTPUT-OPTIMIZED").or(s.get("EXPECT-OUTPUT")) } else { s.get("EXPECT-OUTPUT") };
    if let Some(expected) = expected_output.filter(|o| !o.is_empty()) {
        static INI: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^    - .*?\.ini$").unwrap());
        static INIS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(__inilist__\r?\n)+").unwrap());
        let actual = INIS.replace_all(&INI.replace_all(&output, "__inilist__"), "__inilist__\n").into_owned();
        if !matches_format(expected.trim_end(), actual.trim_end()) {
            failures.push(format!("output differs\nexpected:\n{}\nactual:\n{}", expected.trim_end(), actual.trim_end()));
        }
    }

    Ok(if failures.is_empty() { Verdict::Pass } else { Verdict::Fail(failures.join("\n")) })
}

/// Fixtures expected to fail: they test Composer master (not the 2.10.3 target) or need what komposer
/// leaves out.
const KNOWN_DIVERGENCES: &[(&str, &str)] = &[
    ("install-policy-custom-list-block-scope-install.test", "master: custom lists with block-scope (2.10.3 never blocks them on install)"),
    ("partial-update-policy-cooldown-locked-package-kept.test", "master: cooldown policy and published-time"),
    ("update-abandoned-package-required-but-blocked-via-audit-config.test", "master wording of the abandoned problem"),
    ("update-policy-cooldown-advisory-blocked-older-versions.test", "master: cooldown policy"),
    ("update-policy-cooldown-composer-repo-without-published-time.test", "master: cooldown policy"),
    ("update-policy-cooldown-problem-output.test", "master: cooldown policy"),
    ("update-policy-cooldown-time-fallback.test", "master: cooldown policy"),
    ("update-policy-cooldown-withholds-new-version.test", "master: cooldown policy"),
    ("update-security-advisory-branch-alias-matching.test", "master: dev versions are matched against advisories"),
];

#[test]
fn composer_installer_fixtures() {
    let dir = PathBuf::from(
        std::env::var("KOMPOSER_INSTALLER_FIXTURES")
            .unwrap_or_else(|_| "/Volumes/T9/tmp/komposer-ref/composer/tests/Composer/Test/Fixtures/installer".into()),
    );
    if !dir.is_dir() {
        eprintln!("skipped: no fixtures at {}", dir.display());
        return;
    }
    let scratch = std::env::temp_dir().join(format!("komposer-fixtures-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();
    // SAFETY: set before anything reads the environment
    unsafe {
        std::env::set_var("COMPOSER_HOME", &home);
        for var in ["COMPOSER", "COMPOSER_ROOT_VERSION", "COMPOSER_VENDOR_DIR", "COMPOSER_NO_BLOCKING", "COMPOSER_NO_SECURITY_BLOCKING"] {
            std::env::remove_var(var);
        }
    }

    let mut files: Vec<PathBuf> =
        std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()).filter(|p| p.extension().is_some_and(|e| e == "test")).collect();
    files.sort();
    let (mut passed, mut skipped, mut failed) = (vec![], vec![], vec![]);
    for f in &files {
        let name = f.file_name().unwrap().to_string_lossy().to_string();
        if std::env::var("FIXTURE").is_ok_and(|only| !name.starts_with(&only)) {
            continue;
        }
        // testIntegrationWithRawPool and testIntegrationWithPoolOptimizer
        for optimize in [false, true] {
            let verdict = run_fixture(f, &scratch, optimize);
            // SAFETY: as above (InstallerTest::tearDown())
            unsafe { std::env::remove_var("COMPOSER_FUND") };
            let mode = if optimize { "optimized" } else { "raw" };
            match verdict {
                Ok(Verdict::Pass) => passed.push(format!("{name} ({mode})")),
                Ok(Verdict::Skip(why)) => skipped.push(format!("{name} ({mode}): {why}")),
                Ok(Verdict::Fail(why)) => failed.push(format!("{name} ({mode}): {why}")),
                Err(e) => failed.push(format!("{name} ({mode}): harness error {e:#}")),
            }
        }
    }
    let _ = std::fs::remove_dir_all(&scratch);
    eprintln!("{} passed, {} skipped, {} failed\n{}", passed.len(), skipped.len(), failed.len(), skipped.join("\n"));
    let known = |f: &String| KNOWN_DIVERGENCES.iter().any(|(name, _)| f.starts_with(&format!("{name} (")));
    let unexpected: Vec<&String> = failed.iter().filter(|f| !known(f)).collect();
    let fixed: Vec<&str> = KNOWN_DIVERGENCES.iter().map(|(n, _)| *n).filter(|n| passed.iter().any(|p| p.starts_with(&format!("{n} (")))).collect();
    assert!(
        unexpected.is_empty() && fixed.is_empty(),
        "unexpected failures:\n\n{}\n\nno longer failing: {fixed:?}",
        unexpected.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("\n\n")
    );
}
