//! Port of Composer's `DependencyResolver/TransactionTest.php`, plus `LockTransaction` behavior.

mod common;

use common::*;
use komposer_package::repository::RepoKind;
use komposer_package::{Location, Package, PackageRef, RootAlias};
use komposer_solver::{PrettyConstraint, transaction_operations};
use serde_json::json;
use std::sync::Arc;

fn arc(p: Package) -> PackageRef {
    Arc::new(p)
}

fn typed(name: &str, kind: &str) -> Package {
    let mut p = pkg(name, "1.0.0");
    p.package_type = kind.into();
    p
}

#[test]
fn transaction_generation_and_sorting() {
    let a = arc(pkg("a/a", "dev-master"));
    let a_alias = arc(alias(&a, "1.0.x-dev"));
    let b = arc(pkg("b/b", "1.0.0"));
    let e = arc(pkg("e/e", "dev-foo"));
    let e_alias = arc(alias(&e, "1.0.x-dev"));
    let c = arc(pkg("c/c", "1.0.0"));
    let present = vec![a.clone(), a_alias.clone(), b.clone(), e.clone(), e_alias.clone(), c.clone()];

    let b_new = arc(pkg("b/b", "2.1.3"));
    let d = arc(requires(pkg("d/d", "1.2.3"), &[("f/f", vc(">", "0.2")), ("g/provider", vc(">", "0.2"))]));
    let f = arc(pkg("f/f", "1.0.0"));
    let f_alias1 = arc(alias(&f, "dev-foo"));
    let mut g = pkg("g/g", "1.0.0");
    g.provides = vec![link("g/g", "g/provider", vc("==", "1.0.0"), komposer_package::LinkType::Provide)];
    let g = arc(g);
    let a0_first = arc(pkg("a0/first", "1.2.3"));
    let f_alias2 = arc(alias(&f, "dev-bar"));
    let plugin = arc(typed("x/plugin", "composer-installer"));
    let plugin2_dep = arc(pkg("x/plugin2-dep", "1.0.0"));
    let plugin2 = arc(requires(typed("x/plugin2", "composer-plugin"), &[("x/plugin2-dep", vc("=", "1.0.0"))]));
    let mut dl = typed("x/downloads-modifying", "composer-plugin");
    dl.extra = json!({"plugin-modifies-downloads": true});
    let dl = arc(dl);
    let dl2_dep = arc(pkg("x/downloads-modifying2-dep", "1.0.0"));
    let mut dl2 = requires(typed("x/downloads-modifying2", "composer-plugin"), &[("x/downloads-modifying2-dep", vc("=", "1.0.0"))]);
    dl2.extra = json!({"plugin-modifies-downloads": true});
    let dl2 = arc(dl2);
    let result = vec![
        a.clone(),
        a_alias.clone(),
        b_new.clone(),
        d.clone(),
        f.clone(),
        f_alias1.clone(),
        g.clone(),
        a0_first.clone(),
        f_alias2.clone(),
        plugin.clone(),
        plugin2_dep.clone(),
        plugin2.clone(),
        dl.clone(),
        dl2_dep.clone(),
        dl2.clone(),
    ];

    check_operations(
        &transaction_operations(&present, &result),
        &[
            Exp::Remove(&c),
            Exp::Remove(&e),
            Exp::MarkAliasUninstalled(&e_alias),
            Exp::Install(&dl),
            Exp::Install(&dl2_dep),
            Exp::Install(&dl2),
            Exp::Install(&plugin),
            Exp::Install(&plugin2_dep),
            Exp::Install(&plugin2),
            Exp::Install(&a0_first),
            Exp::Update(&b, &b_new),
            Exp::Install(&g),
            Exp::Install(&f),
            Exp::MarkAliasInstalled(&f_alias2),
            Exp::MarkAliasInstalled(&f_alias1),
            Exp::Install(&d),
        ],
    );
}

#[test]
fn reference_and_abandoned_changes_are_updates() {
    let with_ref = |r: &str| {
        let mut p = pkg("a/a", "dev-main");
        p.source = Some(Location { kind: "git".into(), url: "u".into(), reference: Some(r.into()), ..Location::default() });
        arc(p)
    };
    let (old, new) = (with_ref("aaa"), with_ref("bbb"));
    check_operations(&transaction_operations(std::slice::from_ref(&old), std::slice::from_ref(&new)), &[Exp::Update(&old, &new)]);
    check_operations(&transaction_operations(std::slice::from_ref(&old), std::slice::from_ref(&old)), &[]);

    let mut abandoned = (*old).clone();
    abandoned.meta.insert("abandoned".into(), json!(true));
    let abandoned = arc(abandoned);
    check_operations(&transaction_operations(std::slice::from_ref(&old), std::slice::from_ref(&abandoned)), &[Exp::Update(&old, &abandoned)]);
}

/// A full update, then the no-dev extraction moving packages from dev to non-dev, and the used root aliases.
#[test]
fn lock_transaction_packages_and_aliases() {
    let mut r = repo(RepoKind::Remote);
    let a = r.add_package(requires(pkg("a/a", "1.0"), &[("b/b", any())]));
    let b = r.add_package(pkg("b/b", "1.0"));
    let mut c_alias = alias(&r.add_package(pkg("c/c", "dev-main")), "1.0.0");
    c_alias.root_package_alias = true;
    let c_alias = r.add_package(c_alias);
    let env = Env::new(vec![r], repo(RepoKind::Lock));

    let mut request = env.request();
    request.require_name("a/a", None).unwrap();
    request.require_name("c/c", Some(vc("==", "1.0.0"))).unwrap();
    let (_, result, _) = env.solve(&mut request);
    let mut transaction = result.unwrap();
    let mut names: Vec<String> = transaction.new_lock_packages(false, false).iter().map(|p| p.to_string()).collect();
    names.sort();
    assert_eq!(names, ["a/a-1.0.0.0", "b/b-1.0.0.0", "c/c-dev-main"]);
    assert!(transaction.new_lock_packages(true, false).is_empty());

    // only a/a is required without dev requirements
    let mut request = env.request();
    request.require_name("a/a", None).unwrap();
    let (_, extraction, _) = env.solve(&mut request);
    transaction.set_non_dev_packages(&extraction.unwrap());
    let mut non_dev: Vec<String> = transaction.new_lock_packages(false, false).iter().map(|p| p.name.clone()).collect();
    non_dev.sort();
    assert_eq!(non_dev, ["a/a", "b/b"]);
    let dev: Vec<String> = transaction.new_lock_packages(true, false).iter().map(|p| p.name.clone()).collect();
    assert_eq!(dev, ["c/c"]);
    assert!(Arc::ptr_eq(&a, &transaction.new_lock_packages(false, false).into_iter().find(|p| p.name == "a/a").unwrap()));
    let _ = (b, c_alias);

    let aliases = [
        RootAlias { package: "d/d".into(), version: "dev-main".into(), alias: "2.0".into(), alias_normalized: "2.0.0.0".into() },
        RootAlias { package: "c/c".into(), version: "dev-main".into(), alias: "1.0.0".into(), alias_normalized: "1.0.0.0".into() },
    ];
    assert_eq!(transaction.aliases(&aliases), vec![aliases[1].clone()]);
}

#[test]
fn update_mirrors_keeps_present_references() {
    let dist =
        |url: &str, reference: &str| Location { kind: "zip".into(), url: url.into(), reference: Some(reference.into()), ..Location::default() };
    let source =
        |url: &str, reference: &str| Location { kind: "git".into(), url: url.into(), reference: Some(reference.into()), ..Location::default() };
    let old_sha = "1111111111111111111111111111111111111111";
    let new_sha = "2222222222222222222222222222222222222222";

    let mut locked_pkg = pkg("a/a", "1.0.0");
    locked_pkg.source = Some(source("https://old.example/a.git", old_sha));
    locked_pkg.dist = Some(dist(&format!("https://api.github.com/repos/a/a/zipball/{old_sha}"), old_sha));
    let mut l = repo(RepoKind::Lock);
    l.add_package(locked_pkg);

    let mut remote_pkg = pkg("a/a", "1.0.0");
    remote_pkg.source = Some(source("https://new.example/a.git", new_sha));
    remote_pkg.dist = Some(dist(&format!("https://api.github.com/repos/a/a/zipball/{new_sha}"), new_sha));
    let mut r = repo(RepoKind::Remote);
    r.add_package(remote_pkg);

    let env = Env::new(vec![r], l);
    let mut request = env.request();
    request.require_name("a/a", Some(PrettyConstraint::parse("1.0.0").unwrap())).unwrap();
    let (_, result, _) = env.solve(&mut request);
    let transaction = result.unwrap();
    let locked = &transaction.new_lock_packages(false, true)[0];
    assert_eq!(locked.source_reference(), Some(old_sha));
    assert_eq!(locked.source.as_ref().unwrap().url, "https://new.example/a.git");
    assert_eq!(locked.dist.as_ref().unwrap().url, format!("https://api.github.com/repos/a/a/zipball/{old_sha}"));
    assert_eq!(transaction.new_lock_packages(false, false)[0].source_reference(), Some(new_sha));
}
