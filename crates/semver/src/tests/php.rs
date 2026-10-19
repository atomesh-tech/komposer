//! PHP engine ports, checked against results from a real PHP 8.4.

use crate::php::{add, loose_eq, usort};
use crate::version_compare;
use std::cmp::Ordering::{Equal, Greater, Less};

#[test]
fn version_compare_cases() {
    let cases = [
        ("1.0-dev", "1.0", Less),
        ("..", "..", Less),
        ("1.0.0", "1.0", Greater),
        ("1.0", "1.0.0", Less),
        ("1.0rc1", "1.0", Less),
        ("1.0pl1", "1.0", Greater),
        ("1.0a", "1.0b", Less),
        ("1.0-alpha", "1.0-a", Equal),
        ("1#2", "1.2", Greater),
        ("#1", "1", Equal),
        ("1.0", "1.0.a", Greater),
        ("1.0.0", "1.0.a", Greater),
        ("1.0.0-dev", "1.0.0-patch", Less),
        ("1.0-foo", "1.0", Less),
        ("1.0-foo", "1.0-dev", Less),
        ("", "1", Less),
        ("", "", Equal),
        ("1", "", Greater),
        ("-1", "1", Less),
        ("1.0.0.0-beta2", "1.0.0.0-beta10", Less),
        ("99999999999999999999", "9223372036854775807", Equal),
        ("dev-foo", "1.0", Less),
        ("1.0.0.0-RC1", "1.0.0.0-rc1", Equal),
        ("1.0_1", "1.0.1", Equal),
        ("1.0+1", "1.0-1", Equal),
        ("1..0", "1.0", Equal),
        ("a.b", "a.c", Greater),
        ("#a", "#b", Equal),
        (".1", "0.1", Less),
        ("1.0p", "1.0.0", Greater),
        // php-src #21689 (2026) drops a trailing empty part; older PHP returned Less
        ("1.0.", "1.0", Equal),
    ];
    for (a, b, expected) in cases {
        assert_eq!(version_compare(a, b), expected, "{a} vs {b}");
    }
}

#[test]
fn numeric_versions_compare_like_the_full_algorithm() {
    let versions =
        ["0", "0.0", "1", "01", "1.0", "1.0.0", "1.0.0.0", "1.2.3.4", "1.9", "1.10", "10.0", "9223372036854775807", "99999999999999999999"];
    for a in versions {
        for b in versions {
            assert_eq!(version_compare(a, b), crate::php::compare(a.as_bytes(), b.as_bytes()), "{a} vs {b}");
        }
    }
}

#[test]
fn loose_eq_cases() {
    let cases = [
        ("20100102.1", "20100102.10", true),
        ("1", "1.0", true),
        ("01", "1", true),
        ("1e3", "1000", true),
        (" 1", "1", true),
        ("1 ", "1", true),
        ("abc", "abc", true),
        ("1.0.0", "1.0", false),
        ("9223372036854775808", "9223372036854775809", false),
        ("9223372036854775807", "9223372036854775808", false),
        ("1.", "1", true),
        (".5", "0.5", true),
        ("1e", "1", false),
    ];
    for (a, b, expected) in cases {
        assert_eq!(loose_eq(a, b), expected, "{a} == {b}");
    }
}

#[test]
fn add_overflows_like_php() {
    assert_eq!(add("007", 1), "8");
    assert_eq!(add("-3", 2), "-1");
    assert_eq!(add("99999999999999999999", 1), "1.0E+20");
    assert_eq!(add("9223372036854775807", 1), "9.2233720368548E+18");
}

#[test]
fn usort_matches_zend_sort_with_inconsistent_comparator() {
    // usort($a, fn($l, $r) => $l === $r ? 0 : 1)
    let gt = |l: &&str, r: &&str| if l == r { Equal } else { Greater };
    let sorted = |v: &[&str]| -> Vec<String> { usort(v, gt).into_iter().map(|i| v[i].to_string()).collect() };
    assert_eq!(sorted(&["dev-b", "dev-a", "dev-c", "1.0", "dev-d"]), ["dev-d", "1.0", "dev-a", "dev-c", "dev-b"]);
}

#[test]
fn usort_is_a_stable_sort() {
    // past 16 elements zend_sort switches to quicksort; keys collide so the tiebreak matters
    let keys: Vec<u32> = (0..2000u32).map(|i| (i * 7919) % 13).collect();
    let order = usort(&keys, |a: &u32, b: &u32| a.cmp(b));
    let mut expected: Vec<usize> = (0..keys.len()).collect();
    expected.sort_by_key(|&i| keys[i]);
    assert_eq!(order, expected);
}
