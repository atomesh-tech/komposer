//! Port of composer/semver plus the bits of Composer's own VersionParser.
//!
//! Versions are passed around as normalized strings (`1.2.3.0`, `1.0.0.0-beta2`,
//! `dev-main`, `9999999-dev`), exactly like Composer does.

mod constraint;
mod intervals;
mod php;
mod version_parser;

#[cfg(test)]
mod tests;

pub use constraint::{Bound, Constraint, Op};
pub use intervals::{BranchSet, Interval, Intervals, compact_constraint, get_intervals, have_intersections, is_subset_of};
pub use php::version_compare;
pub use version_parser::{
    DEFAULT_BRANCH_ALIAS, ParseError, Stability, is_upgrade, is_valid, normalize, normalize_branch, normalize_default_branch, normalize_full,
    normalize_stability, parse_constraints, parse_name_version_pairs, parse_numeric_alias_prefix, parse_stability,
};

/// `Comparator::compare($a, $op, $b)` on normalized versions (dev branches compared as branches).
pub fn compare(a: &str, op: Op, b: &str) -> bool {
    constraint::match_specific(op, b, Op::Eq, a, true)
}

/// `Semver::satisfies($version, $constraints)`. Invalid input gives `false` where PHP throws.
pub fn satisfies(version: &str, constraints: &str) -> bool {
    let (Ok(v), Ok(c)) = (normalize(version), parse_constraints(constraints)) else { return false };
    c.matches(&Constraint::single(Op::Eq, v))
}

/// `Semver::sort()` (ascending) on pretty or normalized versions.
pub fn sort(versions: &mut [String]) {
    usort(versions, false)
}

/// `Semver::rsort()` (descending).
pub fn rsort(versions: &mut [String]) {
    usort(versions, true)
}

/// `Semver::usort()`. Versions that fail to normalize (PHP throws) are compared as given.
fn usort(versions: &mut [String], desc: bool) {
    let normalized: Vec<String> = versions.iter().map(|v| normalize_default_branch(&normalize(v).unwrap_or_else(|_| v.clone()))).collect();
    let order = php::usort(&normalized, |l, r| {
        use std::cmp::Ordering::*;
        let ord = if l == r {
            Equal
        } else if compare(l, Op::Lt, r) {
            Less
        } else {
            Greater
        };
        if desc { ord.reverse() } else { ord }
    });
    let sorted: Vec<String> = order.into_iter().map(|i| std::mem::take(&mut versions[i])).collect();
    for (slot, version) in versions.iter_mut().zip(sorted) {
        *slot = version;
    }
}
