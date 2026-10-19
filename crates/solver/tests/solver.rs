//! Port of Composer's `DependencyResolver/SolverTest.php`.

mod common;

use common::*;
use komposer_package::io::Io;
use komposer_package::repository::RepoKind;
use komposer_package::{LinkType, PackageRef};
use komposer_semver::Constraint;
use komposer_solver::{DefaultPolicy, PlatformRequirementFilter, Pool, PrettyConstraint, Reason, Request, Solver, SolverProblems};
use std::sync::Arc;

fn add(repo: &mut komposer_package::repository::ArrayRepository, p: komposer_package::Package) -> PackageRef {
    repo.add_package(p)
}

fn remote() -> komposer_package::repository::ArrayRepository {
    repo(RepoKind::Remote)
}

fn lock() -> komposer_package::repository::ArrayRepository {
    repo(RepoKind::Lock)
}

fn operations(env: &Env, request: &mut Request, expected: &[Exp]) {
    let (_, result, _) = env.solve(request);
    let transaction = result.unwrap_or_else(|e| panic!("{e}"));
    check_operations(transaction.operations(), expected);
}

#[test]
fn solver_install_single() {
    let mut r = remote();
    let a = add(&mut r, pkg("a/a", "1.0"));
    let env = Env::new(vec![r], lock());
    let mut request = env.request();
    request.require_name("a/a", None).unwrap();
    operations(&env, &mut request, &[Exp::Install(&a)]);
}

#[test]
fn solver_remove_if_not_requested() {
    let mut l = lock();
    let a = add(&mut l, pkg("a/a", "1.0"));
    let env = Env::new(vec![remote()], l);
    operations(&env, &mut env.request(), &[Exp::Remove(&a)]);
}

#[test]
fn install_non_existing_package_fails() {
    let mut r = remote();
    add(&mut r, pkg("a/a", "1.0"));
    let env = Env::new(vec![r], lock());
    let mut request = env.request();
    request.require_name("b/b", Some(vc("==", "1"))).unwrap();
    let (pool, result, _) = env.solve(&mut request);
    let err = result.unwrap_err();
    let problems = err.downcast_ref::<SolverProblems>().unwrap();
    assert_eq!(problems.problems.len(), 1);
    assert_eq!(SolverProblems::EXIT_CODE, 2);
    assert_eq!(
        problems.pretty_string(&env.set, &request, &pool, false, false, &[]).unwrap(),
        "\n  Problem 1\n    - Root composer.json requires b/b, it could not be found in any version, there may be a typo in the package name.\n\nPotential causes:\n - A typo in the package name\n - The package is not available in a stable-enough version according to your minimum-stability setting\n   see <https://getcomposer.org/doc/04-schema.md#minimum-stability> for more details.\n - It's a private package and you forgot to add a custom repository to find it\n\nRead <https://getcomposer.org/doc/articles/troubleshooting.md> for further common problems."
    );
}

#[test]
fn solver_install_same_package_from_different_repositories() {
    let (mut r1, mut r2) = (remote(), remote());
    let foo1 = add(&mut r1, pkg("foo/foo", "1"));
    add(&mut r2, pkg("foo/foo", "1"));
    let env = Env::new(vec![r1, r2], lock());
    let mut request = env.request();
    request.require_name("foo/foo", None).unwrap();
    operations(&env, &mut request, &[Exp::Install(&foo1)]);
}

#[test]
fn solver_install_with_deps() {
    let mut r = remote();
    let a = add(&mut r, requires(pkg("a/a", "1.0"), &[("b/b", vc("<", "1.1"))]));
    let b = add(&mut r, pkg("b/b", "1.0"));
    add(&mut r, pkg("b/b", "1.1"));
    let env = Env::new(vec![r], lock());
    let mut request = env.request();
    request.require_name("a/a", None).unwrap();
    operations(&env, &mut request, &[Exp::Install(&b), Exp::Install(&a)]);
}

#[test]
fn solver_install_honours_not_equal_operator() {
    let mut r = remote();
    let constraint = Constraint::multi(vec![vc("<=", "1.3").constraint, vc("<>", "1.3").constraint, vc("!=", "1.2").constraint], true);
    let a = add(&mut r, requires(pkg("a/a", "1.0"), &[("b/b", PrettyConstraint::plain(constraint))]));
    add(&mut r, pkg("b/b", "1.0"));
    let b11 = add(&mut r, pkg("b/b", "1.1"));
    add(&mut r, pkg("b/b", "1.2"));
    add(&mut r, pkg("b/b", "1.3"));
    let env = Env::new(vec![r], lock());
    let mut request = env.request();
    request.require_name("a/a", None).unwrap();
    operations(&env, &mut request, &[Exp::Install(&b11), Exp::Install(&a)]);
}

#[test]
fn solver_install_with_deps_in_order() {
    let mut r = remote();
    let a = add(&mut r, pkg("a/a", "1.0"));
    let b = add(&mut r, requires(pkg("b/b", "1.0"), &[("a/a", vc(">=", "1.0")), ("c/c", vc(">=", "1.0"))]));
    let c = add(&mut r, requires(pkg("c/c", "1.0"), &[("a/a", vc(">=", "1.0"))]));
    let env = Env::new(vec![r], lock());
    let mut request = env.request();
    for name in ["a/a", "b/b", "c/c"] {
        request.require_name(name, None).unwrap();
    }
    operations(&env, &mut request, &[Exp::Install(&a), Exp::Install(&c), Exp::Install(&b)]);
}

fn php_constraint(from: &str, to: &str) -> PrettyConstraint {
    PrettyConstraint::plain(Constraint::multi(vec![vc(">=", from).constraint, vc("<", to).constraint], true))
}

/// Same name and version, different requirements: the result depends on the order of the root requires.
#[test]
fn solver_multi_package_name_version_resolution_depends_on_require_order() {
    let mut r = remote();
    let php74 = add(&mut r, pkg("ourcustom/PHP", "7.4.23"));
    let php80 = add(&mut r, pkg("ourcustom/PHP", "8.0.10"));
    let ext74 = add(&mut r, requires(pkg("ourcustom/ext-foobar", "1.0"), &[("ourcustom/PHP", php_constraint("7.4.0", "7.5.0"))]));
    let ext80 = add(&mut r, requires(pkg("ourcustom/ext-foobar", "1.0"), &[("ourcustom/PHP", php_constraint("8.0.0", "8.1.0"))]));
    let env = Env::new(vec![r], lock());

    let mut request = env.request();
    request.require_name("ourcustom/PHP", None).unwrap();
    request.require_name("ourcustom/ext-foobar", None).unwrap();
    operations(&env, &mut request, &[Exp::Install(&php80), Exp::Install(&ext80)]);

    // the ext-foobar requiring php74 comes first in the repo, so requiring it first picks it and thus php74
    let mut request = env.request();
    request.require_name("ourcustom/ext-foobar", None).unwrap();
    request.require_name("ourcustom/PHP", None).unwrap();
    operations(&env, &mut request, &[Exp::Install(&php74), Exp::Install(&ext74)]);
}

#[test]
fn solver_multi_package_name_version_resolution_is_independent_of_require_order_if_ordered_descending_by_requirement() {
    let mut r = remote();
    add(&mut r, pkg("ourcustom/PHP", "7.4"));
    let php80 = add(&mut r, pkg("ourcustom/PHP", "8.0"));
    let ext80 = add(&mut r, requires(pkg("ourcustom/ext-foobar", "1.0"), &[("ourcustom/PHP", php_constraint("8.0.0", "8.1.0"))]));
    add(&mut r, requires(pkg("ourcustom/ext-foobar", "1.0"), &[("ourcustom/PHP", php_constraint("7.4.0", "7.5.0"))]));
    let env = Env::new(vec![r], lock());

    for names in [["ourcustom/PHP", "ourcustom/ext-foobar"], ["ourcustom/ext-foobar", "ourcustom/PHP"]] {
        let mut request = env.request();
        for name in names {
            request.require_name(name, None).unwrap();
        }
        operations(&env, &mut request, &[Exp::Install(&php80), Exp::Install(&ext80)]);
    }
}

#[test]
fn solver_fix_locked() {
    let mut l = lock();
    let a = add(&mut l, pkg("a/a", "1.0"));
    let env = Env::new(vec![remote()], l);
    let mut request = env.request();
    request.fix_package(a);
    operations(&env, &mut request, &[]);
}

#[test]
fn solver_fix_locked_with_alternative() {
    let mut r = remote();
    add(&mut r, pkg("a/a", "1.0"));
    let mut l = lock();
    let a = add(&mut l, pkg("a/a", "1.0"));
    let env = Env::new(vec![r], l);
    let mut request = env.request();
    request.fix_package(a);
    operations(&env, &mut request, &[]);
}

#[test]
fn solver_update_does_only_update() {
    let mut l = lock();
    let a = add(&mut l, requires(pkg("a/a", "1.0"), &[("b/b", vc(">=", "1.0.0.0"))]));
    let b = add(&mut l, pkg("b/b", "1.0"));
    let mut r = remote();
    let new_b = add(&mut r, pkg("b/b", "1.1"));
    let env = Env::new(vec![r], l);
    let mut request = env.request();
    request.fix_package(a);
    request.require_name("b/b", Some(vc("=", "1.1.0.0"))).unwrap();
    operations(&env, &mut request, &[Exp::Update(&b, &new_b)]);
}

#[test]
fn solver_update_single() {
    let mut l = lock();
    let a = add(&mut l, pkg("a/a", "1.0"));
    let mut r = remote();
    let new_a = add(&mut r, pkg("a/a", "1.1"));
    let env = Env::new(vec![r], l);
    let mut request = env.request();
    request.require_name("a/a", None).unwrap();
    operations(&env, &mut request, &[Exp::Update(&a, &new_a)]);
}

#[test]
fn solver_update_all() {
    let mut l = lock();
    let a = add(&mut l, requires(pkg("a/a", "1.0"), &[("b/b", any())]));
    let b = add(&mut l, pkg("b/b", "1.0"));
    let mut r = remote();
    let new_a = add(&mut r, requires(pkg("a/a", "1.1"), &[("b/b", any())]));
    let new_b = add(&mut r, pkg("b/b", "1.1"));
    let env = Env::new(vec![r], l);
    let mut request = env.request();
    request.require_name("a/a", None).unwrap();
    operations(&env, &mut request, &[Exp::Update(&b, &new_b), Exp::Update(&a, &new_a)]);
}

#[test]
fn solver_update_current() {
    let mut l = lock();
    add(&mut l, pkg("a/a", "1.0"));
    let mut r = remote();
    add(&mut r, pkg("a/a", "1.0"));
    let env = Env::new(vec![r], l);
    let mut request = env.request();
    request.require_name("a/a", None).unwrap();
    operations(&env, &mut request, &[]);
}

#[test]
fn solver_update_only_updates_selected_package() {
    let mut l = lock();
    let a = add(&mut l, pkg("a/a", "1.0"));
    let b = add(&mut l, pkg("b/b", "1.0"));
    let mut r = remote();
    let a_newer = add(&mut r, pkg("a/a", "1.1"));
    add(&mut r, pkg("b/b", "1.1"));
    let env = Env::new(vec![r], l);
    let mut request = env.request();
    request.require_name("a/a", None).unwrap();
    request.fix_package(b);
    operations(&env, &mut request, &[Exp::Update(&a, &a_newer)]);
}

#[test]
fn solver_update_constrained() {
    let mut l = lock();
    let a = add(&mut l, pkg("a/a", "1.0"));
    let mut r = remote();
    let new_a = add(&mut r, pkg("a/a", "1.2"));
    add(&mut r, pkg("a/a", "2.0"));
    let env = Env::new(vec![r], l);
    let mut request = env.request();
    request.require_name("a/a", Some(vc("<", "2.0.0.0"))).unwrap();
    operations(&env, &mut request, &[Exp::Update(&a, &new_a)]);
}

#[test]
fn solver_update_fully_constrained_prunes_installed_packages() {
    let mut l = lock();
    let a = add(&mut l, pkg("a/a", "1.0"));
    let b = add(&mut l, pkg("b/b", "1.0"));
    let mut r = remote();
    let new_a = add(&mut r, pkg("a/a", "1.2"));
    add(&mut r, pkg("a/a", "2.0"));
    let env = Env::new(vec![r], l);
    let mut request = env.request();
    request.require_name("a/a", Some(vc("<", "2.0.0.0"))).unwrap();
    operations(&env, &mut request, &[Exp::Remove(&b), Exp::Update(&a, &new_a)]);
}

#[test]
fn solver_all_jobs() {
    let mut l = lock();
    let d = add(&mut l, pkg("d/d", "1.0"));
    let old_c = add(&mut l, pkg("c/c", "1.0"));
    let mut r = remote();
    let a = add(&mut r, requires(pkg("a/a", "2.0"), &[("b/b", vc("<", "1.1"))]));
    let b = add(&mut r, pkg("b/b", "1.0"));
    add(&mut r, pkg("b/b", "1.1"));
    let c = add(&mut r, pkg("c/c", "1.1"));
    add(&mut r, pkg("d/d", "1.0"));
    let env = Env::new(vec![r], l);
    let mut request = env.request();
    request.require_name("a/a", None).unwrap();
    request.require_name("c/c", None).unwrap();
    operations(&env, &mut request, &[Exp::Remove(&d), Exp::Install(&b), Exp::Install(&a), Exp::Update(&old_c, &c)]);
}

#[test]
fn solver_three_alternative_require_and_conflict() {
    let mut r = remote();
    let mut a = requires(pkg("a/a", "2.0"), &[("b/b", vc("<", "1.1"))]);
    a.conflicts = vec![link("a/a", "b/b", vc("<", "1.0"), LinkType::Conflict)];
    let a = add(&mut r, a);
    let middle_b = add(&mut r, pkg("b/b", "1.0"));
    add(&mut r, pkg("b/b", "1.1"));
    add(&mut r, pkg("b/b", "0.9"));
    let env = Env::new(vec![r], lock());
    let mut request = env.request();
    request.require_name("a/a", None).unwrap();
    operations(&env, &mut request, &[Exp::Install(&middle_b), Exp::Install(&a)]);
}

#[test]
fn solver_obsolete() {
    let mut l = lock();
    let a = add(&mut l, pkg("a/a", "1.0"));
    let mut r = remote();
    let mut b = pkg("b/b", "1.0");
    b.replaces = vec![link("b/b", "a/a", any(), LinkType::Replace)];
    let b = add(&mut r, b);
    let env = Env::new(vec![r], l);
    let mut request = env.request();
    request.require_name("b/b", None).unwrap();
    operations(&env, &mut request, &[Exp::Remove(&a), Exp::Install(&b)]);
}

#[test]
fn install_one_of_two_alternatives() {
    let mut r = remote();
    let a = add(&mut r, pkg("a/a", "1.0"));
    add(&mut r, pkg("a/a", "1.0"));
    let env = Env::new(vec![r], lock());
    let mut request = env.request();
    request.require_name("a/a", None).unwrap();
    operations(&env, &mut request, &[Exp::Install(&a)]);
}

#[test]
fn install_provider() {
    let mut r = remote();
    add(&mut r, requires(pkg("a/a", "1.0"), &[("b/b", vc(">=", "1.0"))]));
    let mut q = pkg("q/q", "1.0");
    q.provides = vec![link("q/q", "b/b", vc("=", "1.0"), LinkType::Provide)];
    add(&mut r, q);
    let env = Env::new(vec![r], lock());
    let mut request = env.request();
    request.require_name("a/a", None).unwrap();
    // must explicitly pick the provider, so error in this case
    let (_, result, _) = env.solve(&mut request);
    assert!(result.unwrap_err().downcast_ref::<SolverProblems>().is_some());
}

fn replacer_of_b(name: &str) -> komposer_package::Package {
    let mut q = pkg(name, "1.0");
    q.replaces = vec![link(name, "b/b", vc(">=", "1.0"), LinkType::Replace)];
    q
}

#[test]
fn skip_replacer_of_existing_package() {
    let mut r = remote();
    let a = add(&mut r, requires(pkg("a/a", "1.0"), &[("b/b", vc(">=", "1.0"))]));
    add(&mut r, replacer_of_b("q/q"));
    let b = add(&mut r, pkg("b/b", "1.0"));
    let env = Env::new(vec![r], lock());
    let mut request = env.request();
    request.require_name("a/a", None).unwrap();
    operations(&env, &mut request, &[Exp::Install(&b), Exp::Install(&a)]);
}

#[test]
fn no_install_replacer_of_missing_package() {
    let mut r = remote();
    add(&mut r, requires(pkg("a/a", "1.0"), &[("b/b", vc(">=", "1.0"))]));
    add(&mut r, replacer_of_b("q/q"));
    let env = Env::new(vec![r], lock());
    let mut request = env.request();
    request.require_name("a/a", None).unwrap();
    let (_, result, _) = env.solve(&mut request);
    assert!(result.unwrap_err().downcast_ref::<SolverProblems>().is_some());
}

#[test]
fn skip_replaced_package_if_replacer_is_selected() {
    let mut r = remote();
    let a = add(&mut r, requires(pkg("a/a", "1.0"), &[("b/b", vc(">=", "1.0"))]));
    let q = add(&mut r, replacer_of_b("q/q"));
    add(&mut r, pkg("b/b", "1.0"));
    let env = Env::new(vec![r], lock());
    let mut request = env.request();
    request.require_name("a/a", None).unwrap();
    request.require_name("q/q", None).unwrap();
    operations(&env, &mut request, &[Exp::Install(&q), Exp::Install(&a)]);
}

#[test]
fn pick_older_if_newer_conflicts() {
    let mut r = remote();
    let x = add(&mut r, requires(pkg("x/x", "1.0"), &[("a/a", vc(">=", "2.0.0.0")), ("b/b", vc(">=", "2.0.0.0"))]));
    let a = add(&mut r, requires(pkg("a/a", "2.0.0"), &[("b/b", vc(">=", "2.0.0.0"))]));
    // new package A depends on a version of package B that does not exist, so it is not installable
    add(&mut r, requires(pkg("a/a", "2.1.0"), &[("b/b", vc(">=", "2.2.0.0"))]));
    let new_b = add(&mut r, pkg("b/b", "2.1.0"));
    // S replaces both A and B, so S and B or S and A cannot be installed together
    let mut s = pkg("s/s", "2.0.0");
    s.replaces = vec![link("s/s", "a/a", vc(">=", "2.0.0.0"), LinkType::Replace), link("s/s", "b/b", vc(">=", "2.0.0.0"), LinkType::Replace)];
    add(&mut r, s);
    let env = Env::new(vec![r], lock());
    let mut request = env.request();
    request.require_name("x/x", None).unwrap();
    operations(&env, &mut request, &[Exp::Install(&new_b), Exp::Install(&a), Exp::Install(&x)]);
}

#[test]
fn install_circular_require() {
    let mut r = remote();
    let a = add(&mut r, requires(pkg("a/a", "1.0"), &[("b/b", vc(">=", "1.0"))]));
    add(&mut r, pkg("b/b", "0.9"));
    let b2 = add(&mut r, requires(pkg("b/b", "1.1"), &[("a/a", vc(">=", "1.0"))]));
    let env = Env::new(vec![r], lock());
    let mut request = env.request();
    request.require_name("a/a", None).unwrap();
    operations(&env, &mut request, &[Exp::Install(&b2), Exp::Install(&a)]);
}

#[test]
fn install_alternative_with_circular_require() {
    let mut r = remote();
    let a = add(&mut r, requires(pkg("a/a", "1.0"), &[("b/b", vc(">=", "1.0"))]));
    let b = add(&mut r, requires(pkg("b/b", "1.0"), &[("virtual/virtual", vc(">=", "1.0"))]));
    let provider = |name: &str| {
        let mut p = requires(pkg(name, "1.0"), &[("a/a", vc("==", "1.0"))]);
        p.provides = vec![link(name, "virtual/virtual", vc("==", "1.0"), LinkType::Provide)];
        p
    };
    let c = add(&mut r, provider("c/c"));
    add(&mut r, provider("d/d"));
    let env = Env::new(vec![r], lock());
    let mut request = env.request();
    request.require_name("a/a", None).unwrap();
    request.require_name("c/c", None).unwrap();
    operations(&env, &mut request, &[Exp::Install(&b), Exp::Install(&a), Exp::Install(&c)]);
}

/// If a replacer D replaces B and C with C not otherwise available, D must be installed instead of the original B.
#[test]
fn use_replacer_if_necessary() {
    let mut r = remote();
    let a = add(&mut r, requires(pkg("a/a", "1.0"), &[("b/b", vc(">=", "1.0")), ("c/c", vc(">=", "1.0"))]));
    add(&mut r, pkg("b/b", "1.0"));
    let replacer = |version: &str| {
        let mut d = pkg("d/d", version);
        d.replaces = vec![link("d/d", "b/b", vc(">=", "1.0"), LinkType::Replace), link("d/d", "c/c", vc(">=", "1.0"), LinkType::Replace)];
        d
    };
    add(&mut r, replacer("1.0"));
    let d2 = add(&mut r, replacer("1.1"));
    let env = Env::new(vec![r], lock());
    let mut request = env.request();
    request.require_name("a/a", None).unwrap();
    request.require_name("d/d", None).unwrap();
    operations(&env, &mut request, &[Exp::Install(&d2), Exp::Install(&a)]);
}

#[test]
fn issue_265() {
    let mut r = remote();
    add(&mut r, pkg("a/a", "2.0.999999-dev"));
    add(&mut r, pkg("a/a", "2.1-dev"));
    add(&mut r, pkg("a/a", "2.2-dev"));
    add(&mut r, requires(pkg("b/b", "2.0.10"), &[("a/a", vc("==", "2.1.0.0-dev"))]));
    let mut b2 = requires(pkg("b/b", "2.0.9"), &[("a/a", vc("==", "2.1.0.0-dev"))]);
    b2.replaces = vec![link("b/b", "d/d", vc("==", "2.0.9.0"), LinkType::Replace)];
    add(&mut r, b2);
    add(&mut r, requires(pkg("c/c", "2.0-dev"), &[("a/a", vc(">=", "2.0")), ("d/d", vc(">=", "2.0"))]));
    add(&mut r, requires(pkg("d/d", "2.0.9"), &[("a/a", vc(">=", "2.1")), ("b/b", vc(">=", "2.0-dev"))]));
    let env = Env::new(vec![r], lock());
    let mut request = env.request();
    request.require_name("c/c", Some(vc("==", "2.0.0.0-dev"))).unwrap();
    let (_, result, _) = env.solve(&mut request);
    assert!(result.unwrap_err().downcast_ref::<SolverProblems>().is_some());
}

#[test]
fn conflict_result_empty() {
    let mut r = remote();
    let mut a = pkg("a/a", "1.0");
    a.conflicts = vec![link("a/a", "b/b", vc(">=", "1.0"), LinkType::Conflict)];
    add(&mut r, a);
    add(&mut r, pkg("b/b", "1.0"));
    let env = Env::new(vec![r], lock());
    let mut request = env.request();
    request.require_name("a/a", Some(any())).unwrap();
    request.require_name("b/b", Some(any())).unwrap();
    assert_eq!(
        env.problems(&mut request),
        "\n  Problem 1\n    - Root composer.json requires a/a * -> satisfiable by a/a[1.0].\n    - Root composer.json requires b/b * -> satisfiable by b/b[1.0].\n    - a/a 1.0 conflicts with b/b 1.0.\n"
    );
}

#[test]
fn unsatisfiable_requires() {
    let mut r = remote();
    add(&mut r, requires(pkg("a/a", "1.0"), &[("b/b", vc(">=", "2.0"))]));
    add(&mut r, pkg("b/b", "1.0"));
    let env = Env::new(vec![r], lock());
    let mut request = env.request();
    request.require_name("a/a", None).unwrap();
    assert_eq!(
        env.problems(&mut request),
        "\n  Problem 1\n    - Root composer.json requires a/a * -> satisfiable by a/a[1.0].\n    - a/a 1.0 requires b/b >= 2.0 -> found b/b[1.0] but it does not match the constraint.\n"
    );
}

#[test]
fn require_mismatch_exception() {
    let mut r = remote();
    add(&mut r, requires(pkg("a/a", "1.0"), &[("b/b", vc(">=", "1.0"))]));
    add(&mut r, requires(pkg("b/b", "1.0"), &[("c/c", vc(">=", "1.0"))]));
    add(&mut r, pkg("b/b", "0.9"));
    add(&mut r, requires(pkg("c/c", "1.0"), &[("d/d", vc(">=", "1.0"))]));
    add(&mut r, requires(pkg("d/d", "1.0"), &[("b/b", vc("<", "1.0"))]));
    let env = Env::new(vec![r], lock());
    let mut request = env.request();
    request.require_name("a/a", Some(any())).unwrap();
    assert_eq!(
        env.problems(&mut request),
        "\n  Problem 1\n    - Root composer.json requires a/a * -> satisfiable by a/a[1.0].\n    - a/a 1.0 requires b/b >= 1.0 -> satisfiable by b/b[1.0].\n    - b/b 1.0 requires c/c >= 1.0 -> satisfiable by c/c[1.0].\n    - c/c 1.0 requires d/d >= 1.0 -> satisfiable by d/d[1.0].\n    - d/d 1.0 requires b/b < 1.0 -> satisfiable by b/b[0.9].\n    - You can only install one version of a package, so only one of these can be installed: b/b[0.9, 1.0].\n"
    );
}

#[test]
fn learn_literals_with_sorted_rule_literals() {
    let mut r = remote();
    add(&mut r, pkg("twig/twig", "2.0"));
    let twig16 = add(&mut r, pkg("twig/twig", "1.6"));
    add(&mut r, pkg("twig/twig", "1.5"));
    let mut symfony = pkg("symfony/symfony", "2.0");
    symfony.replaces = vec![link("symfony/symfony", "symfony/twig-bridge", vc("==", "2.0"), LinkType::Replace)];
    add(&mut r, symfony);
    let bridge = add(&mut r, requires(pkg("symfony/twig-bridge", "2.0"), &[("twig/twig", vc("<", "2.0"))]));
    let env = Env::new(vec![r], lock());
    let mut request = env.request();
    request.require_name("symfony/twig-bridge", None).unwrap();
    request.require_name("twig/twig", None).unwrap();
    operations(&env, &mut request, &[Exp::Install(&twig16), Exp::Install(&bridge)]);
}

#[test]
fn install_recursive_alias_dependencies() {
    let mut r = remote();
    add(&mut r, pkg("a/a", "1.0"));
    let b = add(&mut r, requires(pkg("b/b", "2.0"), &[("a/a", vc(">=", "2.0"))]));
    let a2 = add(&mut r, requires(pkg("a/a", "2.0"), &[("b/b", vc("==", "2.0"))]));
    let a2_alias = add(&mut r, alias(&a2, "1.1"));
    let env = Env::new(vec![r], lock());
    let mut request = env.request();
    request.require_name("a/a", Some(vc("==", "1.1.0.0"))).unwrap();
    operations(&env, &mut request, &[Exp::Install(&b), Exp::Install(&a2), Exp::MarkAliasInstalled(&a2_alias)]);
}

#[test]
fn install_dev_alias() {
    let mut r = remote();
    let a = add(&mut r, pkg("a/a", "2.0"));
    let b = add(&mut r, requires(pkg("b/b", "1.0"), &[("a/a", vc("<", "2.0"))]));
    let a_alias = add(&mut r, alias(&a, "1.1"));
    let env = Env::new(vec![r], lock());
    let mut request = env.request();
    request.require_name("a/a", Some(vc("==", "2.0"))).unwrap();
    request.require_name("b/b", None).unwrap();
    operations(&env, &mut request, &[Exp::Install(&a), Exp::MarkAliasInstalled(&a_alias), Exp::Install(&b)]);
}

#[test]
fn install_root_aliases_if_alias_of_is_installed() {
    let mut r = remote();
    let root_alias = |r: &mut komposer_package::repository::ArrayRepository, of: &PackageRef| {
        let mut a = alias(of, "1.1");
        a.root_package_alias = true;
        add(r, a)
    };
    // root aliased, required
    let a = add(&mut r, pkg("a/a", "1.0"));
    let a_alias = root_alias(&mut r, &a);
    // root aliased, not required, should still be installed as it is root alias
    let b = add(&mut r, pkg("b/b", "1.0"));
    let b_alias = root_alias(&mut r, &b);
    // regular alias, not required, alias should not be installed
    let c = add(&mut r, pkg("c/c", "1.0"));
    let c_alias = add(&mut r, alias(&c, "1.1"));
    let env = Env::new(vec![r], lock());
    let mut request = env.request();
    request.require_name("a/a", Some(vc("==", "1.1"))).unwrap();
    request.require_name("b/b", Some(vc("==", "1.0"))).unwrap();
    request.require_name("c/c", Some(vc("==", "1.0"))).unwrap();
    operations(
        &env,
        &mut request,
        &[
            Exp::Install(&a),
            Exp::MarkAliasInstalled(&a_alias),
            Exp::Install(&b),
            Exp::MarkAliasInstalled(&b_alias),
            Exp::Install(&c),
            Exp::MarkAliasInstalled(&c_alias),
        ],
    );
}

/// Only a combination this complex makes the solver learn a negatively decided literal as positive assertion.
#[test]
fn learn_positive_literal() {
    let mut r = remote();
    let a = add(&mut r, requires(pkg("a/a", "1.0"), &[("b/b", vc("==", "1.0")), ("c/c", vc(">=", "1.0")), ("d/d", vc("==", "1.0"))]));
    let b = add(&mut r, requires(pkg("b/b", "1.0"), &[("e/e", vc("==", "1.0"))]));
    add(&mut r, requires(pkg("c/c", "1.0"), &[("f/f", vc("==", "1.0"))]));
    let c2 = add(&mut r, requires(pkg("c/c", "2.0"), &[("f/f", vc("==", "1.0")), ("g/g", vc(">=", "1.0"))]));
    let d = add(&mut r, requires(pkg("d/d", "1.0"), &[("f/f", vc(">=", "1.0"))]));
    let e = add(&mut r, requires(pkg("e/e", "1.0"), &[("g/g", vc("<=", "2.0"))]));
    let f1 = add(&mut r, pkg("f/f", "1.0"));
    add(&mut r, pkg("f/f", "2.0"));
    add(&mut r, pkg("g/g", "1.0"));
    let g2 = add(&mut r, pkg("g/g", "2.0"));
    add(&mut r, pkg("g/g", "3.0"));
    let env = Env::new(vec![r], lock());
    let mut request = env.request();
    request.require_name("a/a", None).unwrap();
    let (_, result, learned_positive) = env.solve(&mut request);
    check_operations(
        result.unwrap().operations(),
        &[Exp::Install(&f1), Exp::Install(&d), Exp::Install(&g2), Exp::Install(&c2), Exp::Install(&e), Exp::Install(&b), Exp::Install(&a)],
    );
    // the code path turning a negative literal into a positive learned literal was executed
    assert!(learned_positive);
}

#[test]
fn solve_surfaces_filter_list_removed_locked_packages_as_problems() {
    // the pool only knows the package was removed by a filter list
    let package: PackageRef = Arc::new(pkg("vendor/malware", "1.0.0"));
    let pool = filter_list_removed_pool("vendor/malware", "1.0.0.0");
    let mut request = Request::new();
    request.fix_package(package.clone());
    request.lock_package(package.clone());
    let (policy, io) = (DefaultPolicy::default(), Io::null());
    let mut solver = Solver::new(&policy, &pool, &io);
    let err = solver.solve(&request, &PlatformRequirementFilter::IgnoreNothing).unwrap_err();
    let problems = err.downcast_ref::<SolverProblems>().unwrap();
    assert_eq!(problems.problems.len(), 1);
    let rule = problems.problems[0].reasons().next().unwrap()[0];
    match problems.rules.reason(rule) {
        Reason::LockedFilterListRemoved(p) => assert!(Arc::ptr_eq(p, &package)),
        other => panic!("unexpected reason {other:?}"),
    }
}

/// A pool whose filter list removed `name` in `version`, built through the filter like Composer does.
fn filter_list_removed_pool(name: &str, version: &str) -> Pool {
    use komposer_package::repository::{FilterEntry, LoadRequest, LoadResult, Repository};
    use komposer_solver::{PolicyOptions, PoolOptions, RepositorySet};

    struct Malware(String);
    impl Repository for Malware {
        fn repo_name(&self) -> String {
            "malware list".into()
        }
        fn load_packages(&self, _: &LoadRequest) -> anyhow::Result<LoadResult> {
            Ok(LoadResult::default())
        }
        fn packages(&self) -> anyhow::Result<Vec<PackageRef>> {
            Ok(vec![])
        }
        fn filter_entries(&self, _: &[(String, Constraint)], _: &[String]) -> anyhow::Result<Vec<FilterEntry>> {
            Ok(vec![FilterEntry {
                package_name: self.0.clone(),
                list_name: "malware".into(),
                constraint: Constraint::Any,
                url: None,
                reason: None,
                id: None,
                source: None,
            }])
        }
    }

    let mut locked = repo(RepoKind::Lock);
    let package = locked.add_package(komposer_package::Package::new(name, version, version));
    let locked: Arc<dyn Repository> = Arc::new(locked);
    let set = RepositorySet::new(komposer_semver::Stability::Stable);
    let mut request = Request::with_locked_repository(locked).unwrap();
    request.fix_locked_package(package);
    let malware: Arc<dyn Repository> = Arc::new(Malware(name.into()));
    let options = PoolOptions::for_install(&PolicyOptions::default(), &[malware]);
    let pool = set.create_pool(&mut request, &Io::null(), &options).unwrap();
    assert!(pool.is_empty());
    pool
}
