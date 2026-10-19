//! Ports of Composer's `RuleTest`, `RuleSetTest::testPrettyString`, `PoolTest`, `RequestTest` and `ProblemTest`,
//! plus solver problem outputs taken from Composer's installer fixtures.

mod common;

use common::*;
use indexmap::IndexMap;
use komposer_package::io::Io;
use komposer_package::loader::{LoadOptions, load};
use komposer_package::repository::{ArrayRepository, RepoKind, Repository};
use komposer_package::{LinkType, Package, PackageRef};
use komposer_semver::{Constraint, Stability};
use komposer_solver::{
    BlockScope, DefaultPolicy, PlatformRequirementFilter, PolicyOptions, Pool, PoolOptions, PrettyConstraint, Reason, RepositorySet, Request,
    RuleKind, RuleSet, RuleType, Solver, SolverProblems, UpdateAllowTransitive, missing_locked_package_reason,
};
use serde_json::{Value, json};
use std::sync::Arc;

fn arc(p: Package) -> PackageRef {
    Arc::new(p)
}

fn empty_context() -> (RepositorySet, Request) {
    (RepositorySet::new(Stability::Stable), Request::new())
}

#[test]
fn rule_pretty_string() {
    let foo = arc(pkg("foo", "2.1"));
    let mut baz = pkg("baz", "1.1");
    baz.requires = vec![link("baz", "foo", any(), LinkType::Require)];
    let baz = arc(baz);
    let pool = Pool::new(vec![foo, baz.clone()]);
    let mut rules = RuleSet::new();
    let rule = rules.add(&[1, -2], RuleKind::Generic, Reason::PackageRequires { package: baz, link: 0 }, RuleType::Package).unwrap();
    let (set, request) = empty_context();
    // Composer's test link has no type, so it "relates to"
    assert_eq!(rules.rule_pretty_string(rule, &set, &request, &pool, false).unwrap(), "baz 1.1 requires foo * -> satisfiable by foo[2.1].");
}

#[test]
fn locked_filter_list_removed_rule() {
    let package = arc(pkg("vendor/malware", "1.0"));
    let reason = Reason::LockedFilterListRemoved(package.clone());
    assert_eq!(reason.required_package(), Some("vendor/malware"));
    let pool = Pool::new(vec![package]);
    let mut rules = RuleSet::new();
    let rule = rules.add_detached(&[], RuleKind::Generic, reason);
    let (set, request) = empty_context();
    assert_eq!(
        rules.rule_pretty_string(rule, &set, &request, &pool, false).unwrap(),
        "vendor/malware 1.0 was removed by a dependency policy (e.g. malware) and cannot be installed."
    );
}

#[test]
fn rule_set_pretty_string() {
    let pool = Pool::new(vec![arc(pkg("foo", "2.1"))]);
    let mut rules = RuleSet::new();
    let reason = Reason::RootRequire(Box::new(("foo/bar".into(), PrettyConstraint::plain(Constraint::None))));
    rules.add(&[1], RuleKind::Generic, reason, RuleType::Request);
    let (set, request) = empty_context();
    assert!(
        rules
            .pretty_string(&set, &request, &pool, false)
            .unwrap()
            .contains("REQUEST : No package found to satisfy root composer.json require foo/bar")
    );
}

#[test]
fn pool_what_provides_and_ids() {
    let first = arc(pkg("foo", "1"));
    let second = arc(pkg("foo", "2"));
    let pool = Pool::new(vec![first.clone(), second.clone()]);
    assert_eq!(&*pool.what_provides("foo", None), &[1, 2]);
    assert_eq!(&*pool.what_provides("foo", None), &[1, 2]);
    assert_eq!(&*pool.what_provides("foo", Some(&vc("==", "2").constraint)), &[2]);
    assert!(Arc::ptr_eq(pool.package(1), &first));
    assert_eq!(pool.id_of(&second), Some(2));
    assert!(Pool::new(vec![]).what_provides("foo", None).is_empty());
}

#[test]
fn request_requires() {
    let mut request = Request::new();
    request.require_name("Foo", None).unwrap();
    request.require_name("bar", Some(vc("=", "1"))).unwrap();
    assert_eq!(request.requires()["foo"], PrettyConstraint::new(Constraint::Any, "*"));
    assert_eq!(request.requires()["bar"], vc("=", "1"));
    assert!(request.require_name("bar", None).is_err());
}

#[test]
fn missing_locked_package_reason_for_filter_list_removed_package() {
    let mut entry = malware("vendor/malware", "*");
    entry.url = Some("https://example.org/malware/vendor-malware".into());
    entry.reason = Some("looks suspicious".into());
    let repos: Vec<Arc<dyn Repository>> = vec![Arc::new(PolicyRepo { filter: vec![entry], ..PolicyRepo::default() })];
    let package = arc(pkg("vendor/malware", "1.0.0"));
    let mut request = Request::new();
    request.fix_locked_package(package.clone());
    let pool = komposer_solver::filter_list_filter(
        Pool::new(vec![package.clone()]),
        &repos,
        &request,
        &PolicyOptions::default(),
        BlockScope::Install,
        &Io::null(),
    )
    .unwrap();

    let (prefix, suffix) = missing_locked_package_reason(&pool, &package).unwrap();
    assert_eq!(prefix, "- Package vendor/malware 1.0.0 (in the lock file) ");
    assert_eq!(
        suffix,
        "was not loaded, because it was flagged as malware (see https://example.org/malware/vendor-malware) reason: looks suspicious. To ignore filters for this package, add the package to the \"policy.malware.ignore\" config. To turn the feature off entirely, you can set \"policy.malware.block\" to false."
    );
}

/// `ArrayLoader::load()` of a composer.json-like package.
fn json_package(data: Value) -> PackageRef {
    load(data.as_object().unwrap(), LoadOptions::default()).unwrap().remove(0)
}

fn repo_of(kind: RepoKind, packages: Value) -> ArrayRepository {
    let mut repo = ArrayRepository::new(if kind == RepoKind::Lock { "lock repo" } else { "package repo" }, kind);
    for p in packages.as_array().unwrap() {
        repo.add(json_package(p.clone()));
    }
    repo
}

/// `Installer::doUpdate()` up to the solver problems: root and platform packages fixed, root requires required.
struct Update {
    requires: Vec<(&'static str, &'static str)>,
    repos: Vec<ArrayRepository>,
    platform: Value,
    lock: Value,
    allow_list: Option<Vec<String>>,
    minimum_stability: Stability,
}

impl Update {
    fn problems(self) -> String {
        let mut root = pkg("__root__", "1.0.0");
        root.pretty_version = "1.0.0+no-version-set".into();
        let mut root_repo = ArrayRepository::new("root package repo", RepoKind::Root);
        let root = root_repo.add_package(root);
        let platform = repo_of(RepoKind::Platform, self.platform);
        let platform_packages = platform.all().to_vec();
        let locked: Arc<dyn Repository> = Arc::new(repo_of(RepoKind::Lock, self.lock));

        let requires: IndexMap<String, PrettyConstraint> =
            self.requires.iter().map(|(n, c)| (n.to_string(), PrettyConstraint::parse(c).unwrap())).collect();
        let mut set = RepositorySet::new(self.minimum_stability).with_root_requires(requires.clone());
        set.add_repository(Arc::new(root_repo));
        set.add_repository(Arc::new(platform));
        for r in self.repos {
            set.add_repository(Arc::new(r));
        }
        set.add_repository(locked.clone());

        let mut request = Request::with_locked_repository(locked).unwrap();
        request.fix_package(root);
        for p in platform_packages {
            request.fix_package(p);
        }
        if let Some(list) = self.allow_list {
            request.set_update_allow_list(list, UpdateAllowTransitive::OnlyListed);
        }
        for (name, constraint) in requires {
            request.require_name(&name, Some(constraint)).unwrap();
        }
        let pool = set.create_pool(&mut request, &Io::null(), &PoolOptions::default()).unwrap();
        let (policy, io) = (DefaultPolicy::default(), Io::null());
        let mut solver = Solver::new(&policy, &pool, &io);
        let err = solver.solve(&request, &PlatformRequirementFilter::IgnoreNothing).expect_err("unsolvable");
        let problems = err.downcast_ref::<SolverProblems>().expect("solver problems");
        problems.pretty_string(&set, &request, &pool, false, false, &["".into(), "/etc/php.ini".into()]).unwrap()
    }
}

/// `Fixtures/installer/solver-problems.test`, with this platform in place of the PHP running the tests.
#[test]
fn installer_fixture_solver_problems() {
    let repo = repo_of(
        RepoKind::Remote,
        json!([
            { "name": "package/found", "version": "2.0.0", "require": { "unstable/package2": "2.*" } },
            { "name": "package/found3", "version": "2.0.0", "require": { "unstable/package2": "2.*" } },
            { "name": "package/found4", "version": "2.0.0", "require": { "non-existent/pkg2": "1.*" } },
            { "name": "package/found5", "version": "2.0.0", "require": { "requirer/pkg": "1.*" } },
            { "name": "package/found6", "version": "2.0.0", "require": { "stable-requiree-excluded/pkg2": "1.0.1" } },
            { "name": "package/found7", "version": "2.0.0", "require": { "php-64bit": "1.0.1" } },
            { "name": "conflict/requirer", "version": "2.0.0", "require": { "conflict/dep": "1.0.0" } },
            { "name": "conflict/requirer2", "version": "2.0.0", "require": { "conflict/dep": "2.0.0" } },
            { "name": "conflict/dep", "version": "1.0.0" },
            { "name": "conflict/dep", "version": "2.0.0" },
            { "name": "unstable/package", "version": "2.0.0-alpha" },
            { "name": "unstable/package", "version": "1.0.0" },
            { "name": "unstable/package2", "version": "2.0.0-alpha" },
            { "name": "unstable/package2", "version": "1.0.0" },
            { "name": "requirer/pkg", "version": "1.0.0", "require": { "dependency/pkg": "1.0.0", "dependency/unstable-pkg": "1.0.0-dev" } },
            { "name": "dependency/pkg", "version": "2.0.0" },
            { "name": "dependency/pkg", "version": "1.0.0" },
            { "name": "dependency/unstable-pkg", "version": "1.0.0-dev" },
            { "name": "stable-requiree-excluded/pkg", "version": "1.0.1" },
            { "name": "stable-requiree-excluded/pkg", "version": "1.0.0" },
            { "name": "api/provider", "description": "Provides the missing API", "version": "1.0.0", "provide": { "missing/provided-api": "1.*" } }
        ]),
    );
    let output = Update {
        requires: vec![
            ("package/found", "2.*"),
            ("package/found3", "2.*"),
            ("package/found4", "2.*"),
            ("package/found5", "2.*"),
            ("package/found6", "2.*"),
            ("package/found7", "2.*"),
            ("missing/provided-api", "2.*"),
            ("conflict/requirer", "2.*"),
            ("conflict/requirer2", "2.*"),
            ("unstable/package", "2.*"),
            ("non-existent/pkg", "1.*"),
            ("requirer/pkg", "1.*"),
            ("dependency/pkg", "2.*"),
            ("stable-requiree-excluded/pkg", "1.0.1"),
            ("lib-xml", "1002.*"),
            ("lib-icu", "1001.*"),
            ("ext-xml", "1002.*"),
            ("php", "1"),
        ],
        repos: vec![repo],
        platform: json!([
            {"name": "php", "version": "8.3.1"},
            {"name": "php-64bit", "version": "8.3.1"},
            {"name": "ext-xml", "version": "8.3.1"},
            {"name": "ext-intl", "version": "8.3.1"},
            {"name": "lib-icu", "version": "74.1"}
        ]),
        lock: json!([
            { "name": "stable-requiree-excluded/pkg", "version": "1.0.0" },
            { "name": "stable-requiree-excluded/pkg2", "version": "1.0.0" }
        ]),
        allow_list: Some(["unstable/package", "requirer/pkg", "dependency/pkg", "conflict/requirer"].map(String::from).to_vec()),
        minimum_stability: Stability::Stable,
    }
    .problems();

    let expected = r#"
  Problem 1
    - Root composer.json requires missing/provided-api 2.*, it could not be found in any version, but the following packages provide it:
      - api/provider Provides the missing API
      Consider requiring one of these to satisfy the missing/provided-api requirement.
  Problem 2
    - Root composer.json requires unstable/package 2.*, found unstable/package[2.0.0-alpha] but it does not match your minimum-stability.
  Problem 3
    - Root composer.json requires non-existent/pkg, it could not be found in any version, there may be a typo in the package name.
  Problem 4
    - Root composer.json requires stable-requiree-excluded/pkg 1.0.1 (exact version match: 1.0.1 or 1.0.1.0), found stable-requiree-excluded/pkg[1.0.1] but the package is fixed to 1.0.0 (lock file version) by a partial update and that version does not match. Make sure you list it as an argument for the update command.
  Problem 5
    - Root composer.json requires linked library lib-xml 1002.* but it has the wrong version installed or is missing from your system, make sure to load the extension providing it.
  Problem 6
    - Root composer.json requires linked library lib-icu 1001.* but it has the wrong version installed, try upgrading the intl extension.
  Problem 7
    - Root composer.json requires PHP extension ext-xml 1002.* but it has the wrong version installed (8.3.1).
  Problem 8
    - Root composer.json requires php 1 (exact version match: 1, 1.0, 1.0.0 or 1.0.0.0) but your php version (8.3.1) does not satisfy that requirement.
  Problem 9
    - Root composer.json requires package/found 2.* -> satisfiable by package/found[2.0.0].
    - package/found 2.0.0 requires unstable/package2 2.* -> found unstable/package2[2.0.0-alpha] but it does not match your minimum-stability.
  Problem 10
    - Root composer.json requires package/found3 2.* -> satisfiable by package/found3[2.0.0].
    - package/found3 2.0.0 requires unstable/package2 2.* -> found unstable/package2[2.0.0-alpha] but it does not match your minimum-stability.
  Problem 11
    - Root composer.json requires package/found4 2.* -> satisfiable by package/found4[2.0.0].
    - package/found4 2.0.0 requires non-existent/pkg2 1.* -> could not be found in any version, there may be a typo in the package name.
  Problem 12
    - Root composer.json requires package/found6 2.* -> satisfiable by package/found6[2.0.0].
    - package/found6 2.0.0 requires stable-requiree-excluded/pkg2 1.0.1 -> found stable-requiree-excluded/pkg2[1.0.0] but it does not match the constraint.
  Problem 13
    - Root composer.json requires package/found7 2.* -> satisfiable by package/found7[2.0.0].
    - package/found7 2.0.0 requires php-64bit 1.0.1 -> your php-64bit version (8.3.1) does not satisfy that requirement.
  Problem 14
    - Root composer.json requires requirer/pkg 1.* -> satisfiable by requirer/pkg[1.0.0].
    - requirer/pkg 1.0.0 requires dependency/pkg 1.0.0 -> found dependency/pkg[1.0.0] but it conflicts with your root composer.json require (2.*).
  Problem 15
    - Root composer.json requires package/found5 2.* -> satisfiable by package/found5[2.0.0].
    - package/found5 2.0.0 requires requirer/pkg 1.* -> satisfiable by requirer/pkg[1.0.0].
    - requirer/pkg 1.0.0 requires dependency/pkg 1.0.0 -> found dependency/pkg[1.0.0] but it conflicts with your root composer.json require (2.*).

Potential causes:
 - A typo in the package name
 - The package is not available in a stable-enough version according to your minimum-stability setting
   see <https://getcomposer.org/doc/04-schema.md#minimum-stability> for more details.
 - It's a private package and you forgot to add a custom repository to find it

Read <https://getcomposer.org/doc/articles/troubleshooting.md> for further common problems.

To enable extensions, verify that they are enabled in your .ini files:
    - /etc/php.ini
You can also run `php --ini` in a terminal to see which files are used by PHP in CLI mode.
Alternatively, you can run Composer with `--ignore-platform-req=ext-xml` to temporarily ignore these required extensions.

Use the option --with-all-dependencies (-W) to allow upgrades, downgrades and removals for packages currently locked to specific versions."#;
    assert_eq!(output, expected);
}

/// `Fixtures/installer/alias-solver-problems.test`: a learned rule in the output.
#[test]
fn installer_fixture_alias_solver_problems() {
    let repo = repo_of(
        RepoKind::Remote,
        json!([
            {"name": "a/a", "version": "dev-master", "require": {"d/d": "1.0.0"}, "default-branch": true},
            {"name": "b/b", "version": "dev-master", "require": {"d/d": "2.0.0"}, "default-branch": true},
            {"name": "d/d", "version": "1.0.0"},
            {"name": "d/d", "version": "2.0.0"}
        ]),
    );
    let output = Update {
        requires: vec![("a/a", "*@dev"), ("b/b", "*@dev")],
        repos: vec![repo],
        platform: json!([]),
        lock: json!([]),
        allow_list: Some(vec!["a/a".into(), "b/b".into()]),
        minimum_stability: Stability::Dev,
    }
    .problems();
    assert_eq!(
        output,
        "\n  Problem 1\n    - Root composer.json requires a/a *@dev -> satisfiable by a/a[dev-master].\n    - Root composer.json requires b/b *@dev -> satisfiable by b/b[dev-master].\n    - a/a dev-master requires d/d 1.0.0 -> satisfiable by d/d[1.0.0].\n    - b/b dev-master requires d/d 2.0.0 -> satisfiable by d/d[2.0.0].\n    - Conclusion: install b/b dev-master (conflict analysis result)\n    - You can only install one version of a package, so only one of these can be installed: d/d[1.0.0, 2.0.0].\n"
    );
}

/// Package versions are condensed and deduplicated across rules (`problems-reduce-versions.test` style).
#[test]
fn deduplicated_requirements_list_versions() {
    let mut packages = vec![];
    for minor in 0..8 {
        packages.push(json!({"name": "a/a", "version": format!("1.{minor}.0"), "require": {"b/b": "^1.0"}}));
    }
    packages.push(json!({"name": "b/b", "version": "2.0.0"}));
    let output = Update {
        requires: vec![("a/a", "^1.0")],
        repos: vec![repo_of(RepoKind::Remote, Value::Array(packages))],
        platform: json!([]),
        lock: json!([]),
        allow_list: None,
        minimum_stability: Stability::Stable,
    }
    .problems();
    assert_eq!(
        output,
        "\n  Problem 1\n    - Root composer.json requires a/a ^1.0 -> satisfiable by a/a[1.0.0, ..., 1.7.0].\n    - a/a[1.0.0, ..., 1.7.0] require b/b ^1.0 -> found b/b[2.0.0] but it does not match the constraint.\n"
    );
}

#[test]
fn problem_messages_for_conflicts_and_replacers() {
    let conflicting = || {
        let mut p = pkg("a/a", "1.0");
        p.conflicts = vec![link("a/a", "c/replaced", vc(">=", "1.0"), LinkType::Conflict)];
        p
    };
    let mut r = repo(RepoKind::Remote);
    r.add_package(conflicting());
    let mut b = pkg("b/b", "1.0");
    b.replaces = vec![link("b/b", "c/replaced", vc("==", "1.5"), LinkType::Replace)];
    r.add_package(b);
    let env = Env::new(vec![r], repo(RepoKind::Lock));
    let mut request = env.request();
    request.require_name("a/a", None).unwrap();
    request.require_name("b/b", None).unwrap();
    assert_eq!(
        env.problems(&mut request),
        "\n  Problem 1\n    - Root composer.json requires a/a * -> satisfiable by a/a[1.0].\n    - Root composer.json requires b/b * -> satisfiable by b/b[1.0].\n    - a/a 1.0 conflicts with c/replaced >= 1.0 (b/b 1.0 replaces c/replaced == 1.5).\n"
    );

    let mut r = repo(RepoKind::Remote);
    for name in ["x/one", "x/two"] {
        let mut p = pkg(name, "1.0");
        p.replaces = vec![link(name, "x/common", any(), LinkType::Replace)];
        r.add_package(p);
    }
    let env = Env::new(vec![r], repo(RepoKind::Lock));
    let mut request = env.request();
    request.require_name("x/one", None).unwrap();
    request.require_name("x/two", None).unwrap();
    assert_eq!(
        env.problems(&mut request),
        "\n  Problem 1\n    - Root composer.json requires x/one * -> satisfiable by x/one[1.0].\n    - Root composer.json requires x/two * -> satisfiable by x/two[1.0].\n    - Only one of these can be installed: x/two[1.0], x/one[1.0]. They both replace x/common and thus cannot coexist.\n"
    );
}

#[test]
fn problem_messages_for_missing_packages() {
    let mut r = repo(RepoKind::Remote);
    r.add_package(pkg("a/a", "1.0.0"));
    r.add_package(pkg("main/branch", "dev-main"));
    let env = Env::new(vec![r], repo(RepoKind::Lock));

    let mut request = env.request();
    request.require_name("a/a", Some(PrettyConstraint::parse("2.0").unwrap())).unwrap();
    assert!(env.problems(&mut request).starts_with(
        "\n  Problem 1\n    - Root composer.json requires a/a 2.0 (exact version match: 2.0, 2.0.0 or 2.0.0.0), found a/a[1.0.0] but it does not match the constraint.\n"
    ));

    let mut request = env.request();
    request.require_name("main/branch", Some(PrettyConstraint::parse("dev-master").unwrap())).unwrap();
    assert!(env.problems(&mut request).contains(
        "- Root composer.json requires main/branch dev-master, found main/branch[dev-main] but it does not match the constraint. Perhaps dev-master was renamed to dev-main?"
    ));

    let mut request = env.request();
    request.require_name("in valid", None).unwrap();
    assert!(env.problems(&mut request).contains(
        "- Root composer.json requires in valid, it could not be found, it looks like its name is invalid, \" \" is not allowed in package names."
    ));

    let mut request = env.request();
    request.require_name("ext-missing", None).unwrap();
    assert!(env.problems(&mut request).contains(
        "- Root composer.json requires PHP extension ext-missing * but it is missing from your system. Install or enable PHP's missing extension."
    ));
}

#[test]
fn problem_message_for_lock_file_only_packages() {
    let mut l = repo(RepoKind::Lock);
    l.add_package(pkg("a/a", "1.0.0"));
    let env = Env::new(vec![repo(RepoKind::Remote)], l);
    let mut request = env.request();
    request.require_name("a/a", Some(PrettyConstraint::parse("^1.0").unwrap())).unwrap();
    let mut request_with_list = request.clone();
    request_with_list.set_update_allow_list(vec!["a/a".into()], UpdateAllowTransitive::OnlyListed);
    assert!(env.problems(&mut request_with_list).contains(
        "- Root composer.json requires a/a ^1.0, found a/a[1.0.0] in the lock file but not in remote repositories, make sure you avoid updating this package to keep the one from the lock file."
    ));
}

#[test]
fn problem_message_for_advisory_removed_versions() {
    let mut r = repo(RepoKind::Remote);
    r.add_package(pkg("acme/package", "1.0.0"));
    let mut set = RepositorySet::new(Stability::Stable);
    set.add_repository(Arc::new(r));
    set.add_repository(Arc::new(PolicyRepo {
        advisories: vec![advisory("acme/package", "PKSA-aaaa-bbbb-cccc", Some("CVE-1"), "<2.0")],
        ..PolicyRepo::default()
    }));
    let mut request = Request::new();
    request.require_name("acme/package", Some(PrettyConstraint::parse("^1.0").unwrap())).unwrap();
    let options = PoolOptions::for_update(&DefaultPolicy::default(), &PolicyOptions::default(), false, set.repositories());
    let pool = set.create_pool(&mut request, &Io::null(), &options).unwrap();
    assert!(pool.is_empty());
    let (policy, io) = (DefaultPolicy::default(), Io::null());
    let err = Solver::new(&policy, &pool, &io).solve(&request, &PlatformRequirementFilter::IgnoreNothing).err().unwrap();
    let text = err.downcast_ref::<SolverProblems>().unwrap().pretty_string(&set, &request, &pool, false, false, &[]).unwrap();
    assert_eq!(
        text,
        "\n  Problem 1\n    - Root composer.json requires acme/package ^1.0, found acme/package[1.0.0] but these were not loaded, because they are affected by security advisories (\"<href=https://packagist.org/security-advisories/PKSA-aaaa-bbbb-cccc>PKSA-aaaa-bbbb-cccc</>\"). Go to https://packagist.org/security-advisories/ to find advisory details. To ignore the advisories, add their IDs to the \"policy.advisories.ignore-id\" config or add the package to \"policy.advisories.ignore\". To turn the feature off entirely, you can set \"policy.advisories.block\" to false.\n"
    );
}

/// `--with` temporary constraints filter the pool, and are named in problems.
#[test]
fn temporary_constraints() {
    let repo = || repo_of(RepoKind::Remote, json!([{"name": "a/a", "version": "1.0.0"}, {"name": "a/a", "version": "2.0.0"}]));
    let temporary: IndexMap<String, PrettyConstraint> = [("a/a".to_string(), PrettyConstraint::parse("^1.0").unwrap())].into();
    let solve = |requirement: &str| {
        let mut set = RepositorySet::new(Stability::Stable).with_temporary_constraints(temporary.clone());
        set.add_repository(Arc::new(repo()));
        let mut request = Request::new();
        request.require_name("a/a", Some(PrettyConstraint::parse(requirement).unwrap())).unwrap();
        let pool = set.create_pool(&mut request, &Io::null(), &PoolOptions::default()).unwrap();
        let (policy, io) = (DefaultPolicy::default(), Io::null());
        let result = Solver::new(&policy, &pool, &io).solve(&request, &PlatformRequirementFilter::IgnoreNothing);
        result.map_err(|e| e.downcast_ref::<SolverProblems>().unwrap().pretty_string(&set, &request, &pool, false, false, &[]).unwrap())
    };

    let installed = solve("*").unwrap().operations().iter().map(|op| op.package().to_string()).collect::<Vec<_>>();
    assert_eq!(installed, ["a/a-1.0.0.0"]);
    assert!(
        solve("^2.0")
            .unwrap_err()
            .contains("- Root composer.json requires a/a ^2.0, found a/a[2.0.0] but it conflicts with your temporary update constraint (a/a:^1.0).")
    );
}
