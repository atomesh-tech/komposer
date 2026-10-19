//! Ports of Composer's `SecurityAdvisoryPoolFilterTest.php` and `FilterListPoolFilterTest.php` (2.10.3
//! behavior), plus `PolicyOptions::from_config()`.

mod common;

use common::*;
use komposer_package::config::Config;
use komposer_package::io::Io;
use komposer_package::repository::{ArrayRepository, RepoKind, Repository};
use komposer_package::{Package, PackageRef};
use komposer_semver::{Constraint, Op};
use komposer_solver::{BlockScope, PolicyOptions, Pool, PrettyConstraint, Request, filter_list_filter, security_advisory_filter};
use serde_json::json;
use std::sync::Arc;

fn arc(p: Package) -> PackageRef {
    Arc::new(p)
}

fn eq(version: &str) -> Constraint {
    Constraint::single(Op::Eq, version)
}

fn names(pool: &Pool) -> Vec<String> {
    pool.packages().iter().map(|p| p.to_string()).collect()
}

fn advisories_filter(packages: Vec<PackageRef>, repo: PolicyRepo, policy: &PolicyOptions) -> Pool {
    let repos: Vec<Arc<dyn Repository>> = vec![Arc::new(repo)];
    security_advisory_filter(Pool::new(packages), &repos, &Request::new(), policy, &Io::null()).unwrap()
}

fn blocking_abandoned() -> PolicyOptions {
    PolicyOptions { abandoned_block: true, ..PolicyOptions::default() }
}

#[test]
fn filter_packages_by_advisories() {
    let repo = PolicyRepo {
        advisories: vec![
            advisory("acme/package", "PKSA-1", Some("CVE-1999-1000"), ">=1.0.0,<1.1.0"),
            advisory("acme/package", "PKSA-2", Some("CVE-1999-1001"), ">=1.0.0,<1.1.0"),
        ],
        ..PolicyRepo::default()
    };
    let packages = vec![arc(pkg("acme/package", "1.0")), arc(pkg("acme/package", "2.0")), arc(pkg("acme/other", "1.0"))];
    let pool = advisories_filter(packages, repo, &blocking_abandoned());
    assert_eq!(names(&pool), ["acme/package-2.0.0.0", "acme/other-1.0.0.0"]);
    assert!(pool.is_security_removed("acme/package", &eq("1.0.0.0")));
    assert_eq!(pool.security_advisory_ids("acme/package", &eq("1.0.0.0")), ["PKSA-1", "PKSA-2"]);
    assert!(!pool.is_abandoned_removed("acme/package", &Constraint::Any));
}

#[test]
fn dont_filter_packages_by_ignored_advisories() {
    let repo = PolicyRepo { advisories: vec![advisory("acme/package", "PKSA-1", Some("CVE-2024-1234"), ">=1.0.0,<1.1.0")], ..PolicyRepo::default() };
    let policy = PolicyOptions { advisories_ignore: vec!["CVE-2024-1234".into()], ..blocking_abandoned() };
    let pool = advisories_filter(vec![arc(pkg("acme/package", "1.0")), arc(pkg("acme/package", "1.1"))], repo, &policy);
    assert_eq!(pool.len(), 2);
    assert!(!pool.is_security_removed("acme/package", &Constraint::Any));
}

#[test]
fn dont_filter_packages_with_block_insecure_disabled() {
    let repo = PolicyRepo { advisories: vec![advisory("acme/package", "PKSA-1", Some("CVE-2024-1234"), ">=1.0.0,<1.1.0")], ..PolicyRepo::default() };
    let policy = PolicyOptions { advisories_block: false, ..blocking_abandoned() };
    let pool = advisories_filter(vec![arc(pkg("acme/package", "1.0")), arc(pkg("acme/package", "1.1"))], repo, &policy);
    assert_eq!(pool.len(), 2);
}

#[test]
fn dont_filter_packages_with_abandoned_package() {
    let abandoned = |name: &str| {
        let mut p = pkg(name, "1.0");
        p.meta.insert("abandoned".into(), json!(true));
        arc(p)
    };
    let expected = arc(pkg("acme/other", "1.1"));
    let ignored = abandoned("acme/ignore-abandoned");
    let policy = PolicyOptions { abandoned_ignore: vec!["acme/ignore-abandoned".into()], ..blocking_abandoned() };
    let pool = advisories_filter(vec![expected.clone(), abandoned("acme/package"), ignored.clone()], PolicyRepo::default(), &policy);
    assert_eq!(names(&pool), [expected.to_string(), ignored.to_string()]);
    assert!(pool.is_abandoned_removed("acme/package", &eq("1.0.0.0")));
    assert!(!pool.is_security_removed("acme/package", &Constraint::Any));
}

#[test]
fn unreachable_advisory_repositories_are_ignored_unless_configured_not_to() {
    let failing = || PolicyRepo { failing: Some("HTTP/1.1 502 Bad Gateway".into()), ..PolicyRepo::default() };
    let pool = advisories_filter(vec![arc(pkg("acme/package", "1.0"))], failing(), &PolicyOptions::default());
    assert_eq!(pool.len(), 1);

    let strict = PolicyOptions { ignore_unreachable_update: false, ..PolicyOptions::default() };
    let repos: Vec<Arc<dyn Repository>> = vec![Arc::new(failing())];
    let err = security_advisory_filter(Pool::new(vec![arc(pkg("acme/package", "1.0"))]), &repos, &Request::new(), &strict, &Io::null());
    assert!(err.err().unwrap().to_string().contains("502"));
}

/// Composer 2.10.3 never matches dev versions, numeric branches included.
#[test]
fn dev_versions_are_not_filtered_by_advisories() {
    let repo = PolicyRepo { advisories: vec![advisory("acme/package", "PKSA-1", Some("CVE-2024-1234"), "<3.5")], ..PolicyRepo::default() };
    let packages = vec![arc(pkg("acme/package", "dev-main")), arc(pkg("acme/package", "3.3.x-dev")), arc(pkg("acme/package", "3.4.0"))];
    let pool = advisories_filter(packages, repo, &PolicyOptions::default());
    assert_eq!(names(&pool), ["acme/package-dev-main", "acme/package-3.3.9999999.9999999-dev"]);
}

/// Root aliases are left out of the advisory lookup, but their version is matched (2.10.3).
#[test]
fn root_alias_versions_are_matched_against_advisories() {
    let repo = PolicyRepo { advisories: vec![advisory("acme/package", "PKSA-1", None, "*")], ..PolicyRepo::default() };
    let dev = arc(pkg("acme/package", "dev-main"));
    let mut root_alias = alias(&dev, "1.0.0");
    root_alias.root_package_alias = true;
    let requested = Arc::new(repo);
    let repos: Vec<Arc<dyn Repository>> = vec![requested.clone()];
    let pool = Pool::new(vec![dev.clone(), arc(root_alias)]);
    let pool = security_advisory_filter(pool, &repos, &Request::new(), &PolicyOptions::default(), &Io::null()).unwrap();
    assert_eq!(names(&pool), [dev.to_string()]);
    assert_eq!(*requested.requested.lock().unwrap(), ["acme/package"]);
}

fn malware_filter(pool: Pool, repo: PolicyRepo, request: &Request, policy: &PolicyOptions, scope: BlockScope) -> Pool {
    let repos: Vec<Arc<dyn Repository>> = vec![Arc::new(repo)];
    filter_list_filter(pool, &repos, request, policy, scope, &Io::null()).unwrap()
}

fn malware_repo(constraint: &str) -> PolicyRepo {
    PolicyRepo { filter: vec![malware("acme/package", constraint)], ..PolicyRepo::default() }
}

#[test]
fn filter_list_filters_packages() {
    let pool = Pool::new(vec![arc(pkg("acme/package", "1.0")), arc(pkg("acme/package", "2.0")), arc(pkg("acme/other", "1.0"))]);
    let pool = malware_filter(pool, malware_repo("1.0"), &Request::new(), &PolicyOptions::default(), BlockScope::Update);
    assert_eq!(names(&pool), ["acme/package-2.0.0.0", "acme/other-1.0.0.0"]);
    assert!(pool.is_filter_list_removed("acme/package", &eq("1.0.0.0")));
    assert_eq!(
        pool.filter_list_entry_descriptions("acme/package", &eq("1.0.0.0"))["malware"],
        "flagged as malware (see https://example.org/malware/acme/package) reason: malware"
    );
}

#[test]
fn filter_list_ignores_configured_packages() {
    for ignore in [json!(["acme/package"]), json!({"acme/package": {"constraint": "*"}})] {
        let mut config = Config::new(false, None);
        config.merge_config(&json!({"policy": {"malware": {"ignore": ignore}}}));
        let policy = PolicyOptions::from_config(&config).unwrap();
        let pool = Pool::new(vec![arc(pkg("acme/package", "1.0")), arc(pkg("acme/package", "1.1"))]);
        let pool = malware_filter(pool, malware_repo("*"), &Request::new(), &policy, BlockScope::Update);
        assert_eq!(pool.len(), 2);
    }
}

#[test]
fn filter_list_ignore_constraint_intersection() {
    let mut config = Config::new(false, None);
    config.merge_config(&json!({"policy": {"malware": {"ignore": {"acme/package": {"constraint": "<=1.0"}}}}}));
    let policy = PolicyOptions::from_config(&config).unwrap();
    let pool = Pool::new(vec![arc(pkg("acme/package", "1.0")), arc(pkg("acme/package", "1.1"))]);
    let pool = malware_filter(pool, malware_repo(">=1.0"), &Request::new(), &policy, BlockScope::Update);
    assert_eq!(names(&pool), ["acme/package-1.0.0.0"]);
}

#[test]
fn install_scope_filters_locked_packages_against_malware_list() {
    let repo = || PolicyRepo { filter: vec![malware("acme/locked", "*")], ..PolicyRepo::default() };
    let package = arc(pkg("acme/locked", "1.0"));
    let mut request = Request::new();
    request.fix_locked_package(package.clone());
    for scope in [BlockScope::Install, BlockScope::Update] {
        let pool = malware_filter(Pool::new(vec![package.clone()]), repo(), &request, &PolicyOptions::default(), scope);
        assert!(pool.is_empty());
        assert!(pool.is_filter_list_removed("acme/locked", &eq("1.0.0.0")));
    }
}

#[test]
fn update_scope_applies_install_scope_to_packages_in_locked_repository() {
    let repo = PolicyRepo { filter: vec![malware("acme/mirrored", "*")], ..PolicyRepo::default() };
    let policy = PolicyOptions { malware_block_scope: Some(BlockScope::Install), ..PolicyOptions::default() };
    // update mirrors loads fresh instances: same name and version as the locked package, not the same object
    let mut locked = ArrayRepository::new("lock", RepoKind::Lock);
    locked.add_package(pkg("acme/mirrored", "1.0"));
    let mut request = Request::with_locked_repository(Arc::new(locked)).unwrap();
    request.require_name("acme/mirrored", Some(PrettyConstraint::new(eq("1.0.0.0"), "1.0.0.0"))).unwrap();
    let pool = malware_filter(Pool::new(vec![arc(pkg("acme/mirrored", "1.0"))]), repo, &request, &policy, BlockScope::Update);
    assert!(pool.is_empty());
}

#[test]
fn update_scope_ignores_install_only_lists_for_non_locked_packages() {
    let repo = PolicyRepo { filter: vec![malware("acme/free", "*")], ..PolicyRepo::default() };
    let policy = PolicyOptions { malware_block_scope: Some(BlockScope::Install), ..PolicyOptions::default() };
    let pool = malware_filter(Pool::new(vec![arc(pkg("acme/free", "1.0"))]), repo, &Request::new(), &policy, BlockScope::Update);
    assert_eq!(pool.len(), 1);
}

#[test]
fn unreachable_filter_sources() {
    let failing = || PolicyRepo { failing: Some("HTTP/1.1 500 Internal Server Error".into()), ..PolicyRepo::default() };
    let pool =
        malware_filter(Pool::new(vec![arc(pkg("acme/package", "1.0"))]), failing(), &Request::new(), &PolicyOptions::default(), BlockScope::Update);
    assert_eq!(pool.len(), 1);

    let mut config = Config::new(false, None);
    config.merge_config(&json!({"policy": {"ignore-unreachable": false}}));
    let strict = PolicyOptions::from_config(&config).unwrap();
    let repos: Vec<Arc<dyn Repository>> = vec![Arc::new(failing())];
    let result =
        filter_list_filter(Pool::new(vec![arc(pkg("acme/package", "1.0"))]), &repos, &Request::new(), &strict, BlockScope::Update, &Io::null());
    assert!(result.is_err());
}

fn options(config: serde_json::Value) -> PolicyOptions {
    let mut c = Config::new(false, None);
    c.merge_config(&config);
    PolicyOptions::from_config(&c).unwrap()
}

#[test]
fn policy_options_from_config() {
    let defaults = options(json!({}));
    assert!(defaults.enabled && defaults.advisories_block && defaults.malware_block && !defaults.abandoned_block);
    assert!(defaults.ignore_unreachable_update && defaults.ignore_unreachable_install);

    let off = options(json!({"policy": false}));
    assert!(!off.enabled && !off.advisories_block && !off.malware_block);

    let legacy = options(json!({"audit": {
        "block-insecure": false,
        "block-abandoned": true,
        "ignore": {"CVE-1": "reason", "PKSA-2": {"apply": "audit"}, "vendor/pkg": {"apply": "block"}},
        "ignore-abandoned": ["old/*"],
        "ignore-unreachable": true
    }}));
    assert!(!legacy.advisories_block && legacy.abandoned_block);
    assert_eq!(legacy.advisories_ignore, ["CVE-1", "vendor/pkg"]);
    assert_eq!(legacy.abandoned_ignore, ["old/*"]);
    assert!(!legacy.ignore_unreachable_update && !legacy.ignore_unreachable_install);

    let policy = options(json!({"policy": {
        "advisories": {"ignore-id": ["GHSA-1"], "ignore": {"a/b": {"on-block": false}, "c/d": null}},
        "abandoned": {"block": true, "ignore": ["e/f"]},
        "malware": {"block-scope": "install", "ignore": {"g/h": [{"constraint": "^1.0"}]}},
        "ignore-unreachable": ["install"]
    }}));
    assert!(policy.advisories_block && policy.abandoned_block);
    assert_eq!(policy.advisories_ignore, ["GHSA-1", "c/d"]);
    assert_eq!(policy.abandoned_ignore, ["e/f"]);
    assert_eq!(policy.malware_block_scope, Some(BlockScope::Install));
    assert!(policy.blocks_malware(BlockScope::Install) && !policy.blocks_malware(BlockScope::Update));
    assert_eq!(policy.malware_ignore.len(), 1);
    assert!(policy.ignore_unreachable_install && !policy.ignore_unreachable_update);

    assert!(!options(json!({"policy": {"advisories": false, "malware": false}})).advisories_block);
    assert!(!PolicyOptions::default().without_blocking().malware_block);
}
