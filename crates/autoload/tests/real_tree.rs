//! Checks against real projects, run with `cargo test -p komposer-autoload --test real_tree -- --ignored --nocapture`.
//!
//! - `KOMPOSER_AUTOLOAD_SAMPLE=<dir>`: `dump-autoload -o` of a project's own autoload rules
//!   (laravel/framework by default).
//! - `KOMPOSER_AUTOLOAD_E2E=<dir>`: `<dir>/app` is a project installed by Composer 2.10.3 and
//!   `<dir>/ref/<mode>/` holds the `vendor/autoload.php` and `vendor/composer/` files Composer
//!   generated for it with `dump-autoload --no-scripts` and: `optimized` (the defaults with
//!   `optimize-autoloader`), `optimized-no-dev` (`--no-dev`), `authoritative`
//!   (`--classmap-authoritative`), `apcu` (`--apcu-prefix=komposer`), `plain` (`COMPOSER=composer-plain.json`,
//!   a copy of composer.json with `optimize-autoloader: false` and `platform-check: true`).
//!   Only the modes present in `ref/` are checked.

use komposer_autoload::{AutoloadOptions, dump};
use komposer_package::config::Config;
use komposer_package::io::Io;
use komposer_package::{PackageRef, json, loader};
use std::path::{Path, PathBuf};
use std::time::Instant;

#[test]
#[ignore]
fn matches_composer_output() {
    let dir = PathBuf::from(std::env::var("KOMPOSER_AUTOLOAD_E2E").unwrap_or("/Volumes/T9/tmp/komposer-autoload-e2e".into()));
    let project = dir.join("app");
    let vendor = project.join("vendor");
    let installed = json::read(&vendor.join("composer/installed.json")).unwrap_or(serde_json::json!({"packages": [], "dev-package-names": []}));
    let entries = installed["packages"].as_array().unwrap();
    let packages: Vec<(PackageRef, Option<PathBuf>)> = loader::load_packages(entries)
        .unwrap()
        .into_iter()
        .filter(|p| !p.is_alias())
        .map(|p| {
            let entry = entries.iter().find(|e| e["name"] == p.pretty_name.as_str()).unwrap();
            let path = entry["install-path"].as_str().map(|rel| vendor.join("composer").join(rel).canonicalize().unwrap());
            (p, path)
        })
        .collect();
    let dev_package_names: Vec<String> = installed["dev-package-names"].as_array().unwrap().iter().map(|n| n.as_str().unwrap().to_string()).collect();
    let lock = json::read(&project.join("composer.lock")).ok();

    let mut failures = vec![];
    let mut modes: Vec<String> = std::fs::read_dir(dir.join("ref")).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    modes.sort();
    for mode in modes.iter().map(String::as_str) {
        let raw = json::read(&project.join(if mode == "plain" { "composer-plain.json" } else { "composer.json" })).unwrap();
        let root = loader::load_root(raw.as_object().unwrap(), || None).unwrap();
        let mut config = Config::new(false, Some(&project));
        config.merge(raw.as_object().unwrap());
        let opts = AutoloadOptions {
            dev_mode: (mode == "optimized-no-dev").then_some(false),
            optimize: config.get_bool("optimize-autoloader"),
            classmap_authoritative: mode == "authoritative",
            apcu: mode == "apcu",
            apcu_prefix: (mode == "apcu").then(|| "komposer".into()),
            lock_content_hash: lock.as_ref().and_then(|l| l["content-hash"].as_str()).map(String::from),
            dev_package_names: dev_package_names.clone(),
            ..Default::default()
        };
        let start = Instant::now();
        let class_map = dump(&config, &root, &packages, &project, &opts, &Io::null()).unwrap();
        println!("{mode}: {} classes in {:?}", class_map.len(), start.elapsed());
        let reference = dir.join("ref").join(mode);
        compare_dir(&reference, &reference, &vendor, &mut failures);
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

fn compare_dir(root: &Path, dir: &Path, vendor: &Path, failures: &mut Vec<String>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            compare_dir(root, &path, vendor, failures);
            continue;
        }
        let rel = path.strip_prefix(root).unwrap();
        let (expected, actual) = (std::fs::read_to_string(&path).unwrap(), std::fs::read_to_string(vendor.join(rel)).unwrap_or_default());
        if expected != actual {
            let line = expected.lines().zip(actual.lines()).position(|(e, a)| e != a).unwrap_or(expected.lines().count().min(actual.lines().count()));
            failures.push(format!(
                "{}: first difference at line {}:\n  composer: {:?}\n  komposer: {:?}",
                path.display(),
                line + 1,
                expected.lines().nth(line),
                actual.lines().nth(line)
            ));
        }
    }
}

#[test]
#[ignore]
fn optimized_dump_of_real_tree() {
    let project = PathBuf::from(std::env::var("KOMPOSER_AUTOLOAD_SAMPLE").unwrap_or("/Volumes/T9/tmp/komposer-autoload-sample/framework".into()));
    let raw = komposer_package::json::read(&project.join("composer.json")).unwrap();
    let root = komposer_package::loader::load_root(raw.as_object().unwrap(), || None).unwrap();
    let vendor = std::env::temp_dir().join(format!("komposer-autoload-real-tree-{}", std::process::id()));
    let mut config = Config::new(false, Some(&project));
    config.merge_config(&serde_json::json!({"vendor-dir": vendor.to_string_lossy()}));
    let opts = AutoloadOptions { optimize: true, dev_mode: Some(false), suffix: Some("Sample".into()), ..Default::default() };

    let start = Instant::now();
    let class_map = dump(&config, &root, &[], &project, &opts, &Io::null()).unwrap();
    let elapsed = start.elapsed();
    println!("{} classes in {elapsed:?}, {} PSR violations", class_map.len(), class_map.psr_violations().len());
    for violation in class_map.psr_violations() {
        println!("  {violation}");
    }

    if project.ends_with("framework") {
        let src = project.join("src/Illuminate").canonicalize().unwrap();
        for (class, file) in [
            ("Illuminate\\Foundation\\Application", "Foundation/Application.php"),
            ("Illuminate\\Support\\Collection", "Collections/Collection.php"),
            ("Illuminate\\Support\\Traits\\Macroable", "Macroable/Traits/Macroable.php"),
            ("Illuminate\\Contracts\\Container\\Container", "Contracts/Container/Container.php"),
            ("Illuminate\\Database\\Eloquent\\Model", "Database/Eloquent/Model.php"),
        ] {
            assert_eq!(class_map.class_path(class), Some(src.join(file).to_str().unwrap()), "{class}");
        }
        assert!(class_map.len() > 1000);
        assert!(class_map.psr_violations().is_empty());
    }
    std::fs::remove_dir_all(vendor).unwrap();
}
