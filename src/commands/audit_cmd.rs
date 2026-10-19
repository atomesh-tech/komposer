//! `audit`: `AuditCommand`.

use crate::audit::{AuditMode, AuditOptions, FORMATS, audit_with};
use crate::cli::AuditArgs;
use crate::commands::info::filter_required_packages;
use crate::composer::Composer;
use anyhow::{Result, bail};
use komposer_package::io::Io;
use komposer_package::repository::Repository;
use komposer_solver::RepositorySet;
use std::sync::Arc;

/// `AuditCommand::execute()`: 1 when vulnerable, failing abandoned or filtered packages are found.
pub fn audit(io: &Arc<Io>, args: &AuditArgs, run_scripts: bool) -> Result<i32> {
    let _ = run_scripts;
    let c = Composer::load(io.clone())?;
    let packages = if args.locked {
        if !c.locker.is_locked()? {
            bail!("Valid composer.json and composer.lock files are required to run this command with --locked");
        }
        c.locker.locked_repository(!args.no_dev)?.packages()?
    } else {
        let installed = c.installed()?.packages()?;
        if args.no_dev { filter_required_packages(&installed, &c.root.package) } else { installed }
    };
    if packages.is_empty() {
        let root = &c.root.package;
        if !root.requires.is_empty() || (!args.no_dev && !root.dev_requires.is_empty()) {
            io.write_error(
                "No installed packages found. Please run \"composer install\" before running \"audit\" or pass \"--locked\" to audit the lock file.",
            );
            return Ok(1);
        }
        io.write_error("No packages - skipping audit.");
        return Ok(0);
    }

    let mut set = RepositorySet::new(komposer_semver::Stability::Stable);
    for repo in c.repositories()? {
        set.add_repository(repo);
    }
    let abandoned = match args.abandoned.as_deref() {
        Some(a) => Some(AuditMode::parse(a).ok_or_else(|| anyhow::anyhow!("--abandoned must be one of ignore, report, fail."))?),
        None => None,
    };
    let mut options = AuditOptions::from_config(&c.config)?;
    if let Some(mode) = abandoned {
        options.abandoned = mode;
    }
    for severity in &args.ignore_severity {
        if !options.ignore_severity.iter().any(|(s, _)| s == severity) {
            options.ignore_severity.push((severity.clone(), None));
        }
    }
    options.ignore_unreachable |= args.ignore_unreachable;
    if !FORMATS.contains(&args.format.as_str()) {
        bail!("--format must be one of {}.", FORMATS.join(", "));
    }
    Ok(audit_with(io, &set, &options, &packages, &args.format, false)?.min(255))
}
