//! `reinstall`: `ReinstallCommand`.

use crate::cli::ReinstallArgs;
use crate::commands::dump_autoload;
use crate::commands::install::{package_name_regex, preferred_install};
use crate::composer::Composer;
use anyhow::{Result, bail};
use komposer_autoload::AutoloadOptions;
use komposer_installer::{InstallationManager, InstalledRepo};
use komposer_package::PackageRef;
use komposer_package::io::Io;
use komposer_package::operation::Operation;
use komposer_solver::transaction_operations;
use std::sync::Arc;

/// `ReinstallCommand::execute()`.
pub fn reinstall(io: &Arc<Io>, args: &ReinstallArgs, run_scripts: bool) -> Result<i32> {
    let mut c = Composer::load(io.clone())?;
    let mut local_repo = InstalledRepo::load(&c.installed_json().to_string_lossy(), Some(c.root.clone()))?;
    let installed = local_repo.packages.canonical();
    let mut reinstall: Vec<PackageRef> = vec![];
    if !args.types.is_empty() {
        if !args.packages.is_empty() {
            bail!("You cannot specify package names and filter by type at the same time.");
        }
        reinstall.extend(installed.iter().filter(|p| args.types.contains(&p.package_type)).cloned());
    } else {
        if args.packages.is_empty() {
            bail!("You must pass one or more package names to be reinstalled.");
        }
        for pattern in &args.packages {
            let re = package_name_regex(pattern);
            let matched: Vec<&PackageRef> = installed.iter().filter(|p| re.is_match(&p.name)).collect();
            if matched.is_empty() {
                io.warning(&format!("Pattern \"{pattern}\" does not match any currently installed packages."));
            }
            reinstall.extend(matched.into_iter().cloned());
        }
    }
    if reinstall.is_empty() {
        io.warning("Found no packages to reinstall, aborting.");
        return Ok(1);
    }

    // the install operations come ordered by dependencies (and plugins first)
    let result = local_repo.packages.all().to_vec();
    let present: Vec<PackageRef> = result.iter().filter(|p| !reinstall.iter().any(|r| r.name == p.name)).cloned().collect();
    let install_ops = transaction_operations(&present, &result);
    let order =
        |p: &PackageRef| install_ops.iter().position(|op| matches!(op, Operation::Install(i) if !i.is_alias() && i.name == p.name)).unwrap_or(0);
    reinstall.sort_by_key(|p| std::cmp::Reverse(order(p)));
    let uninstall_ops: Vec<Operation> = reinstall.into_iter().map(Operation::Uninstall).collect();

    let (prefer_source, prefer_dist) = preferred_install(&c.config, &args.flags)?;
    let dev_mode = local_repo.dev_mode.unwrap_or(true);
    // SAFETY: single-threaded before running scripts
    unsafe { std::env::set_var("COMPOSER_DEV_MODE", if dev_mode { "1" } else { "0" }) };
    c.scripts(run_scripts).dispatch("pre-install-cmd", &[])?;

    let fetch = crate::installer::fetcher(c.http.clone());
    let mut im = InstallationManager::new(&c.config, &c.io, &fetch);
    im.prefer_source = prefer_source;
    im.prefer_dist = prefer_dist;
    let scripts = c.scripts(run_scripts);
    for ops in [&uninstall_ops, &install_ops] {
        let mut events = scripts.package_events(ops);
        for plugin in im.execute(&mut local_repo, ops, dev_mode, false, events.as_mut().map(|f| f as _))? {
            io.warning(&format!("{plugin} is a Composer plugin, komposer installed it but does not run plugins."));
        }
    }
    drop(im);

    if !args.no_autoloader {
        let flags = &args.flags;
        let authoritative = flags.classmap_authoritative || c.config.get_bool("classmap-authoritative");
        let opts = AutoloadOptions {
            dev_mode: Some(dev_mode),
            optimize: flags.optimize_autoloader || c.config.get_bool("optimize-autoloader") || authoritative,
            classmap_authoritative: authoritative,
            apcu: flags.apcu_autoloader_prefix.is_some() || flags.apcu_autoloader || c.config.get_bool("apcu-autoloader"),
            apcu_prefix: flags.apcu_autoloader_prefix.clone(),
            lock_content_hash: c.locker.locked_content_hash(),
            dev_package_names: local_repo.dev_package_names.clone(),
            is_platform_req_ignored: crate::platform_filter::autoload(flags.ignore_platform_reqs, &flags.ignore_platform_req),
            ..Default::default()
        };
        // its AutoloadGenerator is left at setRunScripts(false): no autoload-dump events here
        dump_autoload::dump(&mut c, &local_repo.packages.canonical(), &opts, false, dev_mode)?;
    }
    c.scripts(run_scripts).dispatch("post-install-cmd", &[])?;
    Ok(0)
}
