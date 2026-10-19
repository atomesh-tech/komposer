//! `dump-autoload`.

use crate::cli::DumpAutoloadArgs;
use crate::composer::Composer;
use crate::platform_filter;
use anyhow::{Result, bail};
use komposer_autoload::AutoloadOptions;
use komposer_package::repository::Repository;

pub fn run(c: &mut Composer, args: &DumpAutoloadArgs, run_scripts: bool) -> Result<i32> {
    let installed = c.installed()?;
    let packages: Vec<_> = installed.packages()?.into_iter().filter(|p| !p.is_alias()).collect();
    let missing = packages.iter().any(|p| c.install_path(p).is_some_and(|path| !path.exists()));
    if missing {
        c.io.write("<warning>Not all dependencies are installed. Make sure to run a \"composer install\" to install missing dependencies</warning>");
    }

    let optimize = args.optimize || c.config.get_bool("optimize-autoloader");
    let authoritative = args.classmap_authoritative || c.config.get_bool("classmap-authoritative");
    let apcu = args.apcu_prefix.is_some() || args.apcu || c.config.get_bool("apcu-autoloader");
    if args.strict_psr && !optimize && !authoritative {
        bail!(
            "--strict-psr mode only works with optimized autoloader, use --optimize or --classmap-authoritative if you want a strict return value."
        );
    }
    if args.strict_ambiguous && !optimize && !authoritative {
        bail!(
            "--strict-ambiguous mode only works with optimized autoloader, use --optimize or --classmap-authoritative if you want a strict return value."
        );
    }
    if args.dev && args.no_dev {
        bail!("You can not use both --no-dev and --dev as they conflict with each other.");
    }

    let label = if authoritative {
        "optimized autoload files (authoritative)"
    } else if optimize {
        "optimized autoload files"
    } else {
        "autoload files"
    };
    c.io.write(&format!("<info>Generating {label}</info>"));

    let dev_mode = if args.no_dev {
        Some(false)
    } else if args.dev {
        Some(true)
    } else {
        None
    };
    let opts = AutoloadOptions {
        dev_mode,
        optimize,
        classmap_authoritative: authoritative,
        apcu,
        apcu_prefix: args.apcu_prefix.clone(),
        strict_ambiguous: args.strict_ambiguous,
        lock_content_hash: c.locker.locked_content_hash(),
        dev_package_names: installed.dev_package_names().to_vec(),
        is_platform_req_ignored: platform_filter::autoload(args.ignore_platform_reqs, &args.ignore_platform_req),
        dry_run: args.dry_run,
        ..Default::default()
    };
    let class_map = dump(c, &packages, &opts, run_scripts && !args.dry_run, dev_mode.unwrap_or(installed.dev_mode().unwrap_or(true)))?;

    let n = class_map.len();
    if authoritative {
        c.io.write(&format!("<info>Generated optimized autoload files (authoritative) containing {n} classes</info>"));
    } else if optimize {
        c.io.write(&format!("<info>Generated optimized autoload files containing {n} classes</info>"));
    } else {
        c.io.write("<info>Generated autoload files</info>");
    }

    if missing || (args.strict_psr && !class_map.psr_violations().is_empty()) {
        return Ok(1);
    }
    if args.strict_ambiguous && !class_map.ambiguous_classes(false).is_empty() {
        return Ok(2);
    }
    Ok(0)
}

/// `AutoloadGenerator::dump()` with its `pre-autoload-dump`/`post-autoload-dump` events.
pub fn dump(
    c: &mut Composer,
    packages: &[komposer_package::PackageRef],
    opts: &AutoloadOptions,
    run_scripts: bool,
    dev_mode: bool,
) -> Result<komposer_autoload::ClassMap> {
    // SAFETY: single-threaded at this point; scripts read COMPOSER_DEV_MODE
    unsafe { std::env::set_var("COMPOSER_DEV_MODE", if dev_mode { "1" } else { "0" }) };
    let scripts = c.scripts(run_scripts);
    scripts.dispatch("pre-autoload-dump", &[])?;
    let with_paths: Vec<_> = packages.iter().map(|p| (p.clone(), c.install_path(p))).collect();
    let class_map = komposer_autoload::dump(&c.config, &c.root, &with_paths, &c.dir, opts, &c.io)?;
    scripts.dispatch("post-autoload-dump", &[])?;
    Ok(class_map)
}
