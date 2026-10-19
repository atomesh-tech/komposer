use crate::constraint::POSITIVE_INFINITY;
use crate::php::{self, version_compare};
use crate::{Constraint, Op};
use std::cell::RefCell;
use std::cmp::Ordering::Equal;
use std::collections::HashMap;
use std::rc::Rc;

/// `Composer\Semver\Interval`: `start` is a `>`/`>=` constraint, `end` a `<`/`<=` one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Interval {
    pub start: Constraint,
    pub end: Constraint,
}

/// The `branches` half of `Intervals::get()`: `exclude == false` means
/// "only these dev branch names", `exclude == true` means "any dev branch except these".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BranchSet {
    pub names: Vec<String>,
    pub exclude: bool,
}

/// Result of `Intervals::get()`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Intervals {
    pub numeric: Vec<Interval>,
    pub branches: BranchSet,
}

impl Interval {
    fn new(start: Constraint, end: Constraint) -> Interval {
        Interval { start, end }
    }

    /// `Interval::any()`: `Interval::fromZero()` to `Interval::untilPositiveInfinity()`.
    fn any() -> Interval {
        Interval::new(Constraint::single(Op::Ge, "0.0.0.0-dev"), Constraint::single(Op::Lt, POSITIVE_INFINITY))
    }
}

fn is_from_zero(c: &Constraint) -> bool {
    matches!(c, Constraint::Single { op: Op::Ge, version } if version == "0.0.0.0-dev")
}

fn is_until_positive_infinity(c: &Constraint) -> bool {
    matches!(c, Constraint::Single { op: Op::Lt, version } if version == POSITIVE_INFINITY)
}

/// Interval borders are always single constraints.
fn border(c: &Constraint) -> (Op, &str) {
    match c {
        Constraint::Single { op, version } => (*op, version),
        _ => unreachable!("interval borders are single constraints"),
    }
}

impl BranchSet {
    /// `Interval::anyDev()`: exclude nothing.
    fn any_dev() -> BranchSet {
        BranchSet { names: vec![], exclude: true }
    }

    /// `Interval::noDev()`: include nothing.
    fn no_dev() -> BranchSet {
        BranchSet { names: vec![], exclude: false }
    }
}

thread_local! {
    /// `Intervals::$intervalsCache`.
    static INTERVALS: RefCell<HashMap<Constraint, Rc<Intervals>>> = RefCell::default();
}

/// `Intervals::get()`.
pub fn get_intervals(constraint: &Constraint) -> Rc<Intervals> {
    if let Some(cached) = INTERVALS.with_borrow(|cache| cache.get(constraint).cloned()) {
        return cached;
    }
    let intervals = Rc::new(generate(constraint, false));
    INTERVALS.with_borrow_mut(|cache| cache.insert(constraint.clone(), intervals.clone()));
    intervals
}

/// `Intervals::isSubsetOf($candidate, $constraint)`.
pub fn is_subset_of(candidate: &Constraint, constraint: &Constraint) -> bool {
    match (candidate, constraint) {
        (_, Constraint::Any) => true,
        (Constraint::None, _) | (_, Constraint::None) => false,
        _ => {
            let parts = [get_intervals(candidate), get_intervals(constraint)];
            combine(&parts, true, false) == *parts[0]
        }
    }
}

/// `Intervals::haveIntersections()`.
pub fn have_intersections(a: &Constraint, b: &Constraint) -> bool {
    match (a, b) {
        (Constraint::Any, _) | (_, Constraint::Any) => true,
        (Constraint::None, _) | (_, Constraint::None) => false,
        _ => {
            let i = combine(&[get_intervals(a), get_intervals(b)], true, true);
            !i.numeric.is_empty() || i.branches.exclude || !i.branches.names.is_empty()
        }
    }
}

/// `Intervals::compactConstraint()`.
pub fn compact_constraint(constraint: &Constraint) -> Constraint {
    use Constraint::Multi;
    if !matches!(constraint, Multi { .. }) {
        return constraint.clone();
    }
    let intervals = get_intervals(constraint);
    let Intervals { numeric, branches } = &*intervals;
    let mut constraints = vec![];
    let mut has_numeric_match_all = false;

    if numeric.len() == 1 && is_from_zero(&numeric[0].start) && is_until_positive_infinity(&numeric[0].end) {
        constraints.push(numeric[0].start.clone());
        has_numeric_match_all = true;
    } else {
        // A bare != N matches all dev-* versions, so if the numeric part is the whole line minus some points,
        // only compact it into unbounded != constraints when the branches already match all dev versions
        let count = numeric.len();
        let skip_not_equal = !branches.exclude
            && count > 1
            && is_from_zero(&numeric[0].start)
            && is_until_positive_infinity(&numeric[count - 1].end)
            && numeric.windows(2).all(|w| {
                let ((end_op, end), (start_op, start)) = (border(&w[0].end), border(&w[1].start));
                end_op == Op::Lt && start_op == Op::Gt && end == start
            });

        let mut unequal = vec![];
        for (i, interval) in numeric.iter().enumerate() {
            let (start_op, start) = border(&interval.start);
            let (end_op, end) = border(&interval.end);

            // if current interval ends with < N and next interval begins with > N we can swap this out for != N
            // but this needs to happen as a conjunctive expression together with the start of the current interval
            // and end of next interval, so [>=M, <N] || [>N, <P] => [>=M, !=N, <P] but M/P can be skipped if
            // they are zero/+inf
            if !skip_not_equal
                && end_op == Op::Lt
                && let Some(next) = numeric.get(i + 1)
                && border(&next.start) == (Op::Gt, end)
            {
                // only add a start if we didn't already do so, can be skipped if we're looking at second
                // interval in [>=M, <N] || [>N, <P] || [>P, <Q] where unequal currently contains
                // [>=M, !=N] already and we only want to add !=P right now
                if unequal.is_empty() && !is_from_zero(&interval.start) {
                    unequal.push(interval.start.clone());
                }
                unequal.push(Constraint::single(Op::Ne, end));
                continue;
            }

            if !unequal.is_empty() {
                // this is where the end of the following interval of a != constraint is added as explained above
                if !is_until_positive_infinity(&interval.end) {
                    unequal.push(interval.end.clone());
                }
                // count is 1 if entire constraint is just one != expression
                constraints.push(if unequal.len() > 1 {
                    Multi { constraints: std::mem::take(&mut unequal), conjunctive: true }
                } else {
                    unequal.remove(0)
                });
                continue;
            }

            // convert back >= x - <= x intervals to == x
            if start == end && start_op == Op::Ge && end_op == Op::Le {
                constraints.push(Constraint::single(Op::Eq, start));
            } else if is_from_zero(&interval.start) {
                constraints.push(interval.end.clone());
            } else if is_until_positive_infinity(&interval.end) {
                constraints.push(interval.start.clone());
            } else {
                constraints.push(Multi { constraints: vec![interval.start.clone(), interval.end.clone()], conjunctive: true });
            }
        }
    }

    if branches.names.is_empty() {
        // otherwise constraint should contain a != operator and already cover this
        if branches.exclude && has_numeric_match_all {
            return Constraint::Any;
        }
    } else {
        let op = if branches.exclude { Op::Ne } else { Op::Eq };
        let mut dev: Vec<Constraint> = branches.names.iter().map(|name| Constraint::single(op, name.clone())).collect();

        // excluded branches, e.g. != dev-foo are conjunctive with the interval, so
        // > 2.0 != dev-foo must return a conjunctive constraint
        if branches.exclude {
            if constraints.len() > 1 {
                dev.insert(0, Multi { constraints, conjunctive: false });
                return Multi { constraints: dev, conjunctive: true };
            }
            if constraints.len() == 1 && is_from_zero(&constraints[0]) {
                return if dev.len() > 1 { Multi { constraints: dev, conjunctive: true } } else { dev.remove(0) };
            }
            constraints.append(&mut dev);
            return Multi { constraints, conjunctive: true };
        }

        // otherwise dev contains a list of == operators for branches which are disjunctive with the
        // rest of the constraint
        constraints.append(&mut dev);
    }

    match constraints.len() {
        0 => Constraint::None,
        1 => constraints.remove(0),
        _ => Multi { constraints, conjunctive: false },
    }
}

/// `Intervals::generateIntervals()`.
fn generate(constraint: &Constraint, stop_on_first_valid_interval: bool) -> Intervals {
    match constraint {
        Constraint::Any => Intervals { numeric: vec![Interval::any()], branches: BranchSet::any_dev() },
        Constraint::None => Intervals { numeric: vec![], branches: BranchSet::no_dev() },
        Constraint::Single { op, version } => single(*op, version),
        Constraint::Multi { constraints, conjunctive } => {
            let parts: Vec<Rc<Intervals>> = constraints.iter().map(get_intervals).collect();
            combine(&parts, *conjunctive, stop_on_first_valid_interval)
        }
    }
}

/// The `MultiConstraint` half of `generateIntervals()`, given the intervals of each part.
fn combine(parts: &[Rc<Intervals>], conjunctive: bool, stop_on_first_valid_interval: bool) -> Intervals {
    let mut branches = if conjunctive { BranchSet::any_dev() } else { BranchSet::no_dev() };
    for b in parts.iter().map(|p| &p.branches) {
        let names = &mut branches.names;
        match (conjunctive, branches.exclude, b.exclude) {
            // array_intersect: !=a,!=b || !=b,!=c => !=b; (==a||==b) && (==a||==c) => ==a
            (false, true, true) | (true, false, false) => names.retain(|n| b.names.contains(n)),
            // array_diff(ours, theirs): !=a,!=b || (==b||==c) => !=a; (==a||==c) && !=a,!=b => ==c
            (false, true, false) | (true, false, true) => names.retain(|n| !b.names.contains(n)),
            // array_merge: (==a||==b) || ==c => ==a||==b||==c; !=a && !=b => !=a,!=b
            (false, false, false) | (true, true, true) => names.extend(b.names.iter().cloned()),
            // array_diff(theirs, ours), taking their exclude flag:
            // (==b||==c) || !=a,!=b => !=a; !=a,!=b && (==a||==c) => ==c
            (false, false, true) | (true, true, false) => {
                *names = b.names.iter().filter(|n| !names.contains(n)).cloned().collect();
                branches.exclude = b.exclude;
            }
        }
    }
    // array_unique
    let mut names: Vec<String> = Vec::with_capacity(branches.names.len());
    for name in branches.names {
        if !names.contains(&name) {
            names.push(name);
        }
    }
    branches.names = names;

    if parts.len() == 1 {
        return Intervals { numeric: parts[0].numeric.clone(), branches };
    }

    let borders: Vec<(&str, Op, bool)> = parts
        .iter()
        .flat_map(|p| &p.numeric)
        .flat_map(|i| {
            let ((start_op, start), (end_op, end)) = (border(&i.start), border(&i.end));
            [(start, start_op, true), (end, end_op, false)]
        })
        .collect();
    let op_order = |op: Op| match op {
        Op::Ge => -3,
        Op::Lt => -2,
        Op::Gt => 2,
        _ => 3, // <=
    };
    let order = php::usort(&borders, |a, b| version_compare(a.0, b.0).then(op_order(a.1).cmp(&op_order(b.1))));

    let threshold = if conjunctive { parts.len() as isize } else { 1 };
    let mut active = 0;
    let mut start: Option<(Op, &str)> = None;
    let mut numeric = vec![];
    for (version, op, is_start) in order.into_iter().map(|i| borders[i]) {
        active += if is_start { 1 } else { -1 };
        match start {
            None if active >= threshold => start = Some((op, version)),
            Some((start_op, start_version)) if active < threshold => {
                // filter out invalid intervals like > x - <= x, or >= x - < x
                let empty = version_compare(start_version, version) == Equal && matches!((start_op, op), (Op::Gt, Op::Le) | (Op::Ge, Op::Lt));
                if !empty {
                    numeric.push(Interval::new(Constraint::single(start_op, start_version), Constraint::single(op, version)));
                    if stop_on_first_valid_interval {
                        break;
                    }
                }
                start = None;
            }
            _ => {}
        }
    }
    Intervals { numeric, branches }
}

/// `Intervals::generateSingleConstraintIntervals()`.
fn single(op: Op, version: &str) -> Intervals {
    let no_dev = |numeric| Intervals { numeric, branches: BranchSet::no_dev() };
    let c = |op| Constraint::single(op, version);

    // handle branch constraints first
    if version.starts_with("dev-") {
        let names = vec![version.to_string()];
        return match op {
            // != dev-foo means any numeric version may match
            Op::Ne => Intervals { numeric: vec![Interval::any()], branches: BranchSet { names, exclude: true } },
            Op::Eq => Intervals { numeric: vec![], branches: BranchSet { names, exclude: false } },
            _ => no_dev(vec![]),
        };
    }

    let Interval { start: zero, end: infinity } = Interval::any();
    match op {
        Op::Gt | Op::Ge => no_dev(vec![Interval::new(c(op), infinity)]),
        // filter out the invalid interval >= 0.0.0.0-dev - < 0.0.0.0-dev
        Op::Lt if version == "0.0.0.0-dev" => no_dev(vec![]),
        Op::Lt | Op::Le => no_dev(vec![Interval::new(zero, c(op))]),
        // convert !=x to intervals of 0 - <x && >x - +inf + dev*
        Op::Ne => Intervals { numeric: vec![Interval::new(zero, c(Op::Lt)), Interval::new(c(Op::Gt), infinity)], branches: BranchSet::any_dev() },
        // convert ==x to an interval of >=x - <=x
        Op::Eq => no_dev(vec![Interval::new(c(Op::Ge), c(Op::Le))]),
    }
}
