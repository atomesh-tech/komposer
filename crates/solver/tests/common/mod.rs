//! Helpers mirroring Composer's `TestCase` (`getPackage()`, `getVersionConstraint()`, `getAliasPackage()`).
#![allow(dead_code)]

use komposer_package::io::Io;
use komposer_package::operation::Operation;
use komposer_package::repository::{ArrayRepository, RepoKind, Repository};
use komposer_package::{Link, LinkType, Package, PackageRef};
use komposer_semver::{Constraint, Op, Stability, normalize};
use komposer_solver::{
    DefaultPolicy, LockTransaction, PlatformRequirementFilter, Pool, PoolOptions, PrettyConstraint, RepositorySet, Request, Solver, SolverProblems,
};
use std::sync::Arc;

/// `getPackage()`.
pub fn pkg(name: &str, version: &str) -> Package {
    Package::new(name, &normalize(version).unwrap(), version)
}

/// `getAliasPackage()`.
pub fn alias(of: &PackageRef, version: &str) -> Package {
    Package::alias(of, &normalize(version).unwrap(), version)
}

/// `getVersionConstraint()`: `new Constraint($op, normalize($version))` with pretty string "op version".
pub fn vc(op: &str, version: &str) -> PrettyConstraint {
    PrettyConstraint::new(Constraint::single(Op::parse(op).unwrap(), normalize(version).unwrap()), format!("{op} {version}"))
}

/// `new Link($source, $target, $constraint, $type)`.
pub fn link(source: &str, target: &str, constraint: PrettyConstraint, kind: LinkType) -> Link {
    Link {
        source: source.to_lowercase(),
        target: target.to_lowercase(),
        constraint: constraint.constraint,
        kind,
        pretty_constraint: constraint.pretty,
    }
}

pub fn any() -> PrettyConstraint {
    PrettyConstraint::new(Constraint::Any, "*")
}

pub fn requires(mut p: Package, links: &[(&str, PrettyConstraint)]) -> Package {
    let source = p.name.clone();
    p.requires = links.iter().map(|(t, c)| link(&source, t, c.clone(), LinkType::Require)).collect();
    p
}

pub fn repo(kind: RepoKind) -> ArrayRepository {
    ArrayRepository::new("test repo", kind)
}

/// A repository set over the given repositories, with the locked repository last (`reposComplete()`).
pub struct Env {
    pub set: RepositorySet,
    pub locked: Arc<dyn Repository>,
    pub policy: DefaultPolicy,
}

impl Env {
    pub fn new(repos: Vec<ArrayRepository>, locked: ArrayRepository) -> Env {
        Env::with_stability(Stability::Stable, repos, locked)
    }

    pub fn with_stability(stability: Stability, repos: Vec<ArrayRepository>, locked: ArrayRepository) -> Env {
        let mut set = RepositorySet::new(stability);
        for r in repos {
            set.add_repository(Arc::new(r));
        }
        let locked: Arc<dyn Repository> = Arc::new(locked);
        set.add_repository(locked.clone());
        Env { set, locked, policy: DefaultPolicy::default() }
    }

    pub fn request(&self) -> Request {
        Request::with_locked_repository(self.locked.clone()).unwrap()
    }

    pub fn pool(&self, request: &mut Request) -> Pool {
        self.set.create_pool(request, &Io::null(), &PoolOptions::default()).unwrap()
    }

    /// `createSolver()` + `solve()`.
    pub fn solve(&self, request: &mut Request) -> (Pool, anyhow::Result<LockTransaction>, bool) {
        let pool = self.pool(request);
        let io = Io::null();
        let mut solver = Solver::new(&self.policy, &pool, &io);
        let result = solver.solve(request, &PlatformRequirementFilter::IgnoreNothing);
        let flag = solver.test_flag_learned_positive_literal;
        drop(solver);
        (pool, result, flag)
    }

    /// The problems output of an unsolvable request.
    pub fn problems(&self, request: &mut Request) -> String {
        let (pool, result, _) = self.solve(request);
        let err = result.expect_err("unsolvable");
        let problems = err.downcast_ref::<SolverProblems>().expect("solver problems");
        problems.pretty_string(&self.set, request, &pool, false, false, &[]).unwrap()
    }
}

/// An expected operation, compared by package identity.
pub enum Exp<'a> {
    Install(&'a PackageRef),
    Update(&'a PackageRef, &'a PackageRef),
    Remove(&'a PackageRef),
    MarkAliasInstalled(&'a PackageRef),
    MarkAliasUninstalled(&'a PackageRef),
}

fn describe(op: &Operation) -> String {
    match op {
        Operation::Update { initial, target } => format!("update {initial} => {target}"),
        op => format!("{} {}", op.kind(), op.package()),
    }
}

pub fn check_operations(operations: &[Operation], expected: &[Exp]) {
    let actual: Vec<String> = operations.iter().map(describe).collect();
    let wanted: Vec<String> = expected
        .iter()
        .map(|e| match e {
            Exp::Install(p) => format!("install {p}"),
            Exp::Update(a, b) => format!("update {a} => {b}"),
            Exp::Remove(p) => format!("uninstall {p}"),
            Exp::MarkAliasInstalled(p) => format!("markAliasInstalled {p}"),
            Exp::MarkAliasUninstalled(p) => format!("markAliasUninstalled {p}"),
        })
        .collect();
    assert_eq!(actual, wanted);
    for (op, e) in operations.iter().zip(expected) {
        let same = match (op, e) {
            (Operation::Install(p), Exp::Install(q))
            | (Operation::Uninstall(p), Exp::Remove(q))
            | (Operation::MarkAliasInstalled(p), Exp::MarkAliasInstalled(q))
            | (Operation::MarkAliasUninstalled(p), Exp::MarkAliasUninstalled(q)) => Arc::ptr_eq(p, q),
            (Operation::Update { initial, target }, Exp::Update(a, b)) => Arc::ptr_eq(initial, a) && Arc::ptr_eq(target, b),
            _ => false,
        };
        assert!(same, "{} is not the expected package instance", describe(op));
    }
}

/// A repository with security advisories and filter list entries, like Composer's `PackageRepository`
/// with `security-advisories` and `filter` data. `failing` makes both fail like an unreachable server.
#[derive(Default)]
pub struct PolicyRepo {
    pub advisories: Vec<komposer_package::repository::SecurityAdvisory>,
    pub filter: Vec<komposer_package::repository::FilterEntry>,
    pub failing: Option<String>,
    /// names advisories were requested for
    pub requested: std::sync::Mutex<Vec<String>>,
}

impl Repository for PolicyRepo {
    fn repo_name(&self) -> String {
        "policy repo".into()
    }
    fn load_packages(&self, _: &komposer_package::repository::LoadRequest) -> anyhow::Result<komposer_package::repository::LoadResult> {
        Ok(Default::default())
    }
    fn packages(&self) -> anyhow::Result<Vec<PackageRef>> {
        Ok(vec![])
    }
    fn has_security_advisories(&self) -> bool {
        true
    }
    fn security_advisories(
        &self,
        constraints: &[(String, Constraint)],
        allow_partial: bool,
    ) -> anyhow::Result<komposer_package::repository::AdvisoryResult> {
        if let Some(msg) = &self.failing {
            anyhow::bail!("{msg}");
        }
        self.requested.lock().unwrap().extend(constraints.iter().map(|(n, _)| n.clone()));
        let mut result = komposer_package::repository::AdvisoryResult::default();
        for a in &self.advisories {
            let matches = constraints.iter().any(|(n, c)| *n == a.package_name && a.affected_versions.matches(c));
            if matches && (allow_partial || !a.is_partial()) {
                result.advisories.entry(a.package_name.clone()).or_default().push(a.clone());
            }
        }
        Ok(result)
    }
    fn filter_entries(&self, _: &[(String, Constraint)], lists: &[String]) -> anyhow::Result<Vec<komposer_package::repository::FilterEntry>> {
        if let Some(msg) = &self.failing {
            anyhow::bail!("{msg}");
        }
        Ok(self.filter.iter().filter(|e| lists.contains(&e.list_name)).cloned().collect())
    }
}

/// `generateSecurityAdvisory()`: a complete advisory.
pub fn advisory(package: &str, id: &str, cve: Option<&str>, affected: &str) -> komposer_package::repository::SecurityAdvisory {
    komposer_package::repository::SecurityAdvisory {
        package_name: package.into(),
        advisory_id: id.into(),
        affected_versions: komposer_semver::parse_constraints(affected).unwrap(),
        affected_versions_pretty: affected.into(),
        title: Some("Security Advisory".into()),
        cve: cve.map(String::from),
        link: None,
        severity: Some("high".into()),
        reported_at: Some("2024-04-31 12:37:47".into()),
        sources: vec![serde_json::json!({"name": "Security Advisory", "remoteId": "test"})],
    }
}

pub fn malware(package: &str, constraint: &str) -> komposer_package::repository::FilterEntry {
    komposer_package::repository::FilterEntry {
        package_name: package.into(),
        list_name: "malware".into(),
        constraint: komposer_semver::parse_constraints(constraint).unwrap(),
        url: Some(format!("https://example.org/malware/{package}")),
        reason: Some("malware".into()),
        id: None,
        source: None,
    }
}
