//! `Filter\PlatformRequirementFilter\*`: `--ignore-platform-req(s)`.

use komposer_package::is_platform_package;
use komposer_semver::{Constraint, Op, get_intervals};
use regex::Regex;
use std::borrow::Cow;

/// `PlatformRequirementFilterInterface` and its three implementations.
#[derive(Clone, Debug, Default)]
pub enum PlatformRequirementFilter {
    /// `IgnoreNothingPlatformRequirementFilter`
    #[default]
    IgnoreNothing,
    /// `IgnoreAllPlatformRequirementFilter`
    IgnoreAll,
    /// `IgnoreListPlatformRequirementFilter`: names with `*` wildcards; a `+` suffix only ignores the upper bound.
    IgnoreList { ignore: Regex, ignore_upper_bound: Regex },
}

impl PlatformRequirementFilter {
    /// `PlatformRequirementFilterFactory::fromBoolOrList()` for a list.
    pub fn ignore_list<S: AsRef<str>>(reqs: &[S]) -> PlatformRequirementFilter {
        let (mut all, mut upper) = (vec![], vec![]);
        for req in reqs {
            match req.as_ref().strip_suffix('+') {
                Some(name) => upper.push(name),
                None => all.push(req.as_ref()),
            }
        }
        PlatformRequirementFilter::IgnoreList { ignore: names_to_regex(&all), ignore_upper_bound: names_to_regex(&upper) }
    }

    /// `isIgnored()`.
    pub fn is_ignored(&self, req: &str) -> bool {
        match self {
            PlatformRequirementFilter::IgnoreNothing => false,
            PlatformRequirementFilter::IgnoreAll => is_platform_package(req),
            PlatformRequirementFilter::IgnoreList { ignore, .. } => is_platform_package(req) && ignore.is_match(req),
        }
    }

    /// `isUpperBoundIgnored()`.
    pub fn is_upper_bound_ignored(&self, req: &str) -> bool {
        match self {
            PlatformRequirementFilter::IgnoreList { ignore_upper_bound, .. } => {
                is_platform_package(req) && (self.is_ignored(req) || ignore_upper_bound.is_match(req))
            }
            _ => self.is_ignored(req),
        }
    }

    /// `IgnoreListPlatformRequirementFilter::filterConstraint()`: drops the upper bound of `+` requirements
    /// (never for conflicts, which pass `allow_upper_bound_override = false`). Other filters return it unchanged.
    pub fn filter_constraint<'a>(&self, req: &str, constraint: &'a Constraint, allow_upper_bound_override: bool) -> Cow<'a, Constraint> {
        let PlatformRequirementFilter::IgnoreList { ignore, ignore_upper_bound } = self else {
            return Cow::Borrowed(constraint);
        };
        if !is_platform_package(req) || !allow_upper_bound_override || !ignore_upper_bound.is_match(req) {
            return Cow::Borrowed(constraint);
        }
        if ignore.is_match(req) {
            return Cow::Owned(Constraint::Any);
        }
        let intervals = get_intervals(constraint);
        match intervals.numeric.last() {
            Some(last) if last.end != Constraint::single(Op::Lt, format!("{}.0.0.0", i64::MAX)) => {
                let Constraint::Single { version, .. } = &last.end else { return Cow::Borrowed(constraint) };
                // a plain `new MultiConstraint()`, not the optimizing `create()`
                Cow::Owned(Constraint::Multi {
                    constraints: vec![constraint.clone(), Constraint::single(Op::Ge, version.clone())],
                    conjunctive: false,
                })
            }
            _ => Cow::Borrowed(constraint),
        }
    }
}

/// `BasePackage::packageNameToRegexp()`: `*` matches anything, case-insensitive, anchored.
pub(crate) fn name_to_regex(pattern: &str) -> Regex {
    names_to_regex(&[pattern])
}

/// `BasePackage::packageNamesToRegexp()`.
pub(crate) fn names_to_regex<S: AsRef<str>>(patterns: &[S]) -> Regex {
    let alternatives: Vec<String> = patterns.iter().map(|p| regex::escape(p.as_ref()).replace(r"\*", ".*")).collect();
    Regex::new(&format!("(?i)^(?:{})$", alternatives.join("|"))).expect("escaped pattern")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignore_list_is_ignored() {
        let cases: &[(&[&str], &str, bool)] = &[
            (&["ext-json", "monolog/monolog"], "ext-json", true),
            (&["ext-json", "monolog/monolog"], "php", false),
            (&["ext-json", "monolog/monolog"], "monolog/monolog", false),
            (&["ext-*"], "ext-json", true),
            (&["ext-*", "php*"], "php", true),
            (&["foo", "*"], "ext-json", true),
            (&["*", "foo"], "php", true),
            (&["*", "monolog/*"], "monolog/monolog", false),
            (&[""], "ext-foo", false),
            (&[], "ext-foo", false),
            (&["ext-", "foo"], "ext-foo", false),
        ];
        for (list, req, expected) in cases {
            assert_eq!(PlatformRequirementFilter::ignore_list(list).is_ignored(req), *expected, "{list:?} {req}");
        }
    }

    #[test]
    fn ignore_list_is_upper_bound_ignored() {
        let cases: &[(&[&str], &str, bool)] = &[
            (&["ext-json", "monolog/monolog"], "ext-json", true),
            (&["ext-json+", "monolog/monolog"], "ext-json", true),
            (&["ext-json+", "monolog/monolog"], "php", false),
            (&["monolog/monolog"], "monolog/monolog", false),
            (&["ext-*+"], "ext-json", true),
            (&["ext-*+", "php*+"], "php", true),
            (&["foo", "*+"], "ext-json", true),
            (&["*+", "foo"], "php", true),
            (&["*+", "monolog/*+"], "monolog/monolog", false),
            (&[""], "ext-foo", false),
            (&[], "ext-foo", false),
            (&["ext-", "foo"], "ext-foo", false),
        ];
        for (list, req, expected) in cases {
            assert_eq!(PlatformRequirementFilter::ignore_list(list).is_upper_bound_ignored(req), *expected, "{list:?} {req}");
        }
    }

    #[test]
    fn ignore_all_and_nothing() {
        let all = PlatformRequirementFilter::IgnoreAll;
        assert!(all.is_ignored("php") && all.is_ignored("ext-json") && !all.is_ignored("monolog/monolog"));
        assert!(all.is_upper_bound_ignored("php") && !all.is_upper_bound_ignored("monolog/monolog"));
        let nothing = PlatformRequirementFilter::IgnoreNothing;
        assert!(!nothing.is_ignored("php") && !nothing.is_upper_bound_ignored("ext-json"));
    }

    #[test]
    fn filter_constraint_drops_upper_bound() {
        let f = PlatformRequirementFilter::ignore_list(&["php+", "ext-json"]);
        let c = komposer_semver::parse_constraints("^7.4").unwrap();
        assert_eq!(
            f.filter_constraint("php", &c, true).into_owned(),
            Constraint::Multi { constraints: vec![c.clone(), Constraint::single(Op::Ge, "8.0.0.0-dev")], conjunctive: false }
        );
        assert_eq!(*f.filter_constraint("php", &c, false), c);
        assert_eq!(*f.filter_constraint("ext-mbstring", &c, true), c);
        let f = PlatformRequirementFilter::ignore_list(&["php", "php+"]);
        assert_eq!(*f.filter_constraint("php", &c, true), Constraint::Any);
    }
}
