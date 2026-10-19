//! Port of Composer's `DependencyResolver/DefaultPolicyTest.php`.

mod common;

use common::*;
use komposer_package::repository::{ArrayRepository, RepoKind, Repository};
use komposer_package::{LinkType, Package, PackageRef};
use komposer_semver::{Constraint, Op, Stability};
use komposer_solver::{DefaultPolicy, Literal, Pool, PrettyConstraint, RepositorySet};
use std::collections::HashMap;
use std::sync::Arc;

fn pool_for(repos: Vec<ArrayRepository>, names: &[&str]) -> Pool {
    let mut set = RepositorySet::new(Stability::Dev);
    for r in repos {
        set.add_repository(Arc::new(r));
    }
    let locked: Arc<dyn Repository> = Arc::new(repo(RepoKind::Lock));
    set.create_pool_for_packages(names, Some(locked)).unwrap()
}

fn ids(pool: &Pool, packages: &[&PackageRef]) -> Vec<Literal> {
    packages.iter().map(|p| pool.id_of(p).expect("package in pool")).collect()
}

fn select(policy: &DefaultPolicy, repo: ArrayRepository, literals: &[&PackageRef], expected: &[&PackageRef]) {
    let pool = pool_for(vec![repo], &["A"]);
    assert_eq!(policy.select_preferred_packages(&pool, &ids(&pool, literals), None), ids(&pool, expected));
}

fn repo_with(versions: &[&str]) -> (ArrayRepository, Vec<PackageRef>) {
    let mut r = repo(RepoKind::Remote);
    let packages = versions.iter().map(|v| r.add_package(pkg("A", v))).collect();
    (r, packages)
}

fn lowest_prefer_dev(prefer_dev_over_prerelease: bool) -> DefaultPolicy {
    DefaultPolicy { prefer_dev_over_prerelease, ..DefaultPolicy::new(true, true, None) }
}

#[test]
fn select_single() {
    let (r, p) = repo_with(&["1.0"]);
    select(&DefaultPolicy::default(), r, &[&p[0]], &[&p[0]]);
}

#[test]
fn select_newest() {
    let (r, p) = repo_with(&["1.0", "2.0"]);
    select(&DefaultPolicy::default(), r, &[&p[0], &p[1]], &[&p[1]]);
}

#[test]
fn select_newest_picks_latest() {
    let (r, p) = repo_with(&["1.0.0", "1.0.1-alpha"]);
    select(&DefaultPolicy::default(), r, &[&p[0], &p[1]], &[&p[1]]);
}

#[test]
fn select_newest_picks_latest_stable_with_prefer_stable() {
    let (r, p) = repo_with(&["1.0.0", "1.0.1-alpha"]);
    select(&DefaultPolicy::new(true, false, None), r, &[&p[0], &p[1]], &[&p[0]]);
}

#[test]
fn select_lowest_with_prefer_dev_over_prerelease() {
    for stability in ["alpha1", "beta1", "RC1"] {
        let (r, p) = repo_with(&["dev-master", &format!("1.0.0-{stability}")]);
        select(&lowest_prefer_dev(true), r, &[&p[0], &p[1]], &[&p[0]]);
    }
}

#[test]
fn select_lowest_prefers_prerelease_over_dev() {
    for stability in ["alpha1", "beta1", "RC1"] {
        let (r, p) = repo_with(&["dev-master", &format!("1.0.0-{stability}")]);
        select(&lowest_prefer_dev(false), r, &[&p[1], &p[0]], &[&p[1]]);
    }
}

#[test]
fn select_lowest_with_prefer_stable_still_prefers_stable() {
    let (r, p) = repo_with(&["1.0.0", "dev-master"]);
    select(&lowest_prefer_dev(true), r, &[&p[0], &p[1]], &[&p[0]]);
}

#[test]
fn select_newest_with_dev_picks_non_dev() {
    let (r, p) = repo_with(&["dev-foo", "1.0.0"]);
    select(&DefaultPolicy::default(), r, &[&p[0], &p[1]], &[&p[1]]);
}

fn preferred(prefer_lowest: bool) -> DefaultPolicy {
    DefaultPolicy::new(false, prefer_lowest, Some(HashMap::from([("a".to_string(), "1.1.0.0".to_string())])))
}

#[test]
fn select_newest_with_preferred_version_picks_preferred_version_if_available() {
    let (r, p) = repo_with(&["1.0.0", "1.1.0", "1.1.0", "1.2.0"]);
    select(&preferred(false), r, &[&p[0], &p[1], &p[2], &p[3]], &[&p[1], &p[2]]);
}

#[test]
fn select_newest_with_preferred_version_picks_newest_otherwise() {
    let (r, p) = repo_with(&["1.0.0", "1.2.0"]);
    select(&preferred(false), r, &[&p[0], &p[1]], &[&p[1]]);
}

#[test]
fn select_newest_with_preferred_version_picks_lowest_if_prefer_lowest() {
    let (r, p) = repo_with(&["1.0.0", "1.2.0"]);
    select(&preferred(true), r, &[&p[0], &p[1]], &[&p[0]]);
}

#[test]
fn repository_ordering_affects_priority() {
    let mut repo1 = repo(RepoKind::Remote);
    let mut repo2 = repo(RepoKind::Remote);
    let p1 = repo1.add_package(pkg("A", "1.0"));
    let p2 = repo1.add_package(pkg("A", "1.1"));
    let p3 = repo2.add_package(pkg("A", "1.1"));
    let p4 = repo2.add_package(pkg("A", "1.2"));
    let (r1, r2) = (Arc::new(repo1), Arc::new(repo2));
    let policy = DefaultPolicy::default();

    for (order, expected) in [([r1.clone(), r2.clone()], &p2), ([r2, r1], &p4)] {
        let mut set = RepositorySet::new(Stability::Dev);
        for r in order {
            set.add_repository(r);
        }
        // the first repository having the name is the only one loaded (Composer's test passes stale ids for the others)
        let pool = set.create_pool_for_package("A", Some(Arc::new(repo(RepoKind::Lock)))).unwrap();
        let literals: Vec<Literal> = [&p1, &p2, &p3, &p4].iter().filter_map(|p| pool.id_of(p)).collect();
        assert_eq!(literals.len(), 2);
        assert_eq!(policy.select_preferred_packages(&pool, &literals, None), ids(&pool, &[expected]));
    }
}

#[test]
fn select_local_repos_first() {
    let branch_alias = |of: &PackageRef| Package::alias(of, "2.1.9999999.9999999-dev", "2.1.x-dev");
    let mut r = repo(RepoKind::Remote);
    let a = r.add_package(pkg("A", "dev-master"));
    r.add_package(branch_alias(&a));
    let mut important = repo(RepoKind::Remote);
    let a_important = important.add_package(pkg("A", "dev-feature-a"));
    let mut alias_important = branch_alias(&a_important);
    alias_important.root_package_alias = true;
    let alias_important = important.add_package(alias_important);
    let a2_important = important.add_package(pkg("A", "dev-master"));
    important.add_package(branch_alias(&a2_important));

    let pool = pool_for(vec![important, r], &["A"]);
    let literals = pool.what_provides("a", Some(&Constraint::single(Op::Eq, "2.1.9999999.9999999-dev")));
    assert!(!literals.is_empty());
    assert_eq!(DefaultPolicy::default().select_preferred_packages(&pool, &literals, None), ids(&pool, &[&alias_important]));
}

fn with_link(name: &str, version: &str, target: &str, kind: LinkType) -> Package {
    let mut p = pkg(name, version);
    let l = link(name, target, PrettyConstraint::new(Constraint::single(Op::Eq, "1.0"), "== 1.0"), kind);
    match kind {
        LinkType::Provide => p.provides = vec![l],
        _ => p.replaces = vec![l],
    }
    p
}

#[test]
fn select_all_providers() {
    let mut r = repo(RepoKind::Remote);
    let a = r.add_package(with_link("A", "1.0", "X", LinkType::Provide));
    let b = r.add_package(with_link("B", "2.0", "X", LinkType::Provide));
    let pool = pool_for(vec![r], &["A", "B"]);
    let literals = ids(&pool, &[&a, &b]);
    assert_eq!(DefaultPolicy::default().select_preferred_packages(&pool, &literals, None), literals);
}

#[test]
fn prefer_non_replacing_from_same_repo() {
    let mut r = repo(RepoKind::Remote);
    let a = r.add_package(pkg("A", "1.0"));
    let b = r.add_package(with_link("B", "2.0", "A", LinkType::Replace));
    let pool = pool_for(vec![r], &["A", "B"]);
    let literals = ids(&pool, &[&a, &b]);
    assert_eq!(DefaultPolicy::default().select_preferred_packages(&pool, &literals, None), literals);
}

#[test]
fn prefer_replacing_package_from_same_vendor() {
    let mut r = repo(RepoKind::Remote);
    let b = r.add_package(with_link("vendor-b/replacer", "1.0", "vendor-a/package", LinkType::Replace));
    let a = r.add_package(with_link("vendor-a/replacer", "1.0", "vendor-a/package", LinkType::Replace));
    let pool = pool_for(vec![r], &["vendor-a/replacer", "vendor-b/replacer"]);
    let literals = ids(&pool, &[&a, &b]);
    assert_eq!(DefaultPolicy::default().select_preferred_packages(&pool, &literals, Some("vendor-a/package")), literals);
}

#[test]
fn select_lowest() {
    let (r, p) = repo_with(&["1.0", "2.0"]);
    select(&DefaultPolicy::new(false, true, None), r, &[&p[0], &p[1]], &[&p[0]]);
}
