//! Composer's dependency resolution: pool building, the SAT solver, problem reporting and
//! transactions (`Composer\DependencyResolver`, `RepositorySet`, `VersionSelector`).
//!
//! Typical use, like `Installer::doUpdate()`:
//! build a [`RepositorySet`], a [`Request`], [`RepositorySet::create_pool`], then
//! [`Solver::solve`] with a [`DefaultPolicy`]; unsolvable requests fail with [`SolverProblems`].

mod filter;
mod policy;
mod policy_filter;
mod pool;
mod pool_builder;
mod pool_optimizer;
mod problem;
mod repository_set;
mod request;
mod rule;
mod solver;
mod transaction;
mod version_selector;

pub use filter::PlatformRequirementFilter;
pub use policy::DefaultPolicy;
pub use policy_filter::{BlockScope, CustomList, PolicyOptions, filter_list_filter, security_advisory_filter};
pub use pool::{Literal, Pool};
pub use pool_builder::{FilterListFilter, PoolOptions};
pub use pool_optimizer::optimize as optimize_pool;
pub use problem::{Problem, SolverProblems, missing_locked_package_reason};
pub use repository_set::{Advisories, RepositorySet};
pub use request::{PrettyConstraint, Request, UpdateAllowTransitive};
pub use rule::{Reason, RuleHandle, RuleKind, RuleSet, RuleType};
pub use solver::Solver;
pub use transaction::{LockTransaction, local_repo_transaction, transaction_operations};
pub use version_selector::VersionSelector;
