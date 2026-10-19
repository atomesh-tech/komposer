//! `Rule`, `GenericRule`, `Rule2Literals`, `MultiConflictRule`, `RuleSet` and `RuleSetIterator`.
//!
//! Rules live in a [`RuleSet`] arena and are referred to by handle. Literals of all rules
//! share one buffer, sorted ascending like Composer sorts them.

use crate::pool::Literal;
use crate::request::PrettyConstraint;
use komposer_package::{Link, PackageRef};
use std::collections::HashMap;
use std::fmt::Write as _;

/// Index of a rule in its [`RuleSet`]'s arena.
pub type RuleHandle = u32;

/// `RuleSet::TYPE_*`, highest priority first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuleType {
    Package,
    Request,
    Learned,
}

impl RuleType {
    fn index(self) -> usize {
        self as usize
    }

    /// `RuleSet::TYPES`.
    pub fn name(self) -> &'static str {
        match self {
            RuleType::Package => "PACKAGE",
            RuleType::Request => "REQUEST",
            RuleType::Learned => "LEARNED",
        }
    }
}

/// The rule classes, which matter for duplicate detection and watching.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RuleKind {
    /// `GenericRule`
    Generic,
    /// `Rule2Literals`
    TwoLiterals,
    /// `MultiConflictRule`: acts as a conflict between every pair of its (negative) literals.
    MultiConflict,
}

/// `Rule::RULE_*` with its reason data.
#[derive(Clone, Debug)]
pub enum Reason {
    RootRequire(Box<(String, PrettyConstraint)>),
    Fixed(PackageRef),
    /// The link is `package.conflicts[link]`.
    PackageConflict {
        package: PackageRef,
        link: usize,
    },
    /// The link is `package.requires[link]`.
    PackageRequires {
        package: PackageRef,
        link: usize,
    },
    PackageSameName(String),
    /// Index into the solver's learned pool.
    Learned(usize),
    PackageAlias(PackageRef),
    PackageInverseAlias(PackageRef),
    LockedFilterListRemoved(PackageRef),
}

impl Reason {
    /// The link of conflict and require rules.
    pub fn link(&self) -> Option<&Link> {
        match self {
            Reason::PackageConflict { package, link } => Some(&package.conflicts[*link]),
            Reason::PackageRequires { package, link } => Some(&package.requires[*link]),
            _ => None,
        }
    }

    /// `getRequiredPackage()`.
    pub fn required_package(&self) -> Option<&str> {
        match self {
            Reason::RootRequire(req) => Some(&req.0),
            Reason::Fixed(p) | Reason::LockedFilterListRemoved(p) => Some(&p.name),
            Reason::PackageRequires { .. } => self.link().map(|l| l.target.as_str()),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
struct RuleData {
    start: u32,
    len: u32,
    kind: RuleKind,
    /// `None` until added to the set (Composer's initial type 255).
    rule_type: Option<RuleType>,
    disabled: bool,
    reason: Reason,
    next_same_hash: Option<RuleHandle>,
}

/// `RuleSet`.
#[derive(Clone, Debug, Default)]
pub struct RuleSet {
    literals: Vec<Literal>,
    rules: Vec<RuleData>,
    /// `ruleById`: handles of the added rules, in order of addition.
    by_id: Vec<RuleHandle>,
    by_type: [Vec<RuleHandle>; 3],
    by_hash: HashMap<u64, RuleHandle>,
}

impl RuleSet {
    pub fn new() -> RuleSet {
        RuleSet::default()
    }

    /// `add()` for a new rule: returns `None` if an equal rule was added before.
    pub fn add(&mut self, literals: &[Literal], kind: RuleKind, reason: Reason, rule_type: RuleType) -> Option<RuleHandle> {
        let handle = self.push(literals, kind, reason);
        let lits = self.literals(handle);
        let hash = lits.iter().fold(kind as u64 + 1, |h, &l| (h.rotate_left(5) ^ l as u32 as u64).wrapping_mul(0x517c_c1b7_2722_0a95));
        let mut candidate = self.by_hash.get(&hash).copied();
        while let Some(other) = candidate {
            if self.rules[other as usize].kind == kind && self.literals(other) == lits {
                self.literals.truncate(self.rules[handle as usize].start as usize);
                self.rules.pop();
                return None;
            }
            candidate = self.rules[other as usize].next_same_hash;
        }
        let data = &mut self.rules[handle as usize];
        data.next_same_hash = self.by_hash.insert(hash, handle);
        data.rule_type = Some(rule_type);
        self.by_id.push(handle);
        self.by_type[rule_type.index()].push(handle);
        Some(handle)
    }

    /// A rule that is not part of the set (problem reasons, learned duplicates).
    pub fn add_detached(&mut self, literals: &[Literal], kind: RuleKind, reason: Reason) -> RuleHandle {
        self.push(literals, kind, reason)
    }

    fn push(&mut self, literals: &[Literal], kind: RuleKind, reason: Reason) -> RuleHandle {
        let start = self.literals.len();
        self.literals.extend_from_slice(literals);
        self.literals[start..].sort_unstable();
        let handle = self.rules.len() as RuleHandle;
        let len = literals.len() as u32;
        self.rules.push(RuleData { start: start as u32, len, kind, rule_type: None, disabled: false, reason, next_same_hash: None });
        handle
    }

    /// `count()`: the number of added rules.
    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    /// `ruleById[$id]`.
    pub fn by_id(&self, id: usize) -> RuleHandle {
        self.by_id[id]
    }

    /// The rules of one type, in order of addition (`getIteratorFor()`).
    pub fn of_type(&self, rule_type: RuleType) -> &[RuleHandle] {
        &self.by_type[rule_type.index()]
    }

    /// `getIterator()`: all rules, package rules first, then request and learned rules.
    pub fn iter(&self) -> impl Iterator<Item = RuleHandle> + '_ {
        self.by_type.iter().flatten().copied()
    }

    pub fn literals(&self, rule: RuleHandle) -> &[Literal] {
        let data = &self.rules[rule as usize];
        &self.literals[data.start as usize..(data.start + data.len) as usize]
    }

    pub fn reason(&self, rule: RuleHandle) -> &Reason {
        &self.rules[rule as usize].reason
    }

    pub fn rule_type(&self, rule: RuleHandle) -> Option<RuleType> {
        self.rules[rule as usize].rule_type
    }

    pub fn kind(&self, rule: RuleHandle) -> RuleKind {
        self.rules[rule as usize].kind
    }

    pub fn is_disabled(&self, rule: RuleHandle) -> bool {
        self.rules[rule as usize].disabled
    }

    pub fn disable(&mut self, rule: RuleHandle) {
        self.rules[rule as usize].disabled = true;
    }

    pub fn enable(&mut self, rule: RuleHandle) {
        self.rules[rule as usize].disabled = false;
    }

    /// `isAssertion()`: a generic rule with a single literal.
    pub fn is_assertion(&self, rule: RuleHandle) -> bool {
        let data = &self.rules[rule as usize];
        data.kind == RuleKind::Generic && data.len == 1
    }

    /// `__toString()`, e.g. `(-1|2)`, `disabled(multi(-1|-2|-3))`.
    pub fn rule_to_string(&self, rule: RuleHandle) -> String {
        let data = &self.rules[rule as usize];
        let mut out = String::from(if data.disabled { "disabled(" } else { "(" });
        let multi = data.kind == RuleKind::MultiConflict;
        if multi {
            out.push_str("multi(");
        }
        for (i, l) in self.literals(rule).iter().enumerate() {
            let _ = write!(out, "{}{l}", if i == 0 { "" } else { "|" });
        }
        out.push_str(if multi { "))" } else { ")" });
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use komposer_semver::Constraint;

    fn root_require() -> Reason {
        Reason::RootRequire(Box::new((String::new(), PrettyConstraint::new(Constraint::Any, "*"))))
    }

    #[test]
    fn add_keeps_order_per_type() {
        let mut set = RuleSet::new();
        let a = set.add(&[1], RuleKind::Generic, root_require(), RuleType::Request).unwrap();
        let l = set.add(&[], RuleKind::Generic, Reason::Learned(1), RuleType::Learned).unwrap();
        let b = set.add(&[2], RuleKind::Generic, root_require(), RuleType::Request).unwrap();
        assert_eq!(set.of_type(RuleType::Request), &[a, b]);
        assert_eq!(set.of_type(RuleType::Learned), &[l]);
        assert!(set.of_type(RuleType::Package).is_empty());
        assert_eq!(set.len(), 3);
        assert_eq!(set.by_id(0), a);
        assert_eq!(set.iter().collect::<Vec<_>>(), vec![a, b, l]);
        assert_eq!(set.rule_type(a), Some(RuleType::Request));
    }

    #[test]
    fn add_ignores_duplicates() {
        let mut set = RuleSet::new();
        for _ in 0..3 {
            set.add(&[], RuleKind::Generic, root_require(), RuleType::Request);
        }
        assert_eq!(set.of_type(RuleType::Request).len(), 1);
        // a 2-literal generic rule is never a duplicate of a Rule2Literals, like Composer's differing hashes
        assert!(set.add(&[-2, -1], RuleKind::Generic, root_require(), RuleType::Package).is_some());
        assert!(set.add(&[-1, -2], RuleKind::TwoLiterals, root_require(), RuleType::Package).is_some());
        assert!(set.add(&[-2, -1], RuleKind::TwoLiterals, root_require(), RuleType::Package).is_none());
    }

    #[test]
    fn equality_compares_sorted_literals() {
        let mut set = RuleSet::new();
        assert!(set.add(&[1, 2], RuleKind::Generic, root_require(), RuleType::Package).is_some());
        assert!(set.add(&[1, 3], RuleKind::Generic, root_require(), RuleType::Package).is_some());
        assert!(set.add(&[1], RuleKind::Generic, root_require(), RuleType::Package).is_some());
        assert!(set.add(&[12, 1], RuleKind::Generic, root_require(), RuleType::Package).is_some());
        assert!(set.add(&[1, 12], RuleKind::Generic, root_require(), RuleType::Package).is_none());
        assert_eq!(set.literals(set.by_id(3)), &[1, 12]);
    }

    #[test]
    fn flags_and_strings() {
        let mut set = RuleSet::new();
        let r = set.add(&[3, -1], RuleKind::Generic, root_require(), RuleType::Request).unwrap();
        let single = set.add(&[1], RuleKind::Generic, root_require(), RuleType::Request).unwrap();
        let multi = set.add(&[-3, -1, -2], RuleKind::MultiConflict, root_require(), RuleType::Package).unwrap();
        assert!(!set.is_assertion(r));
        assert!(set.is_assertion(single));
        assert!(!set.is_assertion(multi));
        set.disable(r);
        assert!(set.is_disabled(r));
        set.enable(r);
        assert!(!set.is_disabled(r));
        set.disable(r);
        assert_eq!(set.rule_to_string(r), "disabled(-1|3)");
        assert_eq!(set.rule_to_string(multi), "(multi(-3|-2|-1))");
    }
}
