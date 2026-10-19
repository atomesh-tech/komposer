//! `DependencyResolver/Operation/*`: what the solver decided and the installer executes.

use crate::package::{DisplayMode, PackageRef};

#[derive(Clone, Debug)]
pub enum Operation {
    Install(PackageRef),
    Update {
        initial: PackageRef,
        target: PackageRef,
    },
    Uninstall(PackageRef),
    /// `package` is an alias
    MarkAliasInstalled(PackageRef),
    /// `package` is an alias
    MarkAliasUninstalled(PackageRef),
}

impl Operation {
    /// `getOperationType()`.
    pub fn kind(&self) -> &'static str {
        match self {
            Operation::Install(_) => "install",
            Operation::Update { .. } => "update",
            Operation::Uninstall(_) => "uninstall",
            Operation::MarkAliasInstalled(_) => "markAliasInstalled",
            Operation::MarkAliasUninstalled(_) => "markAliasUninstalled",
        }
    }

    /// The package the operation is about (the target for updates).
    pub fn package(&self) -> &PackageRef {
        match self {
            Operation::Update { target, .. } => target,
            Operation::Install(p) | Operation::Uninstall(p) | Operation::MarkAliasInstalled(p) | Operation::MarkAliasUninstalled(p) => p,
        }
    }

    pub fn is_alias_op(&self) -> bool {
        matches!(self, Operation::MarkAliasInstalled(_) | Operation::MarkAliasUninstalled(_))
    }

    /// `show($lock)`.
    pub fn show(&self, lock: bool) -> String {
        match self {
            Operation::Install(p) => format!(
                "{} <info>{}</info> (<comment>{}</comment>)",
                if lock { "Locking" } else { "Installing" },
                p.pretty_name,
                p.full_pretty_version()
            ),
            Operation::Update { initial, target } => {
                let (mut from, mut to) = (initial.full_pretty_version(), target.full_pretty_version());
                if from == to && initial.source_reference() != target.source_reference() {
                    from = initial.full_pretty_version_with(true, DisplayMode::SourceRef);
                    to = target.full_pretty_version_with(true, DisplayMode::SourceRef);
                } else if from == to && initial.dist_reference() != target.dist_reference() {
                    from = initial.full_pretty_version_with(true, DisplayMode::DistRef);
                    to = target.full_pretty_version_with(true, DisplayMode::DistRef);
                }
                let action = if komposer_semver::is_upgrade(&initial.version, &target.version) { "Upgrading" } else { "Downgrading" };
                format!("{action} <info>{}</info> (<comment>{from}</comment> => <comment>{to}</comment>)", initial.pretty_name)
            }
            Operation::Uninstall(p) => {
                format!("Removing <info>{}</info> (<comment>{}</comment>)", p.pretty_name, p.full_pretty_version())
            }
            Operation::MarkAliasInstalled(p) | Operation::MarkAliasUninstalled(p) => {
                let of = p.alias_of.as_ref().unwrap_or(p);
                format!(
                    "Marking <info>{}</info> (<comment>{}</comment>) as {}, alias of <info>{}</info> (<comment>{}</comment>)",
                    p.pretty_name,
                    p.full_pretty_version(),
                    if matches!(self, Operation::MarkAliasInstalled(_)) { "installed" } else { "uninstalled" },
                    of.pretty_name,
                    of.full_pretty_version()
                )
            }
        }
    }
}
