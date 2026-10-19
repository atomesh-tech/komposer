//! Port of Composer's `DependencyResolver/PoolOptimizerTest.php` with `Fixtures/pooloptimizer/*.test`.

mod common;

use common::*;
use komposer_package::loader::{LoadOptions, load};
use komposer_package::{LinkType, PackageRef};
use komposer_solver::{DefaultPolicy, Pool, PrettyConstraint, Request, optimize_pool};
use regex::Regex;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

fn removed_versions(pool: &Pool, package: &PackageRef) -> Vec<(String, String)> {
    pool.removed_versions_by_package(package).map(|(v, p)| (v.clone(), p.clone())).collect()
}

#[test]
fn removed_versions_by_package_include_versions_from_replacement_name_groups() {
    let replacing = |name: &str, version: &str| {
        let mut p = pkg(name, version);
        p.replaces = vec![link(name, "package/b", PrettyConstraint::parse("^1.0").unwrap(), LinkType::Replace)];
        Arc::new(p)
    };
    let (a100, a110, c150, c160) =
        (replacing("package/a", "1.0.0"), replacing("package/a", "1.1.0"), replacing("package/c", "1.5.0"), replacing("package/c", "1.6.0"));
    let mut request = Request::new();
    request.require_name("package/a", Some(PrettyConstraint::parse("^1.0").unwrap())).unwrap();
    request.require_name("package/b", Some(PrettyConstraint::parse("^1.0").unwrap())).unwrap();
    let pool = optimize_pool(&DefaultPolicy::default(), &request, Pool::new(vec![a100.clone(), a110.clone(), c150.clone(), c160.clone()]));

    let removed: Vec<String> = removed_versions(&pool, &a110).into_iter().map(|(v, _)| v).collect();
    for p in [&a100, &a110, &c150, &c160] {
        assert!(removed.contains(&p.version), "{} missing in {removed:?}", p.version);
    }
}

#[test]
fn keeping_an_alias_records_removed_versions_for_alias_of_and_sibling_aliases() {
    let p110 = Arc::new(pkg("package/a", "1.1.0"));
    let p110_alias = Arc::new(alias(&p110, "1.1.x-dev"));
    let p110_sibling = Arc::new(alias(&p110, "1.1.x-dev"));
    let p111 = Arc::new(pkg("package/a", "1.1.1"));
    let p111_alias = Arc::new(alias(&p111, "1.1.x-dev"));
    let mut request = Request::new();
    request.require_name("package/a", Some(PrettyConstraint::parse("^1.1").unwrap())).unwrap();
    let pool = Pool::new(vec![p110.clone(), p110_alias.clone(), p110_sibling.clone(), p111.clone(), p111_alias]);
    let pool = optimize_pool(&DefaultPolicy::default(), &request, pool);

    let expected = vec![
        (p110.version.clone(), p110.pretty_version.clone()),
        (p110_alias.version.clone(), p110_alias.pretty_version.clone()),
        (p111.version.clone(), p111.pretty_version.clone()),
    ];
    for p in [&p110_alias, &p110, &p110_sibling] {
        assert_eq!(removed_versions(&pool, p), expected);
    }
}

/// `ArrayLoader::load()` plus the aliased package, the alias first like Composer's test.
fn load_packages(data: &Value) -> Vec<PackageRef> {
    let mut out = vec![];
    for p in data.as_array().unwrap() {
        let mut loaded = load(p.as_object().unwrap(), LoadOptions::default()).unwrap();
        loaded.reverse();
        out.extend(loaded);
    }
    out
}

fn describe(packages: &[PackageRef]) -> Vec<String> {
    let mut out: Vec<String> = packages
        .iter()
        .map(|p| match &p.alias_of {
            Some(of) => format!("{}@{} (alias of {})", p.name, p.version, of.version),
            None => format!("{}@{}", p.name, p.version),
        })
        .collect();
    out.sort();
    out
}

#[test]
fn pool_optimizer_fixtures() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pooloptimizer");
    let re = Regex::new(r"(?:^|\n*)--([A-Z-]+)--\n").unwrap();
    let mut count = 0;
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let text = std::fs::read_to_string(&path).unwrap();
        let mut sections: HashMap<String, String> = HashMap::new();
        let marks: Vec<_> = re.captures_iter(&text).map(|c| (c[1].to_string(), c.get(0).unwrap().start(), c.get(0).unwrap().end())).collect();
        for (i, (name, _, end)) in marks.iter().enumerate() {
            let stop = marks.get(i + 1).map_or(text.len(), |m| m.1);
            sections.insert(name.clone(), text[*end..stop].to_string());
        }
        let request_data: Value = serde_json::from_str(&sections["REQUEST"]).unwrap();
        let mut request = Request::new();
        for p in load_packages(request_data.get("locked").unwrap_or(&Value::Array(vec![]))) {
            request.lock_package(p);
        }
        for p in load_packages(request_data.get("fixed").unwrap_or(&Value::Array(vec![]))) {
            request.fix_package(p);
        }
        for (name, c) in request_data["require"].as_object().unwrap() {
            request.require_name(name, Some(PrettyConstraint::parse(c.as_str().unwrap()).unwrap())).unwrap();
        }
        let flag = |k: &str| request_data.get(k) == Some(&Value::Bool(true));
        let policy = DefaultPolicy::new(flag("preferStable"), flag("preferLowest"), None);
        let before = load_packages(&serde_json::from_str(&sections["POOL-BEFORE"]).unwrap());
        let after = load_packages(&serde_json::from_str(&sections["POOL-AFTER"]).unwrap());
        let pool = optimize_pool(&policy, &request, Pool::new(before));
        assert_eq!(describe(pool.packages()), describe(&after), "{}: {}", path.display(), sections["TEST"].trim());
        count += 1;
    }
    assert_eq!(count, 9);
}
