//! VersionParserTest (composer/semver) and Composer's Package/Version/VersionParserTest.

use super::{c, parse};
use crate::version_parser::manipulate_version_string;
use crate::*;

#[test]
fn parse_numeric_alias_prefix_cases() {
    let cases = [
        ("0.x-dev", Some("0.")),
        ("1.0.x-dev", Some("1.0.")),
        ("1.x-dev", Some("1.")),
        ("1.2.x-dev", Some("1.2.")),
        ("1.2-dev", Some("1.2.")),
        ("1-dev", Some("1.")),
        ("dev-develop", None),
        ("dev-master", None),
    ];
    for (input, expected) in cases {
        assert_eq!(parse_numeric_alias_prefix(input).as_deref(), expected, "{input}");
    }
}

#[test]
fn is_valid_cases() {
    let cases = [("0.x-dev", true), ("dev-develop", true), ("1.0.2", true), ("1.0.2.5", true), ("1.0.2.5.5", false), ("foo", false)];
    for (input, expected) in cases {
        assert_eq!(is_valid(input), expected, "{input}");
    }
}

#[test]
fn normalize_succeeds() {
    let cases = [
        ("1.0.0", "1.0.0.0"),
        ("1.2.3.4", "1.2.3.4"),
        ("1.0.0RC1dev", "1.0.0.0-RC1-dev"),
        ("1.0.0-rC15-dev", "1.0.0.0-RC15-dev"),
        ("1.0.0.RC.15-dev", "1.0.0.0-RC15-dev"),
        ("1.0.0-rc1", "1.0.0.0-RC1"),
        ("1.0.0.pl3-dev", "1.0.0.0-patch3-dev"),
        ("1.0-dev", "1.0.0.0-dev"),
        ("0", "0.0.0.0"),
        ("99999", "99999.0.0.0"),
        ("10.4.13-beta", "10.4.13.0-beta"),
        ("10.4.13beta2", "10.4.13.0-beta2"),
        ("10.4.13beta.2", "10.4.13.0-beta2"),
        ("v1.13.11-beta.0", "1.13.11.0-beta0"),
        ("1.13.11.0-beta0", "1.13.11.0-beta0"),
        ("10.4.13-b", "10.4.13.0-beta"),
        ("10.4.13-b5", "10.4.13.0-beta5"),
        ("v1.0.0", "1.0.0.0"),
        ("2010.01", "2010.01.0.0"),
        ("2010.01.02", "2010.01.02.0"),
        ("2010.1.555", "2010.1.555.0"),
        ("2010.10.200", "2010.10.200.0"),
        ("20230131.0.0", "20230131.0.0"),
        ("202301310000.0.0", "202301310000.0.0"),
        ("v20100102", "20100102"),
        ("20100102", "20100102"),
        ("20100102.0", "20100102.0"),
        ("20100102.1.0", "20100102.1.0"),
        ("20100102.0.3", "20100102.0.3"),
        ("100000", "100000"),
        ("2010-01-02-10-20-30.0.3", "2010.01.02.10.20.30.0.3"),
        ("2010-01-02-10-20-30.5", "2010.01.02.10.20.30.5"),
        ("2010-01-02", "2010.01.02"),
        ("2012.06.07", "2012.06.07.0"),
        ("2010-01-02.5", "2010.01.02.5"),
        ("2010.1.555", "2010.1.555.0"),
        ("20100102-203040", "20100102.203040"),
        ("20100102.x-dev", "20100102.9999999.9999999.9999999-dev"),
        ("20100102.203040.x-dev", "20100102.203040.9999999.9999999-dev"),
        ("20100102203040-10", "20100102203040.10"),
        ("20100102-203040-p1", "20100102.203040-patch1"),
        ("201903.0", "201903.0"),
        ("201903.x-dev", "201903.9999999.9999999.9999999-dev"),
        ("201903.0-p2", "201903.0-patch2"),
        ("dev-master", "dev-master"),
        ("master", "dev-master"),
        ("dev-trunk", "dev-trunk"),
        ("1.x-dev", "1.9999999.9999999.9999999-dev"),
        ("dev-feature-foo", "dev-feature-foo"),
        ("DEV-FOOBAR", "dev-FOOBAR"),
        ("dev-feature/foo", "dev-feature/foo"),
        ("dev-feature+issue-1", "dev-feature+issue-1"),
        ("dev-master as 1.0.0", "dev-master"),
        ("dev-load-varnish-only-when-used as ^2.0", "dev-load-varnish-only-when-used"),
        ("dev-load-varnish-only-when-used@dev as ^2.0@dev", "dev-load-varnish-only-when-used"),
        ("1.0.0+foo@dev", "1.0.0.0"),
        ("dev-load-varnish-only-when-used@stable", "dev-load-varnish-only-when-used"),
        ("1.0.0-beta.5+foo", "1.0.0.0-beta5"),
        ("1.0.0+foo", "1.0.0.0"),
        ("1.0.0-alpha.3.1+foo", "1.0.0.0-alpha3.1"),
        ("1.0.0-alpha2.1+foo", "1.0.0.0-alpha2.1"),
        ("1.0.0-alpha-2.1-3+foo", "1.0.0.0-alpha2.1-3"),
        ("1.0.0+foo as 2.0", "1.0.0.0"),
        ("00.01.03.04", "00.01.03.04"),
        ("000.001.003.004", "000.001.003.004"),
        ("0.000.103.204", "0.000.103.204"),
        ("0700", "0700.0.0.0"),
        ("041.x-dev", "041.9999999.9999999.9999999-dev"),
        ("dev-041.003", "dev-041.003"),
        ("dev-1.0.0-dev<1.0.5-dev", "dev-1.0.0-dev<1.0.5-dev"),
        ("dev-foo bar", "dev-foo bar"),
        (" 1.0.0", "1.0.0.0"),
        ("1.0.0 ", "1.0.0.0"),
    ];
    for (input, expected) in cases {
        assert_eq!(normalize(input).as_deref(), Ok(expected), "{input}");
    }
}

#[test]
fn normalize_fails() {
    let cases = [
        "",
        "a",
        "1.0.0-meh",
        "1.0.0.0.0",
        "feature-foo",
        "1.0.0+foo bar",
        "1.0.1-SNAPSHOT",
        "1.0.0<1.0.5-dev",
        "1.0.0-dev<1.0.5-dev",
        "foo bar-dev",
        "1.0 .2",
        " as ",
        " as 1.2",
        "^",
        "^8 || ^",
        "~",
        "~1 ~",
        "~1",
        "^1",
        "1.*",
        "20100102.0.3.4",
        "100000.0.0.0",
        "2023013.0.0",
        "202301311.0.0",
        "20230131000.0.0",
        "2023013100000.0.0",
    ];
    for input in cases {
        assert_eq!(normalize(input), Err(ParseError(format!(r#"Invalid version string "{}""#, input.trim()))), "{input}");
    }
}

/// The `{^([^,\s#]+)(?:#[^ ]+)? +as +([^,\s]+)$}` split the alias tests use.
fn split_alias(full: &str) -> (&str, &str) {
    let (source, rest) = full.split_once(' ').unwrap();
    (source, rest.trim_start().strip_prefix("as").unwrap().trim_start())
}

#[test]
fn normalize_fails_and_reports_alias_issue() {
    let cases = ["1.0.0+foo as ^2.0", "1.0.0+foo as  ~2.0", "1.0.0+foo  as >2.0", "1.0.0+foo as <2.0", "1.0.0+foo@dev as <2.0@dev"];
    for full in cases {
        let (source, alias) = split_alias(full);
        assert!(normalize_full(source, Some(full)).is_ok(), "{full}");
        let expected = format!(r#"Invalid version string "{alias}" in "{full}", the alias must be an exact version"#);
        assert_eq!(normalize_full(alias, Some(full)), Err(ParseError(expected)));
    }
}

#[test]
fn normalize_fails_and_reports_aliasee_issue() {
    let cases = ["^2.0 as 1.0.0+foo", "~2.0 as  1.0.0+foo", ">2.0  as 1.0.0+foo", "<2.0 as 1.0.0+foo", "<2.0@dev as 1.2.3@dev"];
    for full in cases {
        let (source, alias) = split_alias(full);
        let expected = format!(
            r#"Invalid version string "{source}" in "{full}", the alias source must be an exact version, if it is a branch name you should prefix it with dev-"#
        );
        assert_eq!(normalize_full(source, Some(full)), Err(ParseError(expected)));
        assert!(normalize_full(alias, Some(full)).is_ok(), "{full}");
    }
}

#[test]
fn normalize_branch_cases() {
    let cases = [
        ("v1.x", "1.9999999.9999999.9999999-dev"),
        ("v1.*", "1.9999999.9999999.9999999-dev"),
        ("v1.0", "1.0.9999999.9999999-dev"),
        ("2.0", "2.0.9999999.9999999-dev"),
        ("v1.0.x", "1.0.9999999.9999999-dev"),
        ("v1.0.3.*", "1.0.3.9999999-dev"),
        ("v2.4.0", "2.4.0.9999999-dev"),
        ("2.4.4", "2.4.4.9999999-dev"),
        ("master", "dev-master"),
        ("trunk", "dev-trunk"),
        ("feature-a", "dev-feature-a"),
        ("FOOBAR", "dev-FOOBAR"),
        ("feature+issue-1", "dev-feature+issue-1"),
    ];
    for (input, expected) in cases {
        assert_eq!(normalize_branch(input), expected, "{input}");
    }
}

#[test]
fn parse_constraints_ignores_stability_flag() {
    assert_eq!(parse("1.0@dev").to_string(), "== 1.0.0.0");
    assert_eq!(parse(">=1.0@beta").to_string(), ">= 1.0.0.0-beta");
    assert_eq!(parse("dev-load-varnish-only-when-used as ^2.0@dev").to_string(), "== dev-load-varnish-only-when-used");
    assert_eq!(parse("dev-load-varnish-only-when-used@dev as ^2.0@dev").to_string(), "== dev-load-varnish-only-when-used");
}

#[test]
fn parse_constraints_ignores_reference_on_dev_version() {
    assert_eq!(parse("1.0.x-dev#abcd123").to_string(), "== 1.0.9999999.9999999-dev");
    assert_eq!(parse("1.0.x-dev#trunk/@123").to_string(), "== 1.0.9999999.9999999-dev");
}

#[test]
fn parse_constraints_fails_on_bad_reference() {
    assert!(parse_constraints("1.0#abcd123").is_err());
    assert!(parse_constraints("1.0#trunk/@123").is_err());
}

#[test]
fn parse_constraints_nudges_ruby_devs_towards_the_path_of_righteousness() {
    let err = parse_constraints("~>1.2").unwrap_err();
    assert_eq!(err.0, r#"Could not parse version constraint ~>1.2: Invalid operator "~>", you probably meant to use the "~" operator"#);
}

#[test]
fn parse_constraints_simple() {
    let cases = [
        ("*", "*"),
        ("v*", ">= 0.0.0.0-dev"),
        ("*.*", ">= 0.0.0.0-dev"),
        ("v*.*", ">= 0.0.0.0-dev"),
        ("*.x.*", ">= 0.0.0.0-dev"),
        ("x.X.x.*", ">= 0.0.0.0-dev"),
        ("<>1.0.0", "!= 1.0.0.0"),
        ("!=1.0.0", "!= 1.0.0.0"),
        (">1.0.0", "> 1.0.0.0"),
        ("<1.2.3.4", "< 1.2.3.4-dev"),
        ("<=1.2.3", "<= 1.2.3.0"),
        (">=1.2.3", ">= 1.2.3.0-dev"),
        ("=1.2.3", "== 1.2.3.0"),
        ("==1.2.3", "== 1.2.3.0"),
        ("1.2.3", "== 1.2.3.0"),
        ("=1.0", "== 1.0.0.0"),
        ("1.2.3b5", "== 1.2.3.0-beta5"),
        ("1.2.3a1", "== 1.2.3.0-alpha1"),
        ("1.2.3p1234", "== 1.2.3.0-patch1234"),
        ("1.2.3pl1234", "== 1.2.3.0-patch1234"),
        (">= 1.2.3", ">= 1.2.3.0-dev"),
        ("< 1.2.3", "< 1.2.3.0-dev"),
        ("> 1.2.3", "> 1.2.3.0"),
        (">=dev-master", ">= dev-master"),
        ("dev-master", "== dev-master"),
        ("dev-feature-a", "== dev-feature-a"),
        ("dev-some-fix", "== dev-some-fix"),
        ("dev-CAPS", "== dev-CAPS"),
        ("dev-master as 1.0.0", "== dev-master"),
        ("<1.2.3.4-stable", "< 1.2.3.4"),
        (">=1.2.3.4-stable", ">= 1.2.3.4"),
        ("<1.2.3-beta1", "< 1.2.3.0-beta1"),
        (">=1.2.3-alpha1", ">= 1.2.3.0-alpha1"),
        ("<1.2.3-RC1", "< 1.2.3.0-RC1"),
        (">=1.2.3-RC1", ">= 1.2.3.0-RC1"),
        (">=1.2.3-rc1", ">= 1.2.3.0-RC1"),
    ];
    for (input, expected) in cases {
        assert_eq!(parse(input).to_string(), expected, "{input}");
    }
}

/// `(string) ($min ? new MultiConstraint([$min, $max]) : $max)`.
fn assert_range(cases: &[(&str, Option<&str>, &str)]) {
    for &(input, min, max) in cases {
        let expected = min.map_or(max.to_string(), |min| format!("[{min} {max}]"));
        assert_eq!(parse(input).to_string(), expected, "{input}");
    }
}

#[test]
fn parse_constraints_wildcard() {
    assert_range(&[
        ("v2.*", Some(">= 2.0.0.0-dev"), "< 3.0.0.0-dev"),
        ("2.*.*", Some(">= 2.0.0.0-dev"), "< 3.0.0.0-dev"),
        ("20.*", Some(">= 20.0.0.0-dev"), "< 21.0.0.0-dev"),
        ("20.*.*", Some(">= 20.0.0.0-dev"), "< 21.0.0.0-dev"),
        ("2.0.*", Some(">= 2.0.0.0-dev"), "< 2.1.0.0-dev"),
        ("2.x", Some(">= 2.0.0.0-dev"), "< 3.0.0.0-dev"),
        ("2.x.x", Some(">= 2.0.0.0-dev"), "< 3.0.0.0-dev"),
        ("2.2.x", Some(">= 2.2.0.0-dev"), "< 2.3.0.0-dev"),
        ("2.10.X", Some(">= 2.10.0.0-dev"), "< 2.11.0.0-dev"),
        ("2.1.3.*", Some(">= 2.1.3.0-dev"), "< 2.1.4.0-dev"),
        ("0.*", None, "< 1.0.0.0-dev"),
        ("0.*.*", None, "< 1.0.0.0-dev"),
        ("0.x", None, "< 1.0.0.0-dev"),
        ("0.x.x", None, "< 1.0.0.0-dev"),
    ]);
}

#[test]
fn parse_tilde_wildcard() {
    assert_range(&[
        ("~v1", Some(">= 1.0.0.0-dev"), "< 2.0.0.0-dev"),
        ("~1.0", Some(">= 1.0.0.0-dev"), "< 2.0.0.0-dev"),
        ("~1.0.0", Some(">= 1.0.0.0-dev"), "< 1.1.0.0-dev"),
        ("~1.2", Some(">= 1.2.0.0-dev"), "< 2.0.0.0-dev"),
        ("~1.2.3", Some(">= 1.2.3.0-dev"), "< 1.3.0.0-dev"),
        ("~1.2.3.4", Some(">= 1.2.3.4-dev"), "< 1.2.4.0-dev"),
        ("~1.2-beta", Some(">= 1.2.0.0-beta"), "< 2.0.0.0-dev"),
        ("~1.2-b2", Some(">= 1.2.0.0-beta2"), "< 2.0.0.0-dev"),
        ("~1.2-BETA2", Some(">= 1.2.0.0-beta2"), "< 2.0.0.0-dev"),
        ("~1.2.2-dev", Some(">= 1.2.2.0-dev"), "< 1.3.0.0-dev"),
        ("~1.2.2-stable", Some(">= 1.2.2.0"), "< 1.3.0.0-dev"),
        ("~201903.0", Some(">= 201903.0-dev"), "< 201904.0.0.0-dev"),
        ("~201903.0-beta", Some(">= 201903.0-beta"), "< 201904.0.0.0-dev"),
        ("~201903.0-stable", Some(">= 201903.0"), "< 201904.0.0.0-dev"),
        ("~201903.205830.1-stable", Some(">= 201903.205830.1"), "< 201903.205831.0.0-dev"),
        ("~2.x-dev", Some(">= 2.9999999.9999999.9999999-dev"), "< 3.0.0.0-dev"),
        ("~2.0.x-dev", Some(">= 2.0.9999999.9999999-dev"), "< 2.1.0.0-dev"),
        ("~2.0.3.x-dev", Some(">= 2.0.3.9999999-dev"), "< 2.0.4.0-dev"),
        ("~0.x-dev", Some(">= 0.9999999.9999999.9999999-dev"), "< 1.0.0.0-dev"),
    ]);
}

#[test]
fn parse_caret_wildcard() {
    assert_range(&[
        ("^v1", Some(">= 1.0.0.0-dev"), "< 2.0.0.0-dev"),
        ("^0", Some(">= 0.0.0.0-dev"), "< 1.0.0.0-dev"),
        ("^0.0", Some(">= 0.0.0.0-dev"), "< 0.1.0.0-dev"),
        ("^1.2", Some(">= 1.2.0.0-dev"), "< 2.0.0.0-dev"),
        ("^1.2.3-beta.2", Some(">= 1.2.3.0-beta2"), "< 2.0.0.0-dev"),
        ("^1.2.3.4", Some(">= 1.2.3.4-dev"), "< 2.0.0.0-dev"),
        ("^1.2.3", Some(">= 1.2.3.0-dev"), "< 2.0.0.0-dev"),
        ("^0.2.3", Some(">= 0.2.3.0-dev"), "< 0.3.0.0-dev"),
        ("^0.2", Some(">= 0.2.0.0-dev"), "< 0.3.0.0-dev"),
        ("^0.2.0", Some(">= 0.2.0.0-dev"), "< 0.3.0.0-dev"),
        ("^0.0.3", Some(">= 0.0.3.0-dev"), "< 0.0.4.0-dev"),
        ("^0.0.3-alpha", Some(">= 0.0.3.0-alpha"), "< 0.0.4.0-dev"),
        ("^0.0.3-dev", Some(">= 0.0.3.0-dev"), "< 0.0.4.0-dev"),
        ("^0.0.3-stable", Some(">= 0.0.3.0"), "< 0.0.4.0-dev"),
        ("^201903.0", Some(">= 201903.0-dev"), "< 201904.0.0.0-dev"),
        ("^201903.0-beta", Some(">= 201903.0-beta"), "< 201904.0.0.0-dev"),
        ("^201903.205830.1-stable", Some(">= 201903.205830.1"), "< 201904.0.0.0-dev"),
        ("^2.x-dev", Some(">= 2.9999999.9999999.9999999-dev"), "< 3.0.0.0-dev"),
        ("^2.0.*-dev", Some(">= 2.0.9999999.9999999-dev"), "< 3.0.0.0-dev"),
        ("^2.0.x-dev", Some(">= 2.0.9999999.9999999-dev"), "< 3.0.0.0-dev"),
        ("^2.0.3.x-dev", Some(">= 2.0.3.9999999-dev"), "< 3.0.0.0-dev"),
        ("^0.x-dev", Some(">= 0.9999999.9999999.9999999-dev"), "< 1.0.0.0-dev"),
    ]);
}

#[test]
fn parse_hyphen() {
    assert_range(&[
        ("v1 - v2", Some(">= 1.0.0.0-dev"), "< 3.0.0.0-dev"),
        ("1.2.3 - 2.3.4.5", Some(">= 1.2.3.0-dev"), "<= 2.3.4.5"),
        ("1.2-beta - 2.3", Some(">= 1.2.0.0-beta"), "< 2.4.0.0-dev"),
        ("1.2-beta - 2.3-dev", Some(">= 1.2.0.0-beta"), "<= 2.3.0.0-dev"),
        ("1.2-RC - 2.3.1", Some(">= 1.2.0.0-RC"), "<= 2.3.1.0"),
        ("1.2.3-alpha - 2.3-RC", Some(">= 1.2.3.0-alpha"), "<= 2.3.0.0-RC"),
        ("1 - 2.0", Some(">= 1.0.0.0-dev"), "< 2.1.0.0-dev"),
        ("1 - 2.1", Some(">= 1.0.0.0-dev"), "< 2.2.0.0-dev"),
        ("1.2 - 2.1.0", Some(">= 1.2.0.0-dev"), "<= 2.1.0.0"),
        ("1.3 - 2.1.3", Some(">= 1.3.0.0-dev"), "<= 2.1.3.0"),
        ("2.0.3.x-dev - 3.0.3.x-dev", Some(">= 2.0.3.9999999-dev"), "<= 3.0.3.9999999-dev"),
        ("2.0.x-dev - 3.0.x-dev", Some(">= 2.0.9999999.9999999-dev"), "<= 3.0.9999999.9999999-dev"),
        ("2.x-dev - 3.x-dev", Some(">= 2.9999999.9999999.9999999-dev"), "<= 3.9999999.9999999.9999999-dev"),
        ("0.x-dev - 1.x-dev", Some(">= 0.9999999.9999999.9999999-dev"), "<= 1.9999999.9999999.9999999-dev"),
    ]);
}

#[test]
fn parse_constraints_strings() {
    let cases = [
        ("3.x-dev", "== 3.9999999.9999999.9999999-dev"),
        ("3-dev", "== 3.0.0.0-dev"),
        ("dev-3.x", "== dev-3.x"),
        ("xsd2php-dev", "== dev-xsd2php"),
        ("3.next-dev", "== dev-3.next"),
        ("foobar-dev", "== dev-foobar"),
        ("dev-xsd2php", "== dev-xsd2php"),
        ("dev-3.next", "== dev-3.next"),
        ("dev-foobar", "== dev-foobar"),
        ("dev-1.0.0-dev<1.0.5-dev", "== dev-1.0.0-dev<1.0.5-dev"),
        ("dev-1.0.0-dev<1.0.5", "== dev-1.0.0-dev<1.0.5"),
        ("foobar-dev as 2.1.0", "== dev-foobar"),
        ("foobar-dev as 2.1.0 || 3.5", "[== dev-foobar || == 3.5.0.0]"),
        ("foobar-dev as 2.1.0 || 3.5 as 1.5", "[== dev-foobar || == 3.5.0.0]"),
        ("2.1.0 - 2.3-dev", "[>= 2.1.0.0-dev <= 2.3.0.0-dev]"),
        ("1.0 - 2.0.x-dev", "[>= 1.0.0.0-dev <= 2.0.9999999.9999999-dev]"),
        // borked typo constraints but so common historically that we gotta keep them working
        ("^1.", "[>= 1.0.0.0-dev < 2.0.0.0-dev]"),
        ("~1.", "[>= 1.0.0.0-dev < 2.0.0.0-dev]"),
        ("1.2.", "== 1.2.0.0"),
        ("1.2..dev", "== 1.2.0.0-dev"),
        ("1.2-.dev", "== 1.2.0.0-dev"),
        ("1.2_-dev", "== 1.2.0.0-dev"),
        // complex constraints
        ("~2.5.9|~2.6,>=2.6.2", "[[>= 2.5.9.0-dev < 2.6.0.0-dev] || [>= 2.6.0.0-dev < 3.0.0.0-dev >= 2.6.2.0-dev]]"),
    ];
    for (input, expected) in cases {
        assert_eq!(parse(input).to_string(), expected, "{input}");
    }
}

#[test]
fn parse_constraints_multi() {
    let cases = [
        ">2.0,<=3.0",
        ">2.0 <=3.0",
        ">2.0  <=3.0",
        ">2.0, <=3.0",
        ">2.0 ,<=3.0",
        ">2.0 , <=3.0",
        ">2.0   , <=3.0",
        "> 2.0   <=  3.0",
        "> 2.0  ,  <=  3.0",
        "  > 2.0  ,  <=  3.0 ",
    ];
    for input in cases {
        assert_eq!(parse(input).to_string(), "[> 2.0.0.0 <= 3.0.0.0]", "{input}");
    }
}

#[test]
fn parse_constraints_multi_with_stability_suffix() {
    assert_eq!(parse(">=1.1.0-alpha4,<1.2.x-dev").to_string(), "[>= 1.1.0.0-alpha4 < 1.2.9999999.9999999-dev]");
    assert_eq!(parse(">=1.1.0-alpha4,<1.2-beta2").to_string(), "[>= 1.1.0.0-alpha4 < 1.2.0.0-beta2]");
}

#[test]
fn parse_constraints_multi_disjunctive_has_prio_over_conjuctive() {
    for input in [">2.0,<2.0.5 | >2.0.6", ">2.0,<2.0.5 || >2.0.6", "> 2.0 , <2.0.5 | >  2.0.6"] {
        assert_eq!(parse(input).to_string(), "[[> 2.0.0.0 < 2.0.5.0-dev] || > 2.0.6.0]", "{input}");
    }
}

#[test]
fn parse_constraints_multi_with_stabilities() {
    assert_eq!(parse(">2.0@stable,<=3.0@dev").to_string(), "[> 2.0.0.0 <= 3.0.0.0-dev]");
    assert_eq!(parse(">2.0@stable,@dev").to_string(), "[> 2.0.0.0 *]");
    assert_eq!(parse(">2.0@stable || 0@dev").to_string(), "[> 2.0.0.0 || == 0.0.0.0]");
}

#[test]
fn parse_constraints_fails() {
    let cases = [
        "",
        "1.0.0-meh",
        ">2.0,,<=3.0",
        ">2.0 ,, <=3.0",
        ">2.0 ||| <=3.0",
        ",^1@dev || ^4@dev",
        ",^1@dev",
        "|| ^1@dev",
        "^1@dev ||",
        "^1@dev ,",
        "^2.0.*",
        "^2.0.x",
        "^2.0.x-beta",
        "^2.*",
        "^2.x",
        "^2.x-beta",
        "^2.1.2.*",
        "^2.1.2.x",
        "^2.1.2.x-beta",
        "~2.0.*",
        "~2.0.x",
        "~2.0.x-beta",
        "~2.*",
        "~2.x",
        "~2.x-beta",
        "~2.1.2.*",
        "~2.1.2.x",
        "~2.1.2.x-beta",
        "1.x - 2.*",
        "2.x.x.x-dev - 3.x.x.x-dev",
        "^1.*-beta-dev",
        "^1. *-dev",
        "~1.*-beta-dev",
        "1.0.0-dev<1.0.5-dev",
        "*-dev",
        "^",
        "^8 || ^",
        "~",
        "~1 ~",
    ];
    for input in cases {
        assert!(parse_constraints(input).is_err(), "{input}");
    }
}

#[test]
fn parse_constraints_error_messages() {
    let err = |s| parse_constraints(s).unwrap_err().0;
    assert_eq!(err(""), r#"Could not parse version constraint : Invalid version string """#);
    assert_eq!(err("1.0.0-meh"), r#"Could not parse version constraint 1.0.0-meh: Invalid version string "1.0.0-meh""#);
    assert_eq!(err("~1 ~"), r#"Could not parse version constraint ~: Invalid version string "~""#);
}

#[test]
fn parse_stability_cases() {
    use Stability::*;
    let cases = [
        (Stable, "1"),
        (Stable, "1.0"),
        (Stable, "3.2.1"),
        (Stable, "v3.2.1"),
        (Dev, "v2.0.x-dev"),
        (Dev, "v2.0.x-dev#abc123"),
        (Dev, "v2.0.x-dev#trunk/@123"),
        (RC, "3.0-RC2"),
        (Dev, "dev-master"),
        (Dev, "3.1.2-dev"),
        (Dev, "dev-feature+issue-1"),
        (Stable, "3.1.2-p1"),
        (Stable, "3.1.2-pl2"),
        (Stable, "3.1.2-patch"),
        (Alpha, "3.1.2-alpha5"),
        (Beta, "3.1.2-beta"),
        (Beta, "2.0B1"),
        (Alpha, "1.2.0a1"),
        (Alpha, "1.2_a1"),
        (RC, "2.0.0rc1"),
        (Alpha, "1.0.0-alpha11+cs-1.1.0"),
        (Dev, "1-2_dev"),
    ];
    for (expected, input) in cases {
        assert_eq!(parse_stability(input), expected, "{input}");
    }
}

#[test]
fn normalize_stability_cases() {
    assert_eq!(normalize_stability("rc"), Ok(Stability::RC));
    assert_eq!(normalize_stability("BeTa"), Ok(Stability::Beta));
    assert_eq!(normalize_stability("Foo").unwrap_err().0, r#"Invalid stability string "foo", expected one of stable, RC, beta, alpha or dev"#);
}

#[test]
fn manipulate_version_string_with_return_null() {
    assert_eq!(manipulate_version_string(["-3", "-2", "-5", "-9"], 1, 2), None);
}

#[test]
fn complex_conjunctive() {
    let parsed = parse("~0.1 || ~1.0 !=1.0.1");
    assert!(!parsed.matches(&c("=", "1.0.1.0")), r#""~0.1 || ~1.0 !=1.0.1" should not allow version "1.0.1.0""#);
}

/// Composer's VersionParserTest.
#[test]
fn parse_name_version_pairs_cases() {
    let pairs = |input: &[&str]| -> Vec<(String, Option<String>)> {
        parse_name_version_pairs(&input.iter().map(|s| s.to_string()).collect::<Vec<_>>(), |name| name.starts_with("ext-") || name == "php")
    };
    let expect = |expected: &[(&str, Option<&str>)]| -> Vec<(String, Option<String>)> {
        expected.iter().map(|(n, v)| (n.to_string(), v.map(String::from))).collect()
    };
    assert_eq!(pairs(&["php:^7.0"]), expect(&[("php", Some("^7.0"))]));
    assert_eq!(pairs(&["php", "^7.0"]), expect(&[("php", Some("^7.0"))]));
    assert_eq!(pairs(&["php", "ext-apcu"]), expect(&[("php", None), ("ext-apcu", None)]));
    assert_eq!(pairs(&["foo/*", "bar*", "acme/baz", "*@dev"]), expect(&[("foo/*", None), ("bar*", None), ("acme/baz", Some("*@dev"))]));
    assert_eq!(pairs(&["php", "*"]), expect(&[("php", Some("*"))]));
}

#[test]
fn is_upgrade_cases() {
    let cases = [
        ("0.9.0.0", "1.0.0.0", true),
        ("1.0.0.0", "0.9.0.0", false),
        ("1.0.0.0", DEFAULT_BRANCH_ALIAS, true),
        (DEFAULT_BRANCH_ALIAS, DEFAULT_BRANCH_ALIAS, true),
        (DEFAULT_BRANCH_ALIAS, "1.0.0.0", false),
        ("1.0.0.0", "dev-foo", true),
        ("dev-foo", "dev-foo", true),
        ("dev-foo", "1.0.0.0", true),
    ];
    for (from, to, expected) in cases {
        assert_eq!(is_upgrade(from, to), expected, "{from} -> {to}");
    }
}
