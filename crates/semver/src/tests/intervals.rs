//! IntervalsTest and SubsetsTest.

use super::{and, c, or, parse};
use crate::*;

const ZERO: &str = ">= 0.0.0.0-dev";
const INF: &str = "< 9223372036854775807.0.0.0";

/// Builds an `Intervals::get()` result from `"op version"` border strings.
fn iv(numeric: &[(&str, &str)], names: &[&str], exclude: bool) -> Intervals {
    let border = |s: &str| {
        let (op, version) = s.split_once(' ').unwrap();
        c(op, version)
    };
    Intervals {
        numeric: numeric.iter().map(|&(start, end)| Interval { start: border(start), end: border(end) }).collect(),
        branches: BranchSet { names: names.iter().map(|n| n.to_string()).collect(), exclude },
    }
}

fn any() -> Intervals {
    iv(&[(ZERO, INF)], &[], true)
}

fn any_nodev() -> Intervals {
    iv(&[(ZERO, INF)], &[], false)
}

fn none() -> Intervals {
    iv(&[], &[], false)
}

/// `new MultiConstraint(array_map(parseConstraints, $parts), $conjunctive)`.
fn multi_of(parts: &[&str], conjunctive: bool) -> Constraint {
    Constraint::Multi { constraints: parts.iter().map(|p| parse(p)).collect(), conjunctive }
}

#[test]
fn compact_constraint_cases() {
    // "" stands for MatchNoneConstraint
    let cases: &[(&str, &[&str], bool)] = &[
        ("1.0 - 1.2 || ^1.5", &["1.0 - 1.2 || ^1.5", "1.8 - 1.9 || ^1.12"], false),
        ("1.8 - 1.9 || ^1.12", &["1.0 - 1.2 || ^1.5", "1.8 - 1.9 || ^1.12"], true),
        ("1.8 - 1.9 || ^1.12 || dev-master || dev-foo", &["1.8 - 1.9 || ^1.12", "dev-master", "dev-foo"], false),
        ("", &["1.8 - 1.9 || ^1.12", "dev-master", "dev-foo"], true),
        ("", &["1.0", "2.0"], true),
        ("1.0 || 2.0", &["1.0", "2.0"], false),
        ("!= 1.2, != 1.6", &["!= 1.2", "!= 1.6"], true),
        ("!= 1.0, != 2.0", &["!= 1.0", "!= 2.0"], true),
        (">1.5, != 2.0", &["!= 2.0", "> 1.5"], true),
        ("!= 1.0, != 2.0", &["!= 1.0", "!= 2.0"], true),
        ("!= 1.0", &["!= 1.0", "!= 1.0"], false),
        ("*", &["!= 1.0", "!= 1.0", "!= dev-foo", "1.0.5.*"], false),
        ("< 2.0.0.0-stable || > 2.0.0", &["> 1.0.0 != 2.0.0", "< 1.9.0"], false),
        ("< 2.0.0.0-stable || > 2.0.0 < 3.0.0.0-stable || > 3.0.0", &["> 1.0.0 != 2.0.0 != 3.0.0", "< 1.5"], false),
        ("< 2.0.0.0-stable || > 2.0.0 < 3.0.0.0-stable || > 3.0.0 || dev-bar", &["> 1.0.0 != 2.0.0 != 3.0.0", "< 1.5", "dev-bar"], false),
        ("1.0.5.*", &["!= 1.0", "!= 1.0", "!= dev-foo", "1.0.5.*"], true),
        (">= 1.0-dev, != 1.2-stable, <2", &["!= 1.2", "!= dev-foo", "!= dev-bar", "1.*"], true),
        ("!= 1.2, != dev-foo, != dev-bar", &["!= 1.2", "!= dev-foo", "!= dev-bar"], true),
        ("*", &["!= 1.2", "!= dev-foo", "!= dev-bar"], false),
        ("== dev-foo", &["!= 1.2", "== dev-foo", "!= dev-bar"], true),
        ("*", &["!= 1.0", "!= 1.0", "!= dev-foo", "1.0.5.*", "== dev-bla"], false),
        ("dev-bla", &["!= 1.0", "!= 1.0", "!= dev-foo", "== dev-bla"], true),
        ("", &["!= 1.0", "!= 1.0", "!= dev-foo", "1.0.5.*", "== dev-bla"], true),
        ("!= dev-master, != dev-foo", &["!= dev-master", "!= dev-foo"], true),
        ("== dev-foo", &["!= dev-master", "== dev-foo"], true),
        ("!= dev-master", &["!= dev-master", "== dev-foo"], false),
        ("> 5", &["!= dev-master", "!= dev-foo", "> 5"], true),
        ("!= dev-foo", &["!= dev-foo", "!= dev-foo"], true),
        ("!= 3-stable, <5 || >=6, <9", &["!= 3, <5", ">=6, <9"], false),
        ("== dev-foo || == dev-bar", &["!= 1.0", "!= 2.0", "==dev-foo || ==dev-bar"], true),
    ];
    for &(expected, parts, conjunctive) in cases {
        let expected = if expected.is_empty() { Constraint::None } else { parse(expected) };
        assert_eq!(compact_constraint(&multi_of(parts, conjunctive)).to_string(), expected.to_string(), "{parts:?}");
    }
}

#[test]
fn compact_constraint_keeps_matched_set() {
    let cases: &[(&[&str], bool)] = &[
        (&["> 1.0.0 != 2.0.0", "< 1.9.0"], false),
        (&["> 1.0.0 != 2.0.0 != 3.0.0", "< 1.5"], false),
        (&["> 1.0.0 != 2.0.0", "< 1.5", "dev-bar"], false),
        (&["> 1.0.0 != 2.0.0 != 3.0.0", "< 1.5", "dev-bar"], false),
        (&["> 1.0.0 != 2.0.0 < 3.0.0", "< 1.5", ">= 5.0"], false),
        (&["!= 2.0", "!= 3.0"], true),
        (&["!= 2.0", "!= dev-foo"], true),
        (&["!= 2.0", "dev-bar"], false),
    ];
    let dev = c("==", "dev-foo");
    for &(parts, conjunctive) in cases {
        let constraint = multi_of(parts, conjunctive);
        let compacted = compact_constraint(&constraint);
        assert_eq!(constraint.matches(&dev), compacted.matches(&dev), "{compacted}");
        assert!(is_subset_of(&compacted, &constraint), "{compacted}");
        assert!(is_subset_of(&constraint, &compacted), "{compacted}");
    }
}

#[test]
fn get_intervals_from_strings() {
    let cases = [
        (iv(&[(">= 1.0.0.0-dev", "< 2.0.0.0-dev")], &[], false), "^1.0"),
        (iv(&[("> 1.0.0.0", INF)], &[], false), "> 1.0"),
        (
            iv(
                &[(">= 0.9.0.0-dev", "< 1.0.0.0-dev"), (">= 1.2.3.0", "<= 1.2.3.0"), (">= 1.3.4.0", "<= 1.3.4.0"), ("> 2.3.0.0", "< 2.5.0.0-dev")],
                &[],
                false,
            ),
            "1.3.4 || 1.2.3 || >2.3,<2.5 || <1,>=0.9",
        ),
        (iv(&[(">= 1.0.0.0-dev", "< 2.0.0.0-dev"), (">= 3.0.0.0-dev", "< 5.0.0.0-dev")], &[], false), "^4.0 || ^1.0 || ^3.0"),
        (iv(&[(">= 4.0.0.0-dev", INF)], &[], false), "^4.0 || >= 5"),
        (iv(&[(ZERO, "< 6.0.0.0-dev")], &[], false), ">= 5,< 6 || < 5"),
        (any_nodev(), ">= 5 || < 5"),
        (iv(&[(">= 1.1.0.0-dev", "< 2.0.0.0-dev"), (">= 3.0.0.0-dev", "< 5.0.0.0-dev")], &[], false), "^4.0 || ^1.1 || ^3.0 || ^1.2"),
        (iv(&[(">= 1.0.0.0-dev", "< 1.5.0.0-dev")], &[], false), "1.2 - 1.4 || 1.0 - 1.3"),
        (iv(&[(">= 4.0.0.0-dev", INF)], &[], false), "^4.0 || >= 4.5"),
        (iv(&[(ZERO, "< 6.0.0.0-dev")], &[], false), ">= 5,< 6 || < 5.3"),
        (any_nodev(), ">= 5 || <= 5"),
        (iv(&[(">= 1.0.0.0-dev", "< 2.0.0.0-dev")], &[], false), "^1.0 || ^1.0"),
        (iv(&[(ZERO, "< 2.0.0.0-dev")], &[], false), "< 2.0 || < 1.2"),
        (any_nodev(), "< 2.0 || >= 1"),
        (iv(&[(">= 1.0.0.0-dev", "< 2.0.0.0-dev")], &[], false), "< 2.0, >= 1"),
        (none(), "^1.0, ^2.0"),
        (none(), "^1.0, ^3.0"),
        (none(), "== 1.0, != 1.0"),
        (none(), "> 1.0, dev-master"),
        (iv(&[("> 5.0.0.0", INF)], &[], false), "!= dev-master, != dev-foo, > 5"),
        (iv(&[("> 5.0.0.0", "< 6.0.0.0"), ("> 6.0.0.0", INF)], &[], false), "!= 6, > 5"),
        (iv(&[(ZERO, INF)], &["dev-master", "dev-foo"], true), "!= dev-master, != dev-foo || > 5"),
        (iv(&[(">= 1.2.0.0-dev", "< 2.0.0.0-dev")], &[], false), "^1.0, ^1.2"),
        (iv(&[(">= 1.5.0.0-dev", "< 1.7.0.0-dev")], &[], false), "^1.0, ^1.2, 1.4 - 1.8, 1.5 - 1.6, 1.5 - 2"),
        (iv(&[(">= 1.5.0.0-dev", "< 3.0.0.0-dev")], &[], false), "1.5 - 2"),
        (
            iv(&[(">= 1.0.0.0-dev", "< 1.2.3.0"), ("> 1.2.3.0", "< 1.4.5.0"), ("> 1.4.5.0", "< 2.0.0.0-dev")], &[], false),
            "!= 1.4.5, ^1.0, != 1.2.3, != 2.3, != dev-foo, != dev-master",
        ),
        (none(), "!= 1.4.5, ^1.0, != 1.2.3, != 2.3, == dev-foo, == dev-foo"),
        (iv(&[], &["dev-foo"], false), "!= 1.2.3, != 2.3, == dev-foo, == dev-foo"),
        (iv(&[(ZERO, INF)], &["dev-foo"], true), "^1.0 || != dev-foo"),
        (any(), "!= 1.4.5 || ^1.0 || != dev-foo || != dev-master || == dev-master"),
        (any(), "^1.0 || != dev-master || == dev-master"),
        (iv(&[(ZERO, INF)], &["dev-foo"], true), "^1.0 || != dev-foo || == dev-master"),
        (iv(&[(">= 1.0.0.0-dev", "< 2.0.0.0-dev")], &["dev-foo", "dev-master"], false), "^1.0 || == dev-foo || == dev-master || == dev-master"),
        (none(), "dev-master, ^1.0"),
        (none(), "dev-master, dev-foo"),
        (iv(&[], &["dev-master"], false), "dev-master, dev-master"),
        (iv(&[(ZERO, INF)], &["dev-master"], true), "!= dev-master, != dev-master"),
        (iv(&[(ZERO, INF)], &["dev-master", "dev-foo"], true), "!= dev-master, != dev-foo"),
        (iv(&[(">= 1.0.0.0-dev", "< 2.0.0.0-dev")], &["dev-master", "dev-foo"], false), "dev-master || ^1.0 || dev-foo || dev-master"),
        (iv(&[(">= 1.0.0.0-dev", "< 2.0.0.0-dev")], &[], false), "^1.0, *"),
        (any(), "^1.0 || *"),
        (any(), "*, *"),
        (any(), "!= dev-foo || *"),
        (iv(&[("> 5.0.0.0", INF)], &[], false), "> 5, *"),
        (iv(&[("> 5.0.0.0", INF)], &[], false), "!= dev-foo, > 5"),
        (iv(&[(ZERO, INF)], &["dev-foo"], true), "!= dev-foo, != dev-foo"),
        (iv(&[(ZERO, INF)], &["dev-foo", "dev-bar"], true), "!= dev-foo, != dev-bar"),
        (iv(&[], &["dev-bar"], false), "!= dev-foo, == dev-bar"),
        (none(), "dev-foo, > 5"),
        (none(), "!= 1.0, != 1.0, != dev-foo, 1.0.5.*, == dev-bla"),
        (iv(&[(ZERO, INF)], &["dev-master", "dev-foo"], true), "!= dev-master, != dev-foo"),
        (iv(&[(ZERO, INF)], &["dev-foo"], true), "!= dev-foo, != dev-bar || != dev-foo"),
        (iv(&[(ZERO, INF)], &["dev-foo", "dev-bar"], true), "!= dev-foo, != dev-bar || != dev-foo, != dev-bar"),
        (iv(&[], &["dev-foo", "dev-bar"], false), "== dev-foo || == dev-bar"),
        (any(), "== dev-foo || != dev-foo"),
        (iv(&[(ZERO, INF)], &["dev-bar"], true), "== dev-foo || != dev-bar"),
        (iv(&[(ZERO, INF)], &["dev-bar"], true), "== dev-foo || != dev-bar || != dev-bar"),
        (any(), "== dev-foo || != dev-bar || != dev-foo"),
        (none(), "< 0.0.0"),
    ];
    for (expected, input) in cases {
        assert_eq!(*get_intervals(&parse(input)), expected, "{input}");
    }
}

#[test]
fn get_intervals_from_constraints() {
    let range = |from: &str, to: &str| and(vec![c(">=", from), c("<", to)]);
    let cases = [
        (
            iv(&[(">= 1.5.0.0-dev", "< 1.7.0.0-dev")], &[], false),
            and(vec![
                range("1.0.0.0-dev", "2.0.0.0-dev"),
                range("1.2.0.0-dev", "2.0.0.0-dev"),
                range("1.4.0.0-dev", "1.9.0.0-dev"),
                range("1.5.0.0-dev", "1.7.0.0-dev"),
                range("1.5.0.0-dev", "3.0.0.0-dev"),
            ]),
        ),
        (
            iv(&[(">= 1.8.0.0-dev", "< 1.10.0.0-dev"), (">= 1.12.0.0-dev", "< 2.0.0.0-dev")], &[], false),
            and(vec![
                or(vec![range("1.0.0.0-dev", "1.3.0.0-dev"), range("1.5.0.0-dev", "2.0.0.0-dev")]),
                or(vec![range("1.8.0.0-dev", "1.10.0.0-dev"), range("1.12.0.0-dev", "2.0.0.0-dev")]),
            ]),
        ),
        (
            iv(&[(">= 1.3.2.0-dev", "<= 1.3.2.0-dev")], &[], false),
            and(vec![or(vec![c("==", "1.3.1.0-dev"), c("==", "1.3.2.0-dev"), c("==", "1.3.3.0-dev")]), c("==", "1.3.2.0-dev")]),
        ),
        (any_nodev(), and(vec![c(">=", "0.0.0.0-dev"), c("<", "9223372036854775807.0.0.0")])),
        (any(), or(vec![c("!=", "dev-foo"), c("!=", "dev-bar")])),
        (none(), Constraint::None),
        (none(), and(vec![or(vec![c("==", "1.3.1.0-dev"), c("==", "1.3.2.0-dev"), c("==", "1.3.3.0-dev")]), Constraint::None])),
    ];
    for (expected, constraint) in cases {
        assert_eq!(*get_intervals(&constraint), expected, "{constraint}");
    }
}

// SubsetsTest

#[test]
fn is_subset_of_cases() {
    let cases = [
        ("*", "*"),
        ("*", "!= 1 || == 1"),
        ("1.0.0", "*"),
        ("1.0.*", "*"),
        ("^1.0 || ^2.0", "*"),
        ("^3.0", "^3.2 || *"),
        ("^1.0 || ^2.0", "^1.0 || ^2.0"),
        ("^1.0 || ^2.0", "^1.0 || ^2.0 || ^4.0"),
        ("^1.0 || ^2.1", "^1.0 || ^2.1 || ^4.0"),
        ("^1.2", "^1.0 || ^2.0"),
        ("1.2.3", "^1.0 || ^2.0"),
        ("2.0.0-dev", "^1.0 || ^2.0"),
        (">= 2.1.0", ">= 2.0.0"),
        ("^2.0", "<3.0.0"),
        ("^3.0", "> 2.1.3"),
        ("3.0.0", "<= 3.0.0"),
        ("!= 3.0.0", "*"),
        ("!= 3.0.0", "!= 3.0"),
        ("!= 3.0, != 2.0", "!= 2.0, != 3.0"),
        (">3", "^2 || ^3 || >=4"),
        (">3", ">=3"),
        ("<3", "<=3"),
        ("= dev-foo", "= dev-foo"),
        ("!= dev-foo", "!= dev-foo"),
        // invalid range matches nothing so is a subset of any other
        ("< dev-foo", "= dev-foo"),
        // empty numeric range matches nothing so is a subset of itself
        ("< 0.0.0", "< 0.0.0"),
        ("< 0.0.0", "^1.0"),
        ("1.5.*", "^1.4"),
        ("1.5.*", "1.3 - 1.6 || 1.8 - 1.9"),
        ("1.3.2", "1.3.0 || 1.3.1 || 1.3.2"),
        ("1.3.1", "1.3.0 || 1.3.1 || 1.3.2"),
        ("1.3.1 || 1.3.1", "1.3.1"),
        ("^1.0 || ^3.2", "^1.0 || ^3.0"),
        ("^1.3 || ^3.2", ">1.2"),
        ("^1.6", "<1.3 || >1.5"),
        (">1.6", "<1.3 || >1.5"),
        (">1.6", ">1.5, >1.4, !=1.1"),
        (">1.6", ">1.5 || >1.7"),
        ("^1.1", "> 1.0.0"),
        ("^1.1, !=1.5.0", "> 1.0.0"),
        ("^1.1, !=0.5.0", "> 1.0.0"),
        ("^2.0 || dev-foo", "> 1.0 || dev-foo || dev-bar"),
        ("^1.0, ^1.2", ">=1.2"),
        ("^1.0, ^1.2", "^1.2"),
        ("^1.0, ^1.2 || ^1.3", "^1.2"),
    ];
    for (a, b) in cases {
        assert!(is_subset_of(&parse(a), &parse(b)), "{a} should be seen as a subset of {b}");
    }
}

#[test]
fn is_not_subset_of_cases() {
    let cases = [
        // it is a subset of the numeric interval, but * allows dev- branches while the latter does not
        ("*", ">= 1 || < 1"),
        ("*", "1.0.0"),
        ("*", "1.0.*"),
        ("*", "^1.0 || ^2.0"),
        // buggy constraint on the right here, checking it does not match
        ("^1.0 || ^2.0", "^1.0, ^2.0"),
        ("^1.0 || ^2.0", "^1.2"),
        ("^1.0 || ^2.0", "^1.0"),
        ("^1.0 || ^2.0", "1.2.3"),
        ("^1.0 || ^3.0", "1.2.3"),
        ("3.0.0", "^1.0 || ^2.0"),
        ("3.0.0", "< 3.0.0"),
        ("3.0.0", ">= 3.0.1"),
        // it is a subset of the numeric interval, but != x allows dev- branches while the right side does not
        ("!= 3.0.0", "> 3.0.0 || < 3.0.0-stable"),
        ("!= 3.0.0-dev", "^2.0 || <2 || >3.0-dev"),
        ("!= 3.0.0", "= 3.0.0"),
        ("!= 3.0.0", "!= 3.0.1"),
        ("!= 3.0.0", "dev-foo || dev-bar"),
        ("!= 3.0.0", "<dev-foo || >dev-bar"),
        (">= 1.0.0", "= 1.2.3"),
        ("< 2.0.0", "= 1.2.3"),
        (">3", "^2 || ^3 || >4"),
        (">=3", ">3"),
        ("<=3", "<3"),
        ("^2.1", "^2.0, !=2.1.3"),
        ("<2.0", ">=1.1"),
        ("!= dev-foo", "!= dev-bar"),
        ("!= dev-foo", "= dev-bar"),
        ("1.3.3", "1.3.0 || 1.3.1 || 1.3.2"),
        ("1.3.1 || 1.3.2", "1.3.1"),
        (">1.6", ">1.5, >1.4, !=1.7"),
        (">1.6", ">1.5, >1.7"),
        ("^1.0 || ^3.2", "^1.2 || ^3.0"),
        ("^1.0 || ^3.2", "^3.0"),
        ("^1.3 || ^3.2", ">1.4"),
        ("^2.0 || dev-foo", "> 1.0 || dev-bar"),
    ];
    for (a, b) in cases {
        assert!(!is_subset_of(&parse(a), &parse(b)), "{a} should not be seen as a subset of {b}");
    }
}

#[test]
fn match_none_is_no_subset_nor_superset_except_of_match_all() {
    for input in ["1.0.0", "^1.0", ">3", "<3", "dev-foo", "!= 1", "!= dev-foo", "<= dev-foo"] {
        let constraint = parse(input);
        assert!(!is_subset_of(&constraint, &Constraint::None), "{input}");
        assert!(!is_subset_of(&Constraint::None, &constraint), "{input}");
    }
    assert!(!is_subset_of(&Constraint::Any, &Constraint::None));
    assert!(is_subset_of(&Constraint::None, &Constraint::Any));
}
