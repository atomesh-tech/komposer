//! `Solver` (CDCL SAT solving with learning), `Decisions`, `RuleWatchGraph` and `RuleSetGenerator`.

use crate::filter::PlatformRequirementFilter;
use crate::policy::DefaultPolicy;
use crate::pool::{Literal, Pool};
use crate::problem::{Problem, SolverProblems};
use crate::request::{PrettyConstraint, Request};
use crate::rule::{Reason, RuleHandle, RuleKind, RuleSet, RuleType};
use crate::transaction::LockTransaction;
use anyhow::{Result, anyhow, bail};
use indexmap::IndexMap;
use komposer_package::io::{Io, Verbosity};
use komposer_semver::{Constraint, Op};
use std::borrow::Cow;
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Instant;

/// `SolverBugException`.
fn solver_bug(msg: String) -> anyhow::Error {
    anyhow!(
        "{msg}\nThis exception was most likely caused by a bug in Composer.\nPlease report the command you ran, the exact error you received, and your composer.json on https://github.com/composer/composer/issues - thank you!\n"
    )
}

/// `Decisions`: the decided literals, in order, with the rule that caused each.
struct Decisions {
    /// package id => level (positive: install, negative: don't install, 0: undecided)
    map: Vec<i32>,
    queue: Vec<(Literal, RuleHandle)>,
}

impl Decisions {
    fn new(pool_size: usize) -> Decisions {
        Decisions { map: vec![0; pool_size + 1], queue: vec![] }
    }

    fn decide(&mut self, literal: Literal, level: u32, why: RuleHandle, pool: &Pool) -> Result<()> {
        let id = literal.unsigned_abs() as usize;
        let previous = self.map[id];
        if previous != 0 {
            let package = pool.package(literal);
            return Err(solver_bug(format!(
                "Trying to decide {} on level {level}, even though {package} was previously decided as {previous}.",
                pool.literal_to_pretty_string(literal, &HashSet::new())
            )));
        }
        self.map[id] = if literal > 0 { level as i32 } else { -(level as i32) };
        self.queue.push((literal, why));
        Ok(())
    }

    fn get(&self, literal: Literal) -> i32 {
        self.map[literal.unsigned_abs() as usize]
    }

    fn satisfy(&self, literal: Literal) -> bool {
        let d = self.get(literal);
        (literal > 0 && d > 0) || (literal < 0 && d < 0)
    }

    fn conflict(&self, literal: Literal) -> bool {
        let d = self.get(literal);
        (d > 0 && literal < 0) || (d < 0 && literal > 0)
    }

    fn decided(&self, literal: Literal) -> bool {
        self.get(literal) != 0
    }

    fn undecided(&self, literal: Literal) -> bool {
        self.get(literal) == 0
    }

    fn decided_install(&self, literal: Literal) -> bool {
        self.get(literal) > 0
    }

    fn level(&self, literal: Literal) -> u32 {
        self.get(literal).unsigned_abs()
    }

    /// `decisionRule()`: the reason of the first decision about this package.
    fn decision_rule(&self, literal: Literal) -> RuleHandle {
        let id = literal.abs();
        self.queue.iter().find(|(l, _)| l.abs() == id).map(|&(_, why)| why).expect("decided literal has a decision")
    }

    fn reset_to_offset(&mut self, offset: isize) {
        while self.queue.len() as isize > offset + 1 {
            self.revert_last();
        }
    }

    fn revert_last(&mut self) {
        if let Some((literal, _)) = self.queue.pop() {
            self.map[literal.unsigned_abs() as usize] = 0;
        }
    }
}

/// `RuleWatchGraph` + `RuleWatchNode`s: each non-assertion rule watches two of its literals
/// (multi conflict rules all of theirs) to find unit rules quickly.
struct WatchGraph {
    /// literal + pool size => rules watching it, the chain's front is at the end
    chains: Vec<Vec<RuleHandle>>,
    /// rule handle => (watch1, watch2)
    watches: Vec<(Literal, Literal)>,
    offset: i32,
}

impl WatchGraph {
    fn new(pool_size: usize) -> WatchGraph {
        WatchGraph { chains: vec![vec![]; 2 * pool_size + 1], watches: vec![], offset: pool_size as i32 }
    }

    fn chain(&mut self, literal: Literal) -> &mut Vec<RuleHandle> {
        &mut self.chains[(literal + self.offset) as usize]
    }

    /// `new RuleWatchNode($rule)` (optionally with `watch2OnHighest()`) + `insert()`.
    fn insert(&mut self, rules: &RuleSet, rule: RuleHandle, decisions: Option<&Decisions>) {
        let literals = rules.literals(rule);
        let watch1 = literals.first().copied().unwrap_or(0);
        let mut watch2 = literals.get(1).copied().unwrap_or(0);
        let multi = rules.kind(rule) == RuleKind::MultiConflict;
        if let Some(decisions) = decisions
            && literals.len() >= 3
            && !multi
        {
            // Composer only moves the second watch here, which can end up equal to the first one
            let mut watch_level = 0;
            for &l in literals {
                let level = decisions.level(l);
                if level > watch_level {
                    watch2 = l;
                    watch_level = level;
                }
            }
        }
        if self.watches.len() <= rule as usize {
            self.watches.resize(rule as usize + 1, (0, 0));
        }
        if rules.is_assertion(rule) {
            return;
        }
        if multi {
            for &l in literals {
                self.chain(l).push(rule);
            }
        } else {
            self.chain(watch1).push(rule);
            self.chain(watch2).push(rule);
        }
        self.watches[rule as usize] = (watch1, watch2);
    }

    /// `propagateLiteral()`: makes the decisions implied by `decided`, returns a conflicting rule if any.
    fn propagate_literal(
        &mut self,
        decided: Literal,
        level: u32,
        rules: &RuleSet,
        decisions: &mut Decisions,
        pool: &Pool,
    ) -> Result<Option<RuleHandle>> {
        // A was decided => (-A|B) now requires B, so look at the rules watching -A
        let literal = -decided;
        let mut chain = std::mem::take(self.chain(literal));
        let mut conflict = None;
        let (mut i, mut kept) = (chain.len(), chain.len());
        'chain: while i > 0 {
            i -= 1;
            let rule = chain[i];
            if rules.kind(rule) != RuleKind::MultiConflict {
                let (w1, w2) = self.watches[rule as usize];
                let other = if w1 == literal { w2 } else { w1 };
                if !rules.is_disabled(rule) && !decisions.satisfy(other) {
                    let alternative = rules.literals(rule).iter().copied().find(|&l| l != literal && l != other && !decisions.conflict(l));
                    if let Some(alternative) = alternative {
                        let watches = &mut self.watches[rule as usize];
                        if watches.0 == literal {
                            watches.0 = alternative;
                        } else {
                            watches.1 = alternative;
                        }
                        self.chain(alternative).push(rule);
                        continue;
                    }
                    if decisions.conflict(other) {
                        conflict = Some(rule);
                    } else {
                        decisions.decide(other, level, rule, pool)?;
                    }
                }
            } else {
                for &other in rules.literals(rule) {
                    if literal != other && !decisions.satisfy(other) {
                        if decisions.conflict(other) {
                            conflict = Some(rule);
                            break;
                        }
                        decisions.decide(other, level, rule, pool)?;
                    }
                }
            }
            kept -= 1;
            chain[kept] = rule;
            if conflict.is_some() {
                break 'chain;
            }
        }
        // drop the rules that moved away, keeping the order of the rest
        chain.drain(i..kept);
        *self.chain(literal) = chain;
        Ok(conflict)
    }
}

/// `RuleSetGenerator`.
struct RuleSetGenerator<'a> {
    pool: &'a Pool,
    filter: &'a PlatformRequirementFilter,
    rules: RuleSet,
    added: Vec<bool>,
    added_order: Vec<Literal>,
    added_by_name: IndexMap<String, Vec<Literal>>,
}

impl<'a> RuleSetGenerator<'a> {
    /// `getRulesFor()`.
    fn rules_for(pool: &'a Pool, request: &Request, filter: &'a PlatformRequirementFilter) -> Result<RuleSet> {
        let mut generator = RuleSetGenerator {
            pool,
            filter,
            rules: RuleSet::new(),
            added: vec![false; pool.len() + 1],
            added_order: vec![],
            added_by_name: IndexMap::new(),
        };
        generator.add_rules_for_request(request)?;
        generator.add_rules_for_root_aliases();
        generator.add_conflict_rules();
        Ok(generator.rules)
    }

    fn add(&mut self, literals: &[Literal], kind: RuleKind, reason: Reason, rule_type: RuleType) {
        self.rules.add(literals, kind, reason, rule_type);
    }

    /// `createRequireRule()` + `addRule()`: (-A|B|C), skipped when A provides itself.
    fn add_require_rule(&mut self, package: Literal, providers: &[Literal], reason: Reason) {
        if providers.contains(&package) {
            return;
        }
        let literals: Vec<Literal> = std::iter::once(-package).chain(providers.iter().copied()).collect();
        self.add(&literals, RuleKind::Generic, reason, RuleType::Package);
    }

    /// `addRulesForPackage()`.
    fn add_rules_for_package(&mut self, package: Literal) {
        let mut queue = VecDeque::from([package]);
        while let Some(id) = queue.pop_front() {
            if self.added[id as usize] {
                continue;
            }
            self.added[id as usize] = true;
            self.added_order.push(id);

            let p = self.pool.package(id).clone();
            match &p.alias_of {
                None => {
                    for name in p.names(false) {
                        self.added_by_name.entry(name).or_default().push(id);
                    }
                }
                Some(of) => {
                    let Some(of_id) = self.pool.id_of(of) else { continue };
                    queue.push_back(of_id);
                    self.add_require_rule(id, &[of_id], Reason::PackageAlias(p.clone()));
                    // aliases must be installed with their main package, so create a rule the other way around too
                    self.add_require_rule(of_id, &[id], Reason::PackageInverseAlias(of.clone()));
                    // the aliased package takes care of the requirements, unless they were rewritten
                    if !p.has_self_version_requires {
                        continue;
                    }
                }
            }

            for (i, link) in p.requires.iter().enumerate() {
                if self.filter.is_ignored(&link.target) {
                    continue;
                }
                let constraint = self.filter.filter_constraint(&link.target, &link.constraint, true);
                let providers = self.pool.what_provides(&link.target, Some(&constraint));
                self.add_require_rule(id, &providers, Reason::PackageRequires { package: p.clone(), link: i });
                queue.extend(providers.iter().copied());
            }
        }
    }

    /// `addConflictRules()`.
    fn add_conflict_rules(&mut self) {
        for k in 0..self.added_order.len() {
            let id = self.added_order[k];
            let p = self.pool.package(id).clone();
            for (i, link) in p.conflicts.iter().enumerate() {
                // even if the conflict ends up being with an alias, there is at least one actual package by this name
                if !self.added_by_name.contains_key(&link.target) || self.filter.is_ignored(&link.target) {
                    continue;
                }
                let constraint = self.filter.filter_constraint(&link.target, &link.constraint, false);
                for &conflict in self.pool.what_provides(&link.target, Some(&constraint)).iter() {
                    // aliases only need the rule for an exact name match, provides/replaces of the aliased package cover the rest
                    let c = self.pool.package(conflict);
                    if conflict != id && (!c.is_alias() || c.name == link.target) {
                        let reason = Reason::PackageConflict { package: p.clone(), link: i };
                        self.add(&[-id, -conflict], RuleKind::TwoLiterals, reason, RuleType::Package);
                    }
                }
            }
        }

        let same_name: Vec<(String, Vec<Literal>)> =
            self.added_by_name.iter().filter(|(_, ids)| ids.len() > 1).map(|(n, ids)| (n.clone(), ids.clone())).collect();
        for (name, ids) in same_name {
            let literals: Vec<Literal> = ids.iter().map(|id| -id).collect();
            let kind = if literals.len() == 2 { RuleKind::TwoLiterals } else { RuleKind::MultiConflict };
            self.add(&literals, kind, Reason::PackageSameName(name), RuleType::Package);
        }
    }

    /// `addRulesForRequest()`.
    fn add_rules_for_request(&mut self, request: &Request) -> Result<()> {
        for p in request.fixed_packages() {
            // a locked package dropped by a filter list is reported by Solver::check_for_filter_list_removed_locked_packages
            if request.is_locked_package(p) && self.pool.is_filter_list_removed(&p.name, &Constraint::single(Op::Eq, p.version.clone())) {
                continue;
            }
            let Some(id) = self.pool.id_of(p) else {
                // fixed packages may be left out of the pool for not passing the stability requirements
                if self.pool.is_unacceptable_fixed_or_locked_package(p) {
                    continue;
                }
                bail!("Fixed package {} was not added to solver pool.", p.pretty_string());
            };
            self.add_rules_for_package(id);
            self.add(&[id], RuleKind::Generic, Reason::Fixed(p.clone()), RuleType::Request);
        }

        for (name, constraint) in request.requires() {
            if self.filter.is_ignored(name) {
                continue;
            }
            let constraint = filtered(self.filter, name, constraint);
            let packages = self.pool.what_provides(name, Some(&constraint.constraint));
            if packages.is_empty() {
                continue;
            }
            for &p in packages.iter() {
                self.add_rules_for_package(p);
            }
            self.add(&packages, RuleKind::Generic, Reason::RootRequire(Box::new((name.clone(), constraint))), RuleType::Request);
        }
        Ok(())
    }

    /// `addRulesForRootAliases()`: root aliases and aliases of loaded packages are always installed along.
    fn add_rules_for_root_aliases(&mut self) {
        for (i, p) in self.pool.packages().iter().enumerate() {
            let id = i as Literal + 1;
            if self.added[id as usize] {
                continue;
            }
            if let Some(of) = &p.alias_of
                && (p.root_package_alias || self.pool.id_of(of).is_some_and(|of_id| self.added[of_id as usize]))
            {
                self.add_rules_for_package(id);
            }
        }
    }
}

/// A root requirement after `IgnoreListPlatformRequirementFilter::filterConstraint()`.
fn filtered(filter: &PlatformRequirementFilter, name: &str, constraint: &PrettyConstraint) -> PrettyConstraint {
    match filter.filter_constraint(name, &constraint.constraint, true) {
        Cow::Borrowed(_) => constraint.clone(),
        Cow::Owned(c) => PrettyConstraint::plain(c),
    }
}

/// `Solver`.
pub struct Solver<'a> {
    policy: &'a DefaultPolicy,
    pool: &'a Pool,
    io: &'a Io,
    rules: RuleSet,
    rule_set_size: usize,
    watches: WatchGraph,
    decisions: Decisions,
    fixed: Vec<bool>,
    propagate_index: usize,
    branches: Vec<(Vec<Literal>, u32)>,
    problems: Vec<Problem>,
    learned_pool: Vec<Vec<RuleHandle>>,
    learned_why: HashMap<RuleHandle, usize>,
    /// Set when a negative decision was learned as positive literal (for tests).
    pub test_flag_learned_positive_literal: bool,
}

impl<'a> Solver<'a> {
    pub fn new(policy: &'a DefaultPolicy, pool: &'a Pool, io: &'a Io) -> Solver<'a> {
        Solver {
            policy,
            pool,
            io,
            rules: RuleSet::new(),
            rule_set_size: 0,
            watches: WatchGraph::new(0),
            decisions: Decisions::new(0),
            fixed: vec![],
            propagate_index: 0,
            branches: vec![],
            problems: vec![],
            learned_pool: vec![],
            learned_why: HashMap::new(),
            test_flag_learned_positive_literal: false,
        }
    }

    /// `getRuleSetSize()`.
    pub fn rule_set_size(&self) -> usize {
        self.rule_set_size
    }

    /// `solve()`. Unsolvable requests fail with a [`SolverProblems`] error.
    pub fn solve(&mut self, request: &Request, filter: &PlatformRequirementFilter) -> Result<LockTransaction> {
        let pool = self.pool;
        self.fixed = vec![false; pool.len() + 1];
        for p in request.fixed_packages() {
            if let Some(id) = pool.id_of(p) {
                self.fixed[id as usize] = true;
            }
        }

        self.io.write_error_at("Generating rules", Verbosity::Debug);
        self.rules = RuleSetGenerator::rules_for(pool, request, filter)?;
        self.check_for_root_require_problems(request, filter);
        self.check_for_filter_list_removed_locked_packages(request);
        self.decisions = Decisions::new(pool.len());
        self.watches = WatchGraph::new(pool.len());
        for rule in self.rules.iter() {
            self.watches.insert(&self.rules, rule, None);
        }

        // make decisions based on root require/fix assertions
        self.make_assertion_rule_decisions()?;

        self.io.write_error_at("Resolving dependencies through SAT", Verbosity::Debug);
        let before = Instant::now();
        self.run_sat()?;
        self.rule_set_size = self.rules.len();
        self.io.write_error_at("", Verbosity::Debug);
        let elapsed = before.elapsed().as_secs_f64();
        self.io.write_error_at(&format!("Dependency resolution completed in {elapsed:.3} seconds"), Verbosity::Verbose);

        if !self.problems.is_empty() {
            return Err(SolverProblems { problems: std::mem::take(&mut self.problems), rules: std::mem::take(&mut self.rules) }.into());
        }

        LockTransaction::new(pool, request.present_packages(), &self.fixed, &self.decisions.queue)
    }

    /// `makeAssertionRuleDecisions()` (`solver_makeruledecisions`).
    fn make_assertion_rule_decisions(&mut self) -> Result<()> {
        let decision_start = self.decisions.queue.len() as isize - 1;
        let count = self.rules.len();
        let mut index = 0;
        while index < count {
            let rule = self.rules.by_id(index);
            index += 1;
            if !self.rules.is_assertion(rule) || self.rules.is_disabled(rule) {
                continue;
            }
            let literal = self.rules.literals(rule)[0];
            if !self.decisions.decided(literal) {
                self.decisions.decide(literal, 1, rule, self.pool)?;
                continue;
            }
            if self.decisions.satisfy(literal) {
                continue;
            }

            // found a conflict
            if self.rules.rule_type(rule) == Some(RuleType::Learned) {
                self.rules.disable(rule);
                continue;
            }
            let conflict = self.decisions.decision_rule(literal);
            let mut problem = Problem::default();
            problem.add_rule(rule);
            problem.add_rule(conflict);
            if self.rules.rule_type(conflict) == Some(RuleType::Package) {
                self.rules.disable(rule);
                self.problems.push(problem);
                continue;
            }

            // conflict with another root require/fixed package: all assertions about this package are problems
            for &assert_rule in &self.rules.of_type(RuleType::Request).to_vec() {
                if self.rules.is_disabled(assert_rule) || !self.rules.is_assertion(assert_rule) {
                    continue;
                }
                if literal.abs() != self.rules.literals(assert_rule)[0].abs() {
                    continue;
                }
                problem.add_rule(assert_rule);
                self.rules.disable(assert_rule);
            }
            self.problems.push(problem);
            self.decisions.reset_to_offset(decision_start);
            index = 0;
        }
        Ok(())
    }

    /// `checkForRootRequireProblems()`.
    fn check_for_root_require_problems(&mut self, request: &Request, filter: &PlatformRequirementFilter) {
        for (name, constraint) in request.requires() {
            if filter.is_ignored(name) {
                continue;
            }
            let constraint = filtered(filter, name, constraint);
            if self.pool.what_provides(name, Some(&constraint.constraint)).is_empty() {
                let rule = self.rules.add_detached(&[], RuleKind::Generic, Reason::RootRequire(Box::new((name.clone(), constraint))));
                let mut problem = Problem::default();
                problem.add_rule(rule);
                self.problems.push(problem);
            }
        }
    }

    /// `checkForFilterListRemovedLockedPackages()`: locked packages a policy filter list dropped from the pool.
    fn check_for_filter_list_removed_locked_packages(&mut self, request: &Request) {
        for p in request.locked_packages() {
            if !self.pool.is_filter_list_removed(&p.name, &Constraint::single(Op::Eq, p.version.clone())) {
                continue;
            }
            let rule = self.rules.add_detached(&[], RuleKind::Generic, Reason::LockedFilterListRemoved(p));
            let mut problem = Problem::default();
            problem.add_rule(rule);
            self.problems.push(problem);
        }
    }

    /// `propagate()`: returns the conflicting rule, if any.
    fn propagate(&mut self, level: u32) -> Result<Option<RuleHandle>> {
        while self.propagate_index < self.decisions.queue.len() {
            let literal = self.decisions.queue[self.propagate_index].0;
            let conflict = self.watches.propagate_literal(literal, level, &self.rules, &mut self.decisions, self.pool)?;
            self.propagate_index += 1;
            if conflict.is_some() {
                return Ok(conflict);
            }
        }
        Ok(None)
    }

    /// `revert()`: undoes the decisions above `level`.
    fn revert(&mut self, level: u32) {
        while let Some(&(literal, _)) = self.decisions.queue.last() {
            if self.decisions.undecided(literal) || self.decisions.level(literal) <= level {
                break;
            }
            self.decisions.revert_last();
            self.propagate_index = self.decisions.queue.len();
        }
        while self.branches.last().is_some_and(|(_, l)| *l >= level) {
            self.branches.pop();
        }
    }

    /// `setPropagateLearn()`: decides `literal` on a new level and propagates, learning from conflicts.
    /// Returns the new level, 0 if unsolvable.
    fn set_propagate_learn(&mut self, mut level: u32, literal: Literal, rule: RuleHandle) -> Result<u32> {
        level += 1;
        self.decisions.decide(literal, level, rule, self.pool)?;

        while let Some(conflict) = self.propagate(level)? {
            if level == 1 {
                self.analyze_unsolvable(conflict);
                return Ok(0);
            }

            let (learn_literal, new_level, literals, why) = self.analyze(level, conflict)?;
            if new_level == 0 || new_level >= level {
                return Err(solver_bug(format!("Trying to revert to invalid level {new_level} from level {level}.")));
            }
            level = new_level;
            self.revert(level);

            let reason = Reason::Learned(why);
            let new_rule = match self.rules.add(&literals, RuleKind::Generic, reason.clone(), RuleType::Learned) {
                Some(rule) => rule,
                None => self.rules.add_detached(&literals, RuleKind::Generic, reason),
            };
            self.learned_why.insert(new_rule, why);
            self.watches.insert(&self.rules, new_rule, Some(&self.decisions));
            self.decisions.decide(learn_literal, level, new_rule, self.pool)?;
        }
        Ok(level)
    }

    /// `selectAndInstall()`: tries the best candidate, remembering the others as a branch.
    fn select_and_install(&mut self, level: u32, queue: &[Literal], rule: RuleHandle) -> Result<u32> {
        let required = self.rules.reason(rule).required_package().map(str::to_string);
        let mut literals = self.policy.select_preferred_packages(self.pool, queue, required.as_deref());
        let selected = literals.remove(0);
        if !literals.is_empty() {
            self.branches.push((literals, level));
        }
        self.set_propagate_learn(level, selected, rule)
    }

    /// `analyze()`: conflict analysis, returns (learned literal, level to revert to, learned rule literals, why).
    fn analyze(&mut self, level: u32, conflict: RuleHandle) -> Result<(Literal, u32, Vec<Literal>, usize)> {
        let analyzed = conflict;
        let mut rule = conflict;
        let mut rule_level = 1;
        let (mut num, mut l1num) = (0i64, 0i64);
        let mut seen = vec![false; self.pool.len() + 1];
        let mut learned_literal = None;
        let mut other_learned: Vec<Literal> = vec![];
        let mut decision_id = self.decisions.queue.len();
        self.learned_pool.push(vec![]);
        let why = self.learned_pool.len() - 1;

        'analyze: loop {
            self.learned_pool[why].push(rule);
            let multi = self.rules.kind(rule) == RuleKind::MultiConflict;
            for &literal in self.rules.literals(rule) {
                // a multi conflict rule is many rules in one, some may not have finished propagating yet
                if (multi && !self.decisions.decided(literal)) || self.decisions.satisfy(literal) {
                    continue;
                }
                let id = literal.unsigned_abs() as usize;
                if seen[id] {
                    continue;
                }
                seen[id] = true;
                let l = self.decisions.level(literal);
                if l == 1 {
                    l1num += 1;
                } else if l == level {
                    num += 1;
                } else {
                    // not level 1 or conflict level, add to the new rule
                    other_learned.push(literal);
                    rule_level = rule_level.max(l);
                }
            }

            let mut l1retry = true;
            while l1retry {
                l1retry = false;
                if num == 0 {
                    l1num -= 1;
                    if l1num == 0 {
                        // all level 1 literals done
                        break 'analyze;
                    }
                }

                let literal = loop {
                    if decision_id == 0 {
                        return Err(solver_bug(format!(
                            "Reached invalid decision id {decision_id} while looking through {} for a literal present in the analyzed rule {}.",
                            self.rules.rule_to_string(rule),
                            self.rules.rule_to_string(analyzed)
                        )));
                    }
                    decision_id -= 1;
                    let literal = self.decisions.queue[decision_id].0;
                    if seen[literal.unsigned_abs() as usize] {
                        break literal;
                    }
                };
                seen[literal.unsigned_abs() as usize] = false;

                if num != 0 && {
                    num -= 1;
                    num == 0
                } {
                    if literal < 0 {
                        self.test_flag_learned_positive_literal = true;
                    }
                    learned_literal = Some(-literal);
                    if l1num == 0 {
                        break 'analyze;
                    }
                    for &other in &other_learned {
                        seen[other.unsigned_abs() as usize] = false;
                    }
                    // only level 1 marks left
                    l1num += 1;
                    l1retry = true;
                } else {
                    rule = self.decisions.queue[decision_id].1;
                    if self.rules.kind(rule) == RuleKind::MultiConflict {
                        // there is only ever exactly one positive decision in a multi conflict rule
                        for &rule_literal in self.rules.literals(rule) {
                            let id = rule_literal.unsigned_abs() as usize;
                            if !seen[id] && self.decisions.satisfy(-rule_literal) {
                                self.learned_pool[why].push(rule);
                                let l = self.decisions.level(rule_literal);
                                if l == 1 {
                                    l1num += 1;
                                } else if l == level {
                                    num += 1;
                                } else {
                                    other_learned.push(rule_literal);
                                    rule_level = rule_level.max(l);
                                }
                                seen[id] = true;
                                break;
                            }
                        }
                        l1retry = true;
                    }
                }
            }
            rule = self.decisions.queue[decision_id].1;
        }

        let Some(learned_literal) = learned_literal else {
            return Err(solver_bug(format!("Did not find a learnable literal in analyzed rule {}.", self.rules.rule_to_string(analyzed))));
        };
        other_learned.insert(0, learned_literal);
        Ok((learned_literal, rule_level, other_learned, why))
    }

    /// `analyzeUnsolvableRule()`.
    fn analyze_unsolvable_rule(&self, problem: &mut Problem, rule: RuleHandle, seen: &mut HashSet<RuleHandle>) {
        seen.insert(rule);
        match self.rules.rule_type(rule) {
            Some(RuleType::Learned) => {
                for &r in &self.learned_pool[self.learned_why[&rule]] {
                    if !seen.contains(&r) {
                        self.analyze_unsolvable_rule(problem, r, seen);
                    }
                }
            }
            // package rules cannot be part of a problem
            Some(RuleType::Package) => {}
            _ => {
                problem.next_section();
                problem.add_rule(rule);
            }
        }
    }

    /// `analyzeUnsolvable()`.
    fn analyze_unsolvable(&mut self, conflict: RuleHandle) {
        let mut problem = Problem::default();
        problem.add_rule(conflict);
        let mut rule_seen = HashSet::new();
        self.analyze_unsolvable_rule(&mut problem, conflict, &mut rule_seen);

        let mut seen = HashSet::new();
        let mark = |solver: &Solver, rule: RuleHandle, seen: &mut HashSet<Literal>| {
            for &l in solver.rules.literals(rule) {
                // skip the one true literal
                if !solver.decisions.satisfy(l) {
                    seen.insert(l.abs());
                }
            }
        };
        mark(self, conflict, &mut seen);
        for &(literal, why) in self.decisions.queue.iter().rev() {
            // skip literals that are not in this rule
            if !seen.contains(&literal.abs()) {
                continue;
            }
            problem.add_rule(why);
            self.analyze_unsolvable_rule(&mut problem, why, &mut rule_seen);
            mark(self, why, &mut seen);
        }
        self.problems.push(problem);
    }

    /// `runSat()`.
    fn run_sat(&mut self) -> Result<()> {
        self.propagate_index = 0;
        // 1) propagate new decisions, 2) fulfill root requires/fixed packages, 3) fulfill all unresolved rules,
        // 4) minimize the solution if there were choices; on problems, rewind to a safe level and restart
        let mut level = 1;
        let mut system_level = level + 1;
        let mut queue: Vec<Literal> = vec![];

        loop {
            if level == 1
                && let Some(conflict) = self.propagate(level)?
            {
                self.analyze_unsolvable(conflict);
                return Ok(());
            }

            // handle root require/fixed package rules
            if level < system_level {
                let request_rules = self.rules.of_type(RuleType::Request).to_vec();
                let mut broke_at = None;
                for (pos, &rule) in request_rules.iter().enumerate() {
                    if self.rules.is_disabled(rule) {
                        continue;
                    }
                    queue.clear();
                    let mut none_satisfied = true;
                    for &literal in self.rules.literals(rule) {
                        if self.decisions.satisfy(literal) {
                            none_satisfied = false;
                            break;
                        }
                        if literal > 0 && self.decisions.undecided(literal) {
                            queue.push(literal);
                        }
                    }
                    if !none_satisfied || queue.is_empty() {
                        continue;
                    }
                    // if any of the options in the decision queue are fixed, only use those
                    if queue.iter().any(|&l| self.fixed[l.unsigned_abs() as usize]) {
                        queue.retain(|&l| self.fixed[l.unsigned_abs() as usize]);
                    }
                    let old_level = level;
                    level = self.select_and_install(level, &queue, rule)?;
                    if level == 0 {
                        return Ok(());
                    }
                    if level <= old_level {
                        broke_at = Some(pos);
                        break;
                    }
                }
                system_level = level + 1;
                // root requires/fixed packages left: Composer's iterator moves past the rule it stopped at
                if broke_at.is_some_and(|pos| pos + 1 < request_rules.len()) {
                    continue;
                }
            }

            if level < system_level {
                system_level = level;
            }

            let mut rules_count = self.rules.len();
            let mut pass = 1;
            self.io.write_error_at("Looking at all rules.", Verbosity::Debug);
            let (mut i, mut n) = (0, 0);
            while n < rules_count {
                if i == rules_count {
                    let msg = format!("Something's changed, looking at all rules again (pass #{pass})");
                    self.io.write_error_at(&msg, Verbosity::Debug);
                    i = 0;
                    pass += 1;
                }
                let rule = self.rules.by_id(i);
                i += 1;
                n += 1;
                if self.rules.is_disabled(rule) {
                    continue;
                }

                // all negative literals installed and no positive literal installed: the rule is not
                // fulfilled and we just need to decide on the positive literals
                queue.clear();
                let mut fulfilled = false;
                for &literal in self.rules.literals(rule) {
                    if literal <= 0 {
                        if !self.decisions.decided_install(literal) {
                            fulfilled = true;
                            break;
                        }
                    } else {
                        if self.decisions.decided_install(literal) {
                            fulfilled = true;
                            break;
                        }
                        if self.decisions.undecided(literal) {
                            queue.push(literal);
                        }
                    }
                }
                // need at least 2 items to pick from
                if fulfilled || queue.len() < 2 {
                    continue;
                }

                level = self.select_and_install(level, &queue, rule)?;
                if level == 0 {
                    return Ok(());
                }
                // something changed, so look at all rules again
                rules_count = self.rules.len();
                n = 0;
            }

            if level < system_level {
                continue;
            }

            // minimization step
            let mut last = None;
            for (bi, (literals, l)) in self.branches.iter().enumerate().rev() {
                for (offset, &literal) in literals.iter().enumerate() {
                    if literal > 0 && self.decisions.level(literal) > l + 1 {
                        last = Some((literal, bi, offset, *l));
                    }
                }
            }
            if let Some((literal, bi, offset, l)) = last {
                self.branches[bi].0.remove(offset);
                level = l;
                self.revert(level);
                let why = self.decisions.queue.last().expect("decisions above level 0").1;
                level = self.set_propagate_learn(level, literal, why)?;
                if level == 0 {
                    return Ok(());
                }
                continue;
            }
            break;
        }
        Ok(())
    }
}
