//! Ports of the composer/semver PHPUnit suites (and Composer's VersionParserTest).

use crate::{Constraint, Op, parse_constraints};

mod constraint;
mod intervals;
mod php;
mod semver;
mod version_parser;

/// `new Constraint($op, $version)`.
fn c(op: &str, version: &str) -> Constraint {
    Constraint::single(Op::parse(op).unwrap(), version)
}

/// `new MultiConstraint($constraints, true)`.
fn and(constraints: Vec<Constraint>) -> Constraint {
    Constraint::Multi { constraints, conjunctive: true }
}

/// `new MultiConstraint($constraints, false)`.
fn or(constraints: Vec<Constraint>) -> Constraint {
    Constraint::Multi { constraints, conjunctive: false }
}

fn parse(constraints: &str) -> Constraint {
    parse_constraints(constraints).unwrap_or_else(|e| panic!("{constraints}: {e}"))
}
