//! End-to-end check against a real Composer 2.10.3 run: `composer create-project laravel/laravel`, whose
//! Packagist metadata cache, composer.lock and log live in `KOMPOSER_ORACLE` (default
//! `/Volumes/T9/tmp/komposer-oracle`). Skipped when that directory is missing.
//!
//! Replays `Installer::doUpdate()` (full update with dev requirements, then the no-dev extraction) over
//! the cached metadata (~28k versions) and compares the lock file packages and the install order.

use anyhow::Result;
use komposer_package::io::Io;
use komposer_package::loader::{LoadOptions, branch_alias, load, load_root};
use komposer_package::operation::Operation;
use komposer_package::repository::{
    AdvisoryResult, ArrayRepository, LoadRequest, LoadResult, RepoKind, Repository, SecurityAdvisory, is_package_acceptable,
};
use komposer_package::{PackageRef, is_platform_package};
use komposer_semver::{Constraint, parse_constraints, parse_stability};
use komposer_solver::{DefaultPolicy, PlatformRequirementFilter, PolicyOptions, PoolOptions, RepositorySet, Request, Solver, transaction_operations};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// A cached `p2/*.json` file: the expanded versions and the partial advisories.
type CachedFile = Arc<(Vec<Map<String, Value>>, Vec<Value>)>;

/// Packagist's `p2/*.json` metadata read from Composer's cache (`ComposerRepository::loadAsyncPackages()`).
struct PackagistCache {
    dir: PathBuf,
    files: Mutex<HashMap<String, Option<CachedFile>>>,
}

impl PackagistCache {
    fn file(&self, name: &str) -> Option<CachedFile> {
        let mut files = self.files.lock().unwrap();
        files
            .entry(name.to_string())
            .or_insert_with(|| {
                let path = self.dir.join(format!("provider-{}.json", name.replace('/', "~")));
                let data: Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
                let mut expanded: Vec<Map<String, Value>> = vec![];
                // MetadataMinifier::expand()
                for version in data["packages"][name].as_array()? {
                    let mut current = expanded.last().cloned().unwrap_or_default();
                    for (k, v) in version.as_object()? {
                        if v == "__unset" {
                            current.remove(k);
                        } else {
                            current.insert(k.clone(), v.clone());
                        }
                    }
                    expanded.push(current);
                }
                let advisories = data["security-advisories"].as_array().cloned().unwrap_or_default();
                Some(Arc::new((expanded, advisories)))
            })
            .clone()
    }
}

impl Repository for PackagistCache {
    fn repo_name(&self) -> String {
        "composer repo (https://repo.packagist.org)".into()
    }

    fn load_packages(&self, req: &LoadRequest) -> Result<LoadResult> {
        let mut result = LoadResult::default();
        for (name, constraint) in req.names {
            if is_platform_package(name) || name == "__root__" {
                continue;
            }
            let Some(file) = self.file(name) else { continue };
            result.names_found.push(name.clone());
            for version in &file.0 {
                let normalized = version["version_normalized"].as_str().unwrap();
                if req.already_loaded.get(name).is_some_and(|v| v.contains(normalized)) {
                    continue;
                }
                // isVersionAcceptable(): the version or its branch alias
                let mut versions = vec![normalized.to_string()];
                versions.extend(branch_alias(version)?);
                let acceptable = versions.iter().any(|v| {
                    is_package_acceptable(req.acceptable_stabilities, req.stability_flags, std::slice::from_ref(name), parse_stability(v))
                        && constraint.as_ref().is_none_or(|c| c.matches_version(v))
                });
                if acceptable {
                    let mut loaded = load(version, LoadOptions { skip_self_links: true, ..LoadOptions::default() })?;
                    // Composer registers a branch alias before the package it aliases
                    loaded.reverse();
                    result.packages.extend(loaded);
                }
            }
        }
        Ok(result)
    }

    fn packages(&self) -> Result<Vec<PackageRef>> {
        Ok(vec![])
    }

    fn has_security_advisories(&self) -> bool {
        true
    }

    fn security_advisories(&self, constraints: &[(String, Constraint)], _allow_partial: bool) -> Result<AdvisoryResult> {
        let mut result = AdvisoryResult::default();
        for (name, constraint) in constraints {
            let Some(file) = self.file(name) else { continue };
            result.names_found.push(name.clone());
            for a in &file.1 {
                let affected = parse_constraints(a["affectedVersions"].as_str().unwrap())?;
                if affected.matches(constraint) {
                    result.advisories.entry(name.clone()).or_default().push(SecurityAdvisory {
                        package_name: name.clone(),
                        advisory_id: a["advisoryId"].as_str().unwrap().into(),
                        affected_versions_pretty: a["affectedVersions"].as_str().unwrap().into(),
                        affected_versions: affected,
                        title: None,
                        cve: None,
                        link: None,
                        severity: None,
                        reported_at: None,
                        sources: vec![],
                    });
                }
            }
        }
        Ok(result)
    }
}

/// The PHP of the `composer:2.10.3` docker image.
fn platform() -> ArrayRepository {
    let mut repo = ArrayRepository::new("platform repo", RepoKind::Platform);
    let php = "8.4.15";
    let mut add = |name: &str, version: &str| {
        let data = serde_json::json!({"name": name, "version": version});
        repo.add(load(data.as_object().unwrap(), LoadOptions::default()).unwrap().remove(0));
    };
    add("php", php);
    add("php-64bit", php);
    for ext in [
        "bz2",
        "ctype",
        "curl",
        "date",
        "dom",
        "fileinfo",
        "filter",
        "hash",
        "iconv",
        "json",
        "libxml",
        "mbstring",
        "openssl",
        "pcre",
        "pdo",
        "pdo_sqlite",
        "phar",
        "posix",
        "readline",
        "reflection",
        "session",
        "simplexml",
        "sodium",
        "spl",
        "sqlite3",
        "standard",
        "tokenizer",
        "xml",
        "xmlreader",
        "xmlwriter",
        "zip",
        "zlib",
    ] {
        add(&format!("ext-{ext}"), php);
    }
    add("composer", "2.10.3");
    add("composer-plugin-api", "2.9.0");
    add("composer-runtime-api", "2.2.2");
    repo
}

// the cached metadata moves on as Packagist publishes releases, so run it right after refreshing the oracle
#[test]
#[ignore = "needs a fresh Composer oracle snapshot (KOMPOSER_ORACLE)"]
fn laravel_create_project_matches_composer() -> Result<()> {
    let oracle = PathBuf::from(std::env::var("KOMPOSER_ORACLE").unwrap_or_else(|_| "/Volumes/T9/tmp/komposer-oracle".into()));
    if !oracle.join("laravel-app/composer.lock").exists() {
        eprintln!("skipped: no oracle at {}", oracle.display());
        return Ok(());
    }
    let raw: Value = serde_json::from_str(&std::fs::read_to_string(oracle.join("laravel-app/composer.json"))?)?;
    let root = load_root(raw.as_object().unwrap(), || None)?;
    let cache = Arc::new(PackagistCache { dir: oracle.join("cache/repo/https---repo.packagist.org"), files: Mutex::new(HashMap::new()) });

    let started = Instant::now();
    let mut fixed_root = root.package.clone();
    fixed_root.requires.clear();
    fixed_root.dev_requires.clear();
    let mut root_repo = ArrayRepository::new("root package repo", RepoKind::Root);
    let fixed_root = root_repo.add_package(fixed_root);
    let root_repo: Arc<dyn Repository> = Arc::new(root_repo);
    let platform = platform();
    let platform_packages = platform.all().to_vec();
    let platform: Arc<dyn Repository> = Arc::new(platform);

    // Installer::createRepositorySet()
    let links: Vec<_> = root.package.requires.iter().chain(&root.package.dev_requires).collect();
    let root_requires: indexmap::IndexMap<_, _> =
        links.iter().map(|l| (l.target.clone(), komposer_solver::PrettyConstraint::new(l.constraint.clone(), &l.pretty_constraint))).collect();
    let mut flags: HashMap<String, _> = root.stability_flags.iter().cloned().collect();
    flags.insert(root.package.name.clone(), root.package.stability);
    let repository_set = |extra: Option<Arc<dyn Repository>>| {
        let mut set = RepositorySet::new(root.minimum_stability)
            .with_stability_flags(flags.clone())
            .with_root_aliases(&root.aliases)
            .with_root_references(root.references.iter().cloned().collect())
            .with_root_requires(root_requires.clone());
        set.add_repository(root_repo.clone());
        set.add_repository(platform.clone());
        set.add_repository(extra.unwrap_or_else(|| cache.clone()));
        set
    };
    // Installer::createRequest() + requirePackagesForUpdate()
    let request = |dev: bool| {
        let mut request = Request::new();
        request.fix_package(fixed_root.clone());
        for p in &platform_packages {
            request.fix_package(p.clone());
        }
        for link in root.package.requires.iter().chain(root.package.dev_requires.iter().filter(|_| dev)) {
            let constraint = komposer_solver::PrettyConstraint::new(link.constraint.clone(), &link.pretty_constraint);
            request.require_name(&link.target, Some(constraint)).unwrap();
        }
        request
    };
    let policy = DefaultPolicy::new(root.prefer_stable, false, None);
    let io = Io::null();

    let set = repository_set(None);
    let mut update = request(true);
    let configured: Vec<Arc<dyn Repository>> = vec![cache.clone()];
    let pool = set.create_pool(&mut update, &io, &PoolOptions::for_update(&policy, &PolicyOptions::default(), false, &configured))?;
    let pool_size = pool.len();
    let pool_built = started.elapsed();
    let mut solver = Solver::new(&policy, &pool, &io);
    let mut lock = solver.solve(&update, &PlatformRequirementFilter::IgnoreNothing)?;
    let rules = solver.rule_set_size();
    let solved = started.elapsed() - pool_built;

    // Installer::extractDevPackages()
    let mut result_repo = ArrayRepository::new("result", RepoKind::Remote);
    for p in lock.new_lock_packages(false, false) {
        let dumped = komposer_package::dumper::dump(&p);
        for loaded in load(&dumped, LoadOptions { transport_options: true, ..LoadOptions::default() })? {
            result_repo.add(loaded);
        }
    }
    let set = repository_set(Some(Arc::new(result_repo)));
    let pool = set.create_pool_with_all_packages()?;
    let non_dev = Solver::new(&policy, &pool, &io).solve(&request(false), &PlatformRequirementFilter::IgnoreNothing)?;
    lock.set_non_dev_packages(&non_dev);
    let elapsed = started.elapsed();
    eprintln!(
        "laravel update: {pool_size} packages in pool ({pool_built:?} incl. metadata parsing), {rules} rules, solved in {solved:?}, {elapsed:?} in total"
    );

    let expected: Value = serde_json::from_str(&std::fs::read_to_string(oracle.join("laravel-app/composer.lock"))?)?;
    let versions = |key: &str| -> Vec<String> {
        let mut list: Vec<String> =
            expected[key].as_array().unwrap().iter().map(|p| format!("{} {}", p["name"].as_str().unwrap(), p["version"].as_str().unwrap())).collect();
        list.sort();
        list
    };
    let actual = |dev: bool| -> Vec<String> {
        let mut list: Vec<String> = lock.new_lock_packages(dev, false).iter().map(|p| format!("{} {}", p.pretty_name, p.pretty_version)).collect();
        list.sort();
        list
    };
    assert_eq!(actual(false), versions("packages"));
    assert_eq!(actual(true), versions("packages-dev"));

    // `install` from the new lock file into an empty vendor dir: the order Composer installed in
    let log = std::fs::read_to_string(oracle.join("create-project.log"))?;
    let install_section = log.split("Installing dependencies from lock file").nth(1).unwrap();
    let expected_installs: Vec<&str> =
        install_section.lines().filter_map(|l| l.strip_prefix("  - Installing ").and_then(|l| l.strip_suffix(": Extracting archive"))).collect();
    let locked: Vec<PackageRef> = lock.new_lock_packages(false, false).into_iter().chain(lock.new_lock_packages(true, false)).collect();
    let installs: Vec<String> = transaction_operations(&[], &locked)
        .iter()
        .map(|op| match op {
            Operation::Install(p) => format!("{} ({})", p.pretty_name, p.full_pretty_version()),
            op => panic!("unexpected {}", op.kind()),
        })
        .collect();
    assert_eq!(installs, expected_installs);
    Ok(())
}
