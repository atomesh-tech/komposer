use crate::php::{self, version_compare};
use std::cmp::Ordering::{self, Equal, Greater, Less};
use std::fmt;

/// `PHP_INT_MAX.'.0.0.0'`, the version of `Bound::positiveInfinity()` and `Interval::untilPositiveInfinity()`.
pub(crate) const POSITIVE_INFINITY: &str = "9223372036854775807.0.0.0";

/// Comparison operator of a single constraint.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Op {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl Op {
    /// Accepts `=`, `==`, `<`, `<=`, `>`, `>=`, `<>`, `!=`.
    pub fn parse(s: &str) -> Option<Op> {
        Some(match s {
            "=" | "==" => Op::Eq,
            "<" => Op::Lt,
            "<=" => Op::Le,
            ">" => Op::Gt,
            ">=" => Op::Ge,
            "<>" | "!=" => Op::Ne,
            _ => return None,
        })
    }

    /// The canonical spelling Composer prints: `==`, `!=`, `<`, `<=`, `>`, `>=`.
    pub fn as_str(self) -> &'static str {
        match self {
            Op::Eq => "==",
            Op::Ne => "!=",
            Op::Lt => "<",
            Op::Le => "<=",
            Op::Gt => ">",
            Op::Ge => ">=",
        }
    }

    /// Whether `version_compare($a, $b, $op)` holds given `version_compare($a, $b)`.
    fn holds(self, ord: Ordering) -> bool {
        match self {
            Op::Eq => ord == Equal,
            Op::Ne => ord != Equal,
            Op::Lt => ord == Less,
            Op::Le => ord != Greater,
            Op::Gt => ord == Greater,
            Op::Ge => ord != Less,
        }
    }
}

/// `ConstraintInterface` and its four implementations.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Constraint {
    /// `MatchAllConstraint` (`*`).
    Any,
    /// `MatchNoneConstraint`.
    None,
    /// `Constraint($operator, $normalizedVersion)`.
    Single { op: Op, version: String },
    /// `MultiConstraint`.
    Multi { constraints: Vec<Constraint>, conjunctive: bool },
}

fn is_branch(version: &str) -> bool {
    version.starts_with("dev-")
}

impl Constraint {
    pub fn single(op: Op, version: impl Into<String>) -> Constraint {
        Constraint::Single { op, version: version.into() }
    }

    /// `MultiConstraint::create()`, including its single-element and
    /// `>= x < y` range-merging optimizations.
    pub fn multi(constraints: Vec<Constraint>, conjunctive: bool) -> Constraint {
        if constraints.len() <= 1 {
            return constraints.into_iter().next().unwrap_or(Constraint::Any);
        }
        if conjunctive {
            return Constraint::Multi { constraints, conjunctive };
        }
        // [>= 1 < 2] || [>= 2 < 3] || [>= 3 < 4] => [>= 1 < 4]
        let mut merged: Vec<Constraint> = Vec::with_capacity(constraints.len());
        for right in constraints {
            if let Some(left) = merged.last_mut()
                && let Some(range) = merge_contiguous(left, &right)
            {
                *left = range;
            } else {
                merged.push(right);
            }
        }
        if merged.len() == 1 {
            return merged.pop().unwrap();
        }
        Constraint::Multi { constraints: merged, conjunctive: false }
    }

    /// `ConstraintInterface::matches()`.
    pub fn matches(&self, provider: &Constraint) -> bool {
        use Constraint::*;
        match (self, provider) {
            (Any, _) => true,
            (None, _) => false,
            (Single { op, version }, Single { op: pop, version: pversion }) => match_specific(*op, version, *pop, pversion, false),
            (Single { .. }, _) => provider.matches(self),
            (Multi { constraints, conjunctive: false }, _) => constraints.iter().any(|c| provider.matches(c)),
            // [>1 and <2] must not match [<1 or >2]: iterate over the disjunctive side
            (Multi { conjunctive: true, .. }, Multi { conjunctive: false, .. }) => provider.matches(self),
            (Multi { constraints, conjunctive: true }, _) => constraints.iter().all(|c| provider.matches(c)),
        }
    }

    /// `CompilingMatcher::match($this, Constraint::OP_EQ, $normalizedVersion)`.
    pub fn matches_version(&self, normalized_version: &str) -> bool {
        self.compiled_match(Op::Eq, normalized_version)
    }

    /// `CompilingMatcher::match($this, $operator, $version)`: evaluates what `compile($operator)` would generate.
    pub(crate) fn compiled_match(&self, other: Op, v: &str) -> bool {
        use Op::*;
        let (version, op) = match self {
            Constraint::Any => return true,
            Constraint::None => return false,
            Constraint::Multi { constraints, conjunctive: true } => {
                return constraints.iter().all(|c| c.compiled_match(other, v));
            }
            Constraint::Multi { constraints, conjunctive: false } => {
                return constraints.iter().any(|c| c.compiled_match(other, v));
            }
            Constraint::Single { op, version } => (version.as_str(), *op),
        };
        let b = is_branch(v);
        if is_branch(version) {
            return match (op, other) {
                (Eq, Eq) => b && v == version,
                (Eq, Ne) | (Ne, Eq) => !b || v != version,
                (Ne, Ne) => true,
                (Ne, _) => !b,
                _ => false,
            };
        }
        let vc = |o: Op| o.holds(version_compare(v, version));
        match (op, other) {
            (Eq, Eq) => vc(Eq),
            (Eq, Ne) | (Ne, Eq) => b || vc(Ne),
            (Eq, _) => !b && other.holds(version_compare(version, v)),
            (Ne, Ne) => true,
            (Ne, _) => !b,
            (Lt | Le, Lt | Le) | (Gt | Ge, Gt | Ge) => !b,
            (_, Ne) => true,
            (Le, Gt) | (Ge, Lt) => !b && vc(Ne) && vc(op),
            _ => !b && vc(op),
        }
    }

    /// `getLowerBound()`.
    pub fn lower_bound(&self) -> Bound {
        self.bounds().0
    }

    /// `getUpperBound()`.
    pub fn upper_bound(&self) -> Bound {
        self.bounds().1
    }

    fn bounds(&self) -> (Bound, Bound) {
        let (zero, inf) = (Bound::zero(), Bound::positive_infinity());
        match self {
            Constraint::Any => (zero, inf),
            Constraint::None => {
                let none = Bound { version: "0.0.0.0-dev".into(), inclusive: false };
                (none.clone(), none)
            }
            Constraint::Single { version, .. } if is_branch(version) => (zero, inf),
            Constraint::Single { op, version } => {
                let at = |inclusive| Bound { version: version.clone(), inclusive };
                match op {
                    Op::Eq => (at(true), at(true)),
                    Op::Lt => (zero, at(false)),
                    Op::Le => (zero, at(true)),
                    Op::Gt => (at(false), inf),
                    Op::Ge => (at(true), inf),
                    Op::Ne => (zero, inf),
                }
            }
            Constraint::Multi { constraints, conjunctive } => {
                let (lower_op, upper_op) = if *conjunctive { (Op::Gt, Op::Lt) } else { (Op::Lt, Op::Gt) };
                let mut parts = constraints.iter().map(Constraint::bounds);
                let first = parts.next().unwrap_or((zero, inf));
                parts.fold(first, |(lower, upper), (l, u)| {
                    let lower = if l.compare_to(&lower, lower_op) { l } else { lower };
                    let upper = if u.compare_to(&upper, upper_op) { u } else { upper };
                    (lower, upper)
                })
            }
        }
    }
}

/// The `optimizeConstraints()` test: `[>= a <(=) b] || [>= b <(=) c]` becomes `[>= a <(=) c]`.
fn merge_contiguous(left: &Constraint, right: &Constraint) -> Option<Constraint> {
    use Constraint::{Multi, Single};
    let (Multi { constraints: l, conjunctive: true }, Multi { constraints: r, conjunctive: true }) = (left, right) else {
        return None;
    };
    let (
        [start @ Single { op: Op::Ge, .. }, Single { op: left_op @ (Op::Lt | Op::Le), version: left_end }],
        [Single { op: Op::Ge, version: right_start }, end @ Single { op: Op::Lt | Op::Le, .. }],
    ) = (l.as_slice(), r.as_slice())
    else {
        return None;
    };
    // PHP compares substr("< x", 2) / substr("<= x", 2) with substr(">= y", 3)
    let contiguous = match left_op {
        Op::Lt => left_end == right_start,
        _ => right_start.strip_prefix(' ') == Some(left_end.as_str()),
    };
    contiguous.then(|| Multi { constraints: vec![start.clone(), end.clone()], conjunctive: true })
}

/// `Constraint::matchSpecific()` of `[op version]` against the provider `[provider_op provider_version]`.
pub(crate) fn match_specific(op: Op, version: &str, provider_op: Op, provider_version: &str, compare_branches: bool) -> bool {
    use Op::*;
    // '!=' operator is match when other operator is not '==' operator or version is not match
    // these kinds of comparisons always have a solution
    if op == Ne || provider_op == Ne {
        if op == Ne && !matches!(provider_op, Eq | Ne) && is_branch(provider_version) {
            return false;
        }
        if provider_op == Ne && !matches!(op, Eq | Ne) && is_branch(version) {
            return false;
        }
        if op != Eq && provider_op != Eq {
            return true;
        }
        return branch_compare(provider_version, version, Ne, compare_branches);
    }

    // an example for the condition is <= 2.0 & < 1.0
    // these kinds of comparisons always have a solution
    if matches!((op, provider_op), (Lt | Le, Lt | Le) | (Gt | Ge, Gt | Ge)) {
        return !(is_branch(version) || is_branch(provider_version));
    }

    let (v1, v2, cmp_op) = if op == Eq { (version, provider_version, provider_op) } else { (provider_version, version, op) };
    // special case, e.g. require >= 1.0 and provide < 1.0
    // 1.0 >= 1.0 but 1.0 is outside of the provided interval
    branch_compare(v1, v2, cmp_op, compare_branches)
        && !(matches!(provider_op, Lt | Gt) && matches!(op, Eq | Le | Ge) && version_compare(provider_version, version) == Equal)
}

/// `Constraint::versionCompare()`.
fn branch_compare(a: &str, b: &str, op: Op, compare_branches: bool) -> bool {
    let (a_branch, b_branch) = (is_branch(a), is_branch(b));
    if op == Op::Ne && (a_branch || b_branch) {
        return a != b;
    }
    if a_branch && b_branch {
        return op == Op::Eq && a == b;
    }
    // when branches are not comparable, we make sure dev branches never match anything
    if !compare_branches && (a_branch || b_branch) {
        return false;
    }
    op.holds(version_compare(a, b))
}

/// `__toString()`, e.g. `>= 1.0.0.0-dev`, `[>= 1.0.0.0-dev < 2.0.0.0-dev]`, `*`, `[]`.
impl fmt::Display for Constraint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Constraint::Any => f.write_str("*"),
            Constraint::None => f.write_str("[]"),
            Constraint::Single { op, version } => write!(f, "{} {version}", op.as_str()),
            Constraint::Multi { constraints, conjunctive } => {
                f.write_str("[")?;
                for (i, c) in constraints.iter().enumerate() {
                    if i > 0 {
                        f.write_str(if *conjunctive { " " } else { " || " })?;
                    }
                    write!(f, "{c}")?;
                }
                f.write_str("]")
            }
        }
    }
}

/// `Composer\Semver\Constraint\Bound`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Bound {
    pub version: String,
    pub inclusive: bool,
}

impl Bound {
    pub fn zero() -> Bound {
        Bound { version: "0.0.0.0-dev".into(), inclusive: true }
    }

    pub fn positive_infinity() -> Bound {
        Bound { version: POSITIVE_INFINITY.into(), inclusive: false }
    }

    pub fn is_zero(&self) -> bool {
        self.version == "0.0.0.0-dev" && self.inclusive
    }

    pub fn is_positive_infinity(&self) -> bool {
        self.version == POSITIVE_INFINITY && !self.inclusive
    }

    /// `Bound::compareTo($other, $operator)`: "is this bound higher (`Gt`) / lower (`Lt`) than `other`".
    ///
    /// # Panics
    /// On any other operator, where PHP throws `InvalidArgumentException`.
    pub fn compare_to(&self, other: &Bound, op: Op) -> bool {
        assert!(matches!(op, Op::Lt | Op::Gt), "Does not support any other operator other than > or <.");
        // If they are the same it doesn't matter (PHP's loose `$this == $other`)
        if self.inclusive == other.inclusive && php::loose_eq(&self.version, &other.version) {
            return false;
        }
        match version_compare(&self.version, &other.version) {
            Equal if op == Op::Gt => other.inclusive,
            Equal => !other.inclusive,
            ord => ord == if op == Op::Gt { Greater } else { Less },
        }
    }
}

/// `Bound::__toString()`, e.g. `1.0.0.0 [inclusive]`.
impl fmt::Display for Bound {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} [{}]", self.version, if self.inclusive { "inclusive" } else { "exclusive" })
    }
}
