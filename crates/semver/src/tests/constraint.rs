//! ConstraintTest, MultiConstraintTest, MatchAllConstraintTest and MatchNoneConstraintTest.

use super::{and, c, or, parse};
use crate::*;

/// `matchCompiled()`: `CompilingMatcher::match($constraint, $operator, $version)`.
fn compiled(constraint: &Constraint, op: &str, version: &str) -> bool {
    constraint.compiled_match(Op::parse(op).unwrap(), version)
}

fn is_ranged_branch(op: &str, version: &str) -> bool {
    version.starts_with("dev-") && op != "==" && op != "!="
}

#[test]
fn display() {
    assert_eq!(c("==", "1").to_string(), "== 1");
    assert_eq!(and(vec![c(">", "1.0"), c("<", "1.2")]).to_string(), "[> 1.0 < 1.2]");
    assert_eq!(Constraint::Any.to_string(), "*");
    assert_eq!(Constraint::None.to_string(), "[]");
}

#[test]
fn version_match_succeeds() {
    #[rustfmt::skip]
    let cases = [
        ("==", "2", "==", "2"), ("==", "2", "<", "3"), ("==", "2", "<=", "2"), ("==", "2", "<=", "3"),
        ("==", "2", ">=", "1"), ("==", "2", ">=", "2"), ("==", "2", ">", "1"), ("==", "2", "!=", "1"),
        ("==", "2", "!=", "3"),
        ("<", "2", "==", "1"), ("<", "2", "<", "1"), ("<", "2", "<", "2"), ("<", "2", "<", "3"),
        ("<", "2", "<=", "1"), ("<", "2", "<=", "2"), ("<", "2", "<=", "3"), ("<", "2", ">=", "1"),
        ("<", "2", ">", "1"), ("<", "2", "!=", "1"), ("<", "2", "!=", "2"), ("<", "2", "!=", "3"),
        ("<=", "2", "==", "1"), ("<=", "2", "==", "2"), ("<=", "2", "<", "1"), ("<=", "2", "<", "2"),
        ("<=", "2", "<", "3"), ("<=", "2", "<=", "1"), ("<=", "2", "<=", "2"), ("<=", "2", "<=", "3"),
        ("<=", "2", ">=", "1"), ("<=", "2", ">=", "2"), ("<=", "2", ">", "1"), ("<=", "2", "!=", "1"),
        ("<=", "2", "!=", "2"), ("<=", "2", "!=", "3"),
        (">=", "2", "==", "2"), (">=", "2", "==", "3"), (">=", "2", "<", "3"), (">=", "2", "<=", "2"),
        (">=", "2", "<=", "3"), (">=", "2", ">=", "1"), (">=", "2", ">=", "2"), (">=", "2", ">=", "3"),
        (">=", "2", ">", "1"), (">=", "2", ">", "2"), (">=", "2", ">", "3"), (">=", "2", "!=", "1"),
        (">=", "2", "!=", "2"), (">=", "2", "!=", "3"),
        (">", "2", "==", "3"), (">", "2", "<", "3"), (">", "2", "<=", "3"), (">", "2", ">=", "1"),
        (">", "2", ">=", "2"), (">", "2", ">=", "3"), (">", "2", ">", "1"), (">", "2", ">", "2"),
        (">", "2", ">", "3"), (">", "2", "!=", "1"), (">", "2", "!=", "2"), (">", "2", "!=", "3"),
        ("!=", "2", "!=", "1"), ("!=", "2", "!=", "2"), ("!=", "2", "!=", "3"), ("!=", "2", "==", "1"),
        ("!=", "2", "==", "3"), ("!=", "2", "<", "1"), ("!=", "2", "<", "2"), ("!=", "2", "<", "3"),
        ("!=", "2", "<=", "1"), ("!=", "2", "<=", "2"), ("!=", "2", "<=", "3"), ("!=", "2", ">=", "1"),
        ("!=", "2", ">=", "2"), ("!=", "2", ">=", "3"), ("!=", "2", ">", "1"), ("!=", "2", ">", "2"),
        ("!=", "2", ">", "3"),
        // branch names
        ("==", "dev-foo-bar", "==", "dev-foo-bar"), ("==", "dev-events+issue-17", "==", "dev-events+issue-17"),
        ("==", "dev-foo-bar", "!=", "dev-foo-xyz"), ("!=", "dev-foo-bar", "!=", "dev-foo-xyz"),
        // numbers vs branches
        ("==", "0.12", "!=", "dev-foo"), ("<", "0.12", "!=", "dev-foo"), ("<=", "0.12", "!=", "dev-foo"),
        (">=", "0.12", "!=", "dev-foo"), (">", "0.12", "!=", "dev-foo"), ("!=", "0.12", "==", "dev-foo"),
        ("!=", "0.12", "!=", "dev-foo"),
    ];
    for (rop, rv, pop, pv) in cases {
        let (require, provide) = (c(rop, rv), c(pop, pv));
        let name = format!("{require} / {provide}");
        assert!(require.matches(&provide), "{name}");
        assert!(compiled(&require, pop, pv), "{name}");
        assert!(have_intersections(&require, &provide), "{name}");
        assert!(compact_constraint(&require).matches(&compact_constraint(&provide)), "{name}");
        // the operation should be commutative
        assert!(provide.matches(&require), "{name}");
        assert!(compiled(&provide, rop, rv), "{name}");
        assert!(have_intersections(&provide, &require), "{name}");
        assert!(compact_constraint(&provide).matches(&compact_constraint(&require)), "{name}");
    }
}

#[test]
fn version_match_fails() {
    #[rustfmt::skip]
    let mut cases = vec![
        ("==", "2", "==", "1"), ("==", "2", "==", "3"), ("==", "2", "<", "1"), ("==", "2", "<", "2"),
        ("==", "2", "<=", "1"), ("==", "2", ">=", "3"), ("==", "2", ">", "2"), ("==", "2", ">", "3"),
        ("==", "2", "!=", "2"),
        ("<", "2", "==", "2"), ("<", "2", "==", "3"), ("<", "2", ">=", "2"), ("<", "2", ">=", "3"),
        ("<", "2", ">", "2"), ("<", "2", ">", "3"),
        ("<=", "2", "==", "3"), ("<=", "2", ">=", "3"), ("<=", "2", ">", "2"), ("<=", "2", ">", "3"),
        (">=", "2", "==", "1"), (">=", "2", "<", "1"), (">=", "2", "<", "2"), (">=", "2", "<=", "1"),
        (">", "2", "==", "1"), (">", "2", "==", "2"), (">", "2", "<", "1"), (">", "2", "<", "2"),
        (">", "2", "<=", "1"), (">", "2", "<=", "2"),
        ("!=", "2", "==", "2"),
        ("==", "2.0-b2", "<", "2.0-beta2"),
        ("==", "dev-foo-dist", "==", "dev-foo-zist"),
        // different branch names
        ("==", "dev-foo-bar", "==", "dev-foo-xyz"), ("==", "dev-foo-bar", "<", "dev-foo-xyz"),
        ("==", "dev-foo-bar", "<=", "dev-foo-xyz"), ("==", "dev-foo-bar", ">=", "dev-foo-xyz"),
        ("==", "dev-foo-bar", ">", "dev-foo-xyz"),
    ];
    let ops = ["==", "<", "<=", ">=", ">", "!="];
    // different branch names, then same branch names, with every ranged require operator
    for pv in ["dev-foo-xyz", "dev-foo-bar"] {
        for rop in ["<", "<=", ">=", ">"] {
            for pop in ops {
                cases.push((rop, "dev-foo-bar", pop, pv));
            }
        }
    }
    for pop in ["<", "<=", ">=", ">", "!="] {
        cases.push(("==", "dev-foo-bar", pop, "dev-foo-bar"));
    }
    // branch vs number, not comparable so mostly false
    for rop in ["==", "<", "<=", ">=", ">"] {
        for pop in ["==", "<", "<=", ">=", ">"] {
            cases.push((rop, "0.12", pop, "dev-foo"));
        }
    }
    for pop in ["<", "<=", ">=", ">"] {
        cases.push(("!=", "0.12", pop, "dev-foo"));
    }

    for (rop, rv, pop, pv) in cases {
        let (require, provide) = (c(rop, rv), c(pop, pv));
        let name = format!("{require} / {provide}");
        assert!(!require.matches(&provide), "{name}");
        assert!(!compiled(&require, pop, pv), "{name}");
        assert!(!compact_constraint(&require).matches(&compact_constraint(&provide)), "{name}");
        // the operation should be commutative
        assert!(!provide.matches(&require), "{name}");
        assert!(!compiled(&provide, rop, rv), "{name}");
        assert!(!compact_constraint(&provide).matches(&compact_constraint(&require)), "{name}");
        // do not test intersections with >/</>=/<= for dev versions as these are not supported
        if is_ranged_branch(rop, rv) || is_ranged_branch(pop, pv) {
            continue;
        }
        assert!(!have_intersections(&require, &provide), "{name}");
        assert!(!have_intersections(&provide, &require), "{name}");
    }
}

#[test]
fn comparable_branches() {
    let provide = c("==", "dev-foo");
    for (op, compare_branches) in [(">", false), ("<", true)] {
        let require = c(op, "0.12");
        assert!(!require.matches(&provide));
        assert!(!compiled(&require, "==", "dev-foo"));
        assert!(!have_intersections(&require, &provide));
        assert!(!compact_constraint(&require).matches(&compact_constraint(&provide)));
        // `$require->matchSpecific($provide, true)` is `Comparator::compare('dev-foo', $op, '0.12')`
        assert_eq!(compare("dev-foo", Op::parse(op).unwrap(), "0.12"), compare_branches);
    }
}

#[test]
fn invalid_operators() {
    for op in ["invalid", "!", "equals"] {
        assert_eq!(Op::parse(op), None);
    }
}

fn bound(version: &str, inclusive: bool) -> Bound {
    Bound { version: version.into(), inclusive }
}

#[test]
fn bounds() {
    let (zero, inf) = (Bound::zero(), Bound::positive_infinity());
    let cases = [
        ("==", "1.0.0.0", bound("1.0.0.0", true), bound("1.0.0.0", true)),
        ("==", "1.0.0.0-rc3", bound("1.0.0.0-rc3", true), bound("1.0.0.0-rc3", true)),
        (">=", "dev-feature-branch", zero.clone(), inf.clone()),
        ("<", "0.0.4.0", zero.clone(), bound("0.0.4.0", false)),
        ("<", "1.0.0.0", zero.clone(), bound("1.0.0.0", false)),
        ("<", "2.0.0.0", zero.clone(), bound("2.0.0.0", false)),
        ("<", "3.0.3.0", zero.clone(), bound("3.0.3.0", false)),
        ("<", "3.0.3.0-rc3", zero.clone(), bound("3.0.3.0-rc3", false)),
        ("<", "dev-feature-branch", zero.clone(), inf.clone()),
        (">", "0.0.4.0", bound("0.0.4.0", false), inf.clone()),
        (">", "1.0.0.0", bound("1.0.0.0", false), inf.clone()),
        (">", "2.0.0.0", bound("2.0.0.0", false), inf.clone()),
        (">", "3.0.3.0", bound("3.0.3.0", false), inf.clone()),
        (">", "3.0.3.0-rc3", bound("3.0.3.0-rc3", false), inf.clone()),
        (">", "dev-feature-branch", zero.clone(), inf.clone()),
        ("<=", "0.0.4.0", zero.clone(), bound("0.0.4.0", true)),
        ("<=", "1.0.0.0", zero.clone(), bound("1.0.0.0", true)),
        ("<=", "2.0.0.0", zero.clone(), bound("2.0.0.0", true)),
        ("<=", "3.0.3.0", zero.clone(), bound("3.0.3.0", true)),
        ("<=", "3.0.3.0-rc3", zero.clone(), bound("3.0.3.0-rc3", true)),
        ("<=", "dev-feature-branch", zero.clone(), inf.clone()),
        (">=", "0.0.4.0", bound("0.0.4.0", true), inf.clone()),
        (">=", "1.0.0.0", bound("1.0.0.0", true), inf.clone()),
        (">=", "2.0.0.0", bound("2.0.0.0", true), inf.clone()),
        (">=", "3.0.3.0", bound("3.0.3.0", true), inf.clone()),
        (">=", "3.0.3.0-rc3", bound("3.0.3.0-rc3", true), inf.clone()),
        (">=", "dev-feature-branch", zero.clone(), inf.clone()),
        ("<>", "1.0.0.0", zero.clone(), inf.clone()),
    ];
    for (op, version, lower, upper) in cases {
        let constraint = c(op, version);
        assert_eq!(constraint.lower_bound(), lower, "{constraint}");
        assert_eq!(constraint.upper_bound(), upper, "{constraint}");
    }
}

#[test]
fn compile_matrix() {
    let versions = ["1.0", "2.0", "dev-master", "dev-foo", "3.0-b2", "3.0-beta2"];
    let ops = ["==", "!=", ">", "<", ">=", "<="];
    for rv in versions {
        for rop in ops {
            for pv in versions {
                for pop in ops {
                    let (require, provide) = (c(rop, rv), c(pop, pv));
                    let name = format!("{require} / {provide}");
                    // Asserts Compiled version returns the same result than standard
                    let m = require.matches(&provide);
                    assert_eq!(compiled(&require, pop, pv), m, "{name}");
                    assert_eq!(compact_constraint(&require).matches(&compact_constraint(&provide)), m, "{name}");
                    assert_eq!(compiled(&compact_constraint(&require), pop, pv), m, "{name}");
                    // do not test >/</>=/<= for dev versions as these are not supported
                    if is_ranged_branch(rop, rv) || is_ranged_branch(pop, pv) {
                        continue;
                    }
                    assert_eq!(have_intersections(&require, &provide), m, "{name}");
                }
            }
        }
    }
}

// MultiConstraintTest

fn require_range(conjunctive: bool) -> Constraint {
    Constraint::Multi { constraints: vec![c(">", "1.0"), c("<", "1.2")], conjunctive }
}

/// The match, intersection and compacted-match assertions MultiConstraintTest repeats, both ways round.
fn assert_multi(require: &Constraint, provide: &Constraint, expected: bool) {
    assert_eq!(require.matches(provide), expected, "{require} / {provide}");
    assert_eq!(provide.matches(require), expected, "{provide} / {require}");
    assert_eq!(have_intersections(require, provide), expected, "{require} / {provide}");
    assert_eq!(compact_constraint(require).matches(&compact_constraint(provide)), expected);
    assert_eq!(compact_constraint(provide).matches(&compact_constraint(require)), expected);
}

#[test]
fn multi_version_match_succeeds() {
    let require = require_range(true);
    assert_multi(&require, &c("==", "1.1"), true);
    assert!(compiled(&require, "==", "1.1"));
}

#[test]
fn multi_version_provided_match_succeeds() {
    assert_multi(&require_range(true), &and(vec![c(">=", "1.1"), c("<", "2.0")]), true);
}

#[test]
fn multi_version_match_succeeds_inside_foreach_loop() {
    assert_multi(&require_range(false), &or(vec![c(">", "1.0"), c("<", "1.2")]), true);
}

#[test]
fn conjunctive_matches_disjunctive_false() {
    assert_multi(&require_range(true), &or(vec![c("<", "1.0"), c(">", "2.0")]), false);
}

#[test]
fn multi_version_match_fails() {
    let require = require_range(true);
    assert_multi(&require, &c("==", "1.2"), false);
    assert!(!compiled(&require, "==", "1.2"));
}

#[test]
fn multi_bounds() {
    let gt_ge_gt = || vec![c(">", "1.0.0.0"), c(">=", "1.0.0.0"), c(">", "1.0.0.0")];
    let cases = [
        (and(vec![c("==", "1.0.0.0"), c("==", "1.0.0.0")]), bound("1.0.0.0", true), bound("1.0.0.0", true)),
        // ">" should take precedence ">=" for lower bound when conjunctive
        (and(gt_ge_gt()), bound("1.0.0.0", false), Bound::positive_infinity()),
        // ">=" should take precedence ">" for lower bound when disjunctive
        (or(gt_ge_gt()), bound("1.0.0.0", true), Bound::positive_infinity()),
        // Bounds should be limited when conjunctive
        (and(vec![c(">=", "7.0.0.0"), c("<", "8.0.0.0")]), bound("7.0.0.0", true), bound("8.0.0.0", false)),
        // Bounds should be unlimited when disjunctive
        (or(vec![c(">=", "7.0.0.0"), c("<", "8.0.0.0")]), Bound::zero(), Bound::positive_infinity()),
    ];
    for (constraint, lower, upper) in cases {
        assert_eq!(constraint.lower_bound(), lower, "{constraint}");
        assert_eq!(constraint.upper_bound(), upper, "{constraint}");
    }
}

#[test]
fn bounds_integration_with_version_parser() {
    let cases = [
        ("^7.0", bound("7.0.0.0-dev", true), bound("8.0.0.0-dev", false)),
        ("^7.2", bound("7.2.0.0-dev", true), bound("8.0.0.0-dev", false)),
        ("7.4.*", bound("7.4.0.0-dev", true), bound("7.5.0.0-dev", false)),
        ("7.2.* || 7.4.*", bound("7.2.0.0-dev", true), bound("7.5.0.0-dev", false)),
    ];
    for (input, lower, upper) in cases {
        let constraint = parse(input);
        assert_eq!(constraint.lower_bound(), lower, "{input}");
        assert_eq!(constraint.upper_bound(), upper, "{input}");
    }
}

#[test]
fn multiple_multi_constraints_merging() {
    let constraint = and(["^7.0", "^7.2", "7.4.*", "7.2.* || 7.4.*"].map(parse).to_vec());
    assert_eq!(constraint.lower_bound(), bound("7.4.0.0-dev", true));
    assert_eq!(constraint.upper_bound(), bound("7.5.0.0-dev", false));
}

#[test]
fn multiple_multi_constraints_merging_with_gaps() {
    let constraint = and(vec![parse("^7.1.15 || ^7.2.3"), parse("^7.2.2")]);
    assert_eq!(constraint.lower_bound(), bound("7.2.2.0-dev", true));
    assert_eq!(constraint.upper_bound(), bound("8.0.0.0-dev", false));
}

#[test]
fn creates_match_all_constraint_if_none_given() {
    assert_eq!(Constraint::multi(vec![], true), Constraint::Any);
}

#[test]
fn match_all_constraint_within_multi_constraint() {
    let conjunctive = vec![c(">=", "2.5.0.0-dev"), c("<=", "3.0.0.0-dev"), Constraint::Any];
    assert_eq!(Constraint::multi(conjunctive, true).to_string(), "[>= 2.5.0.0-dev <= 3.0.0.0-dev *]");
    let disjunctive = vec![c(">=", "2.5.0.0-dev"), Constraint::Any];
    assert_eq!(Constraint::multi(disjunctive, false).to_string(), "[>= 2.5.0.0-dev || *]");
}

#[test]
fn multi_constraint_optimizations() {
    let range = |from: &str, to: &str| and(vec![c(">=", from), c("<", to)]);
    let cases = [
        ("^2.5 || ^3.0", range("2.5.0.0-dev", "4.0.0.0-dev")),
        ("^2.5 || ^3.0 || ^4.0", range("2.5.0.0-dev", "5.0.0.0-dev")),
        (
            "~2.5.9 || ~2.6, >=2.6.2",
            or(vec![range("2.5.9.0-dev", "2.6.0.0-dev"), and(vec![c(">=", "2.6.0.0-dev"), c("<", "3.0.0.0-dev"), c(">=", "2.6.2.0-dev")])]),
        ),
        (
            "^1.0 || ^2.0 !=2.0.1 || ^3.0 || ^4.0",
            or(vec![
                range("1.0.0.0-dev", "2.0.0.0-dev"),
                and(vec![c(">=", "2.0.0.0-dev"), c("<", "3.0.0.0-dev"), c("!=", "2.0.1.0")]),
                range("3.0.0.0-dev", "5.0.0.0-dev"),
            ]),
        ),
        (
            "^1.0 != 1.0.1 || ^2.0 !=2.0.1 || ^3.0 || ^4.0 != 4.0.1",
            or(vec![
                and(vec![c(">=", "1.0.0.0-dev"), c("<", "2.0.0.0-dev"), c("!=", "1.0.1.0")]),
                and(vec![c(">=", "2.0.0.0-dev"), c("<", "3.0.0.0-dev"), c("!=", "2.0.1.0")]),
                range("3.0.0.0-dev", "4.0.0.0-dev"),
                and(vec![c(">=", "4.0.0.0-dev"), c("<", "5.0.0.0-dev"), c("!=", "4.0.1.0")]),
            ]),
        ),
        (
            "~0.1 || ~1.0 !=1.0.1",
            or(vec![range("0.1.0.0-dev", "1.0.0.0-dev"), and(vec![c(">=", "1.0.0.0-dev"), c("<", "2.0.0.0-dev"), c("!=", "1.0.1.0")])]),
        ),
        ("^0.2 || ^1.0", or(vec![range("0.2.0.0-dev", "0.3.0.0-dev"), range("1.0.0.0-dev", "2.0.0.0-dev")])),
        ("^0.1 || ^1.0 || ^2.0", or(vec![range("0.1.0.0-dev", "0.2.0.0-dev"), range("1.0.0.0-dev", "3.0.0.0-dev")])),
        ("^1.0 || 2.1 || ^3.0", or(vec![range("1.0.0.0-dev", "2.0.0.0-dev"), c("=", "2.1.0.0"), range("3.0.0.0-dev", "4.0.0.0-dev")])),
    ];
    for (input, expected) in cases {
        assert_eq!(parse(input).to_string(), expected.to_string(), "{input}");
    }
}

#[test]
fn multi_constraint_not_conjunctive_fill_with_false() {
    let provide = c("==", "1.1");
    // both always false
    let require = or(vec![c(">", "dev-foo"), c(">", "dev-bar")]);
    assert!(!require.matches(&provide));
    assert!(!provide.matches(&require));
    assert!(!compiled(&require, "==", "1.1"));
    assert!(!have_intersections(&require, &provide));
}

#[test]
fn multi_constraint_conjunctive_fill_with_true() {
    let provide = c("!=", "1.1");
    // both always true
    let require = and(vec![c("!=", "dev-foo"), c("!=", "dev-bar")]);
    assert!(require.matches(&provide));
    assert!(provide.matches(&require));
    assert!(compiled(&require, "!=", "1.1"));
    assert!(have_intersections(&require, &provide));
}

// MatchAllConstraintTest / MatchNoneConstraintTest

#[test]
fn match_all_and_none() {
    assert!(Constraint::Any.matches(&c("==", "1.1")));
    for provide in [c("==", "1.1"), c("!=", "1.1"), c("==", "dev-foo"), c("!=", "dev-foo")] {
        assert!(!Constraint::None.matches(&provide), "{provide}");
    }
}
