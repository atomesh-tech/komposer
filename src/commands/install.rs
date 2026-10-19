//! `install` and `update`, and the option handling they share with `require`/`remove`.

use crate::cli::{InstallArgs, InstallFlags, UpdateArgs};
use crate::composer::Composer;
use crate::installer::Installer;
use anyhow::{Result, bail};
use indexmap::IndexMap;
use komposer_package::config::Config;
use komposer_package::is_platform_package;
use komposer_package::operation::Operation;
use komposer_semver::{Constraint, have_intersections};
use komposer_solver::{PlatformRequirementFilter, PrettyConstraint, UpdateAllowTransitive};
use regex::Regex;
use serde_json::Value;

pub fn install(c: &mut Composer, args: &InstallArgs, run_scripts: bool) -> Result<i32> {
    deprecated_flags(c, args.dev, args.flags.no_suggest);
    if !args.packages.is_empty() {
        let list = args.packages.join(" ");
        c.io.error(&format!("Invalid argument {list}. Use \"composer require {list}\" instead to add packages to your composer.json."));
        return Ok(1);
    }
    if args.no_install {
        c.io.error(
            "Invalid option \"--no-install\". Use \"composer update --no-install\" instead if you are trying to update the composer.lock file.",
        );
        return Ok(1);
    }
    let mut inst = Installer::new(c);
    configure(&mut inst, &args.flags, args.strict_psr_autoloader)?;
    inst.run_scripts = run_scripts;
    inst.download_only = args.download_only;
    inst.dev_mode = !args.no_dev;
    inst.dump_autoloader = !args.no_autoloader;
    // install only audits when asked to
    inst.audit = args.audit;
    inst.error_on_audit = args.audit;
    inst.run()
}

pub fn update(c: &mut Composer, args: &UpdateArgs, run_scripts: bool) -> Result<i32> {
    deprecated_flags(c, args.dev, args.flags.no_suggest);

    // `vendor/name:constraint` arguments add temporary constraints
    let mut packages = args.packages.clone();
    let pair = Regex::new(r"\S+[ =:]\S+").unwrap();
    let name_of_pair = Regex::new(r"^([^ =:]+)[ =:].*$").unwrap();
    let mut reqs: Vec<String> = args.with.clone();
    for p in packages.iter_mut() {
        if pair.is_match(p) {
            reqs.push(p.clone());
            *p = name_of_pair.replace(p, "$1").into_owned();
        }
    }
    let reqs = format_requirements(&reqs)?;

    let root = &mut c.root;
    let links: Vec<(String, String)> = reqs.iter().map(|(n, v)| (n.clone(), v.clone())).collect();
    komposer_package::loader::extract_stability_flags(&links, root.minimum_stability, &mut root.stability_flags);
    komposer_package::loader::extract_references(&links, &mut root.references);

    let root_requirements: Vec<_> = root.package.requires.iter().chain(&root.package.dev_requires).cloned().collect();
    let mut temporary: IndexMap<String, PrettyConstraint> = IndexMap::new();
    for (package, constraint) in &reqs {
        let package = package.to_lowercase();
        let parsed = komposer_semver::parse_constraints(constraint)?;
        if package.contains('*') {
            let re = package_name_regex(&package);
            for req in root_requirements.iter().filter(|r| re.is_match(&r.target)) {
                temporary.insert(req.target.clone(), PrettyConstraint::new(parsed.clone(), constraint));
                if !have_intersections(&parsed, &req.constraint) {
                    c.io.error(&format!(
                        "The temporary constraint \"{constraint}\" for \"{package}\" matching \"{}\" must be a subset of the constraint in your composer.json ({})",
                        req.target, req.pretty_constraint
                    ));
                    return Ok(1);
                }
            }
        } else {
            temporary.insert(package.clone(), PrettyConstraint::new(parsed.clone(), constraint));
            if let Some(req) = root_requirements.iter().find(|r| r.target == package)
                && !have_intersections(&parsed, &req.constraint)
            {
                c.io.error(&format!(
                    "The temporary constraint \"{constraint}\" for \"{package}\" must be a subset of the constraint in your composer.json ({})",
                    req.pretty_constraint
                ));
                c.io.write(&format!(
                    "<info>Run `composer require {package}` or `composer require {package}:{constraint}` instead to replace the constraint</info>"
                ));
                return Ok(1);
            }
        }
    }

    if args.patch_only {
        if !c.locker.is_locked()? {
            bail!("patch-only can only be used with a lock file present");
        }
        let patch = Regex::new(r"^(\d+\.\d+\.\d+)").unwrap();
        for p in c.locker.locked_repository(true)?.canonical() {
            if p.is_dev() {
                continue;
            }
            let Some(m) = patch.captures(&p.version) else { continue };
            let tilde = format!("~{}", &m[1]);
            let constraint = komposer_semver::parse_constraints(&tilde)?;
            let combined = match temporary.get(&p.name) {
                Some(existing) => {
                    let multi = Constraint::multi(vec![existing.constraint.clone(), constraint], true);
                    PrettyConstraint::plain(multi)
                }
                None => PrettyConstraint::new(constraint, tilde),
            };
            temporary.insert(p.name.clone(), combined);
        }
    }

    if args.root_reqs {
        let root = &c.root.package;
        let mut requires: Vec<String> = root.requires.iter().map(|l| l.target.clone()).collect();
        if !args.no_dev {
            requires.extend(root.dev_requires.iter().map(|l| l.target.clone()));
        }
        packages = if packages.is_empty() { requires } else { packages.into_iter().filter(|p| requires.contains(p)).collect() };
    }

    let filtered: Vec<String> = packages.iter().filter(|p| !matches!(p.as_str(), "lock" | "nothing" | "mirrors")).cloned().collect();
    let update_mirrors = args.lock || filtered.len() != packages.len();
    if update_mirrors && !filtered.is_empty() {
        c.io.error("You cannot simultaneously update only a selection of packages and regenerate the lock file metadata.");
        return Ok(255);
    }

    let minimal = args.minimal_changes || env_flag("COMPOSER_MINIMAL_CHANGES") || c.config.get_bool("update-with-minimal-changes");
    let mut inst = Installer::new(c);
    configure(&mut inst, &args.flags, args.strict_psr_autoloader)?;
    inst.run_scripts = run_scripts;
    inst.dev_mode = !args.no_dev;
    inst.dump_autoloader = !args.no_autoloader;
    inst.update = true;
    inst.install = !args.no_install;
    inst.update_mirrors = update_mirrors;
    inst.update_allow_list = (!filtered.is_empty()).then_some(filtered);
    inst.update_allow_transitive = transitive(args.with_all_dependencies, args.with_dependencies);
    inst.prefer_stable = args.prefer_stable || env_flag("COMPOSER_PREFER_STABLE");
    inst.prefer_lowest = args.prefer_lowest || env_flag("COMPOSER_PREFER_LOWEST");
    inst.temporary_constraints = temporary;
    inst.audit = !(args.no_audit || env_flag("COMPOSER_NO_AUDIT"));
    inst.minimal_update = minimal;
    let status = inst.run()?;
    let updated: Vec<String> = inst
        .lock_transaction
        .as_ref()
        .map(|t| {
            t.operations()
                .iter()
                .filter(|op| matches!(op, Operation::Install(_) | Operation::Update { .. }))
                .map(|op| op.package().name.clone())
                .collect()
        })
        .unwrap_or_default();
    if status != 0 || args.lock {
        return Ok(status);
    }

    let bump = match &args.bump_after_update {
        Some(mode) => Some(mode.clone()),
        None => match c.config.raw("bump-after-update") {
            Some(Value::String(mode)) => Some(mode.clone()),
            Some(v) if komposer_package::loader::php_truthy(v) => Some("all".into()),
            _ => None,
        },
    };
    let Some(mode) = bump.filter(|_| !updated.is_empty()) else { return Ok(status) };
    c.io.write_error("<info>Bumping dependencies</info>");
    crate::commands::bump::do_bump(c, mode == "dev", mode == "no-dev", args.flags.dry_run, &updated, "--bump-after-update=dev")
}

/// `Request::UPDATE_*` from `-W` / `-w` (and their env vars).
pub fn transitive(all: bool, deps: bool) -> UpdateAllowTransitive {
    if all || env_flag("COMPOSER_WITH_ALL_DEPENDENCIES") {
        UpdateAllowTransitive::WithDeps
    } else if deps || env_flag("COMPOSER_WITH_DEPENDENCIES") {
        UpdateAllowTransitive::WithDepsNoRootRequire
    } else {
        UpdateAllowTransitive::OnlyListed
    }
}

/// The installer options every installing command shares.
pub fn configure(inst: &mut Installer, flags: &InstallFlags, strict_psr: bool) -> Result<()> {
    let config = &inst.c.config;
    let (prefer_source, prefer_dist) = preferred_install(config, flags)?;
    let optimize = flags.optimize_autoloader || config.get_bool("optimize-autoloader");
    let authoritative = flags.classmap_authoritative || config.get_bool("classmap-authoritative");
    let apcu = flags.apcu_autoloader_prefix.is_some() || flags.apcu_autoloader || config.get_bool("apcu-autoloader");
    if strict_psr && !optimize && !authoritative {
        bail!(
            "--strict-psr-autoloader mode only works with optimized autoloader, use --optimize-autoloader or --classmap-authoritative if you want a strict return value."
        );
    }
    inst.dry_run = flags.dry_run;
    inst.prefer_source = prefer_source;
    inst.prefer_dist = prefer_dist;
    // classmap-authoritative implies an optimized autoloader
    inst.optimize_autoloader = optimize || authoritative;
    inst.classmap_authoritative = authoritative;
    inst.strict_psr_autoloader = strict_psr;
    inst.apcu = apcu;
    inst.apcu_prefix = flags.apcu_autoloader_prefix.clone();
    inst.platform_filter = platform_filter(flags.ignore_platform_reqs, &flags.ignore_platform_req);
    inst.autoload_platform_ignore = (flags.ignore_platform_reqs, flags.ignore_platform_req.clone());
    inst.no_blocking = flags.no_blocking;
    inst.audit_format = flags.audit_format.clone();
    Ok(())
}

/// `getPlatformRequirementFilter()`.
pub fn platform_filter(all: bool, list: &[String]) -> PlatformRequirementFilter {
    if all {
        PlatformRequirementFilter::IgnoreAll
    } else if !list.is_empty() {
        PlatformRequirementFilter::ignore_list(list)
    } else {
        PlatformRequirementFilter::IgnoreNothing
    }
}

/// `getPreferredInstallOptions()`: (prefer source, prefer dist).
pub fn preferred_install(config: &Config, flags: &InstallFlags) -> Result<(bool, bool)> {
    let (mut source, mut dist) = match config.get("preferred-install").as_ref().and_then(|v| v.as_str()) {
        Some("source") => (true, false),
        Some("dist") => (false, true),
        _ => (false, false),
    };
    let (mut flag_source, mut flag_dist) = (flags.prefer_source, flags.prefer_dist);
    if let Some(pref) = &flags.prefer_install {
        if flags.prefer_source {
            bail!("--prefer-source can not be used together with --prefer-install");
        }
        if flags.prefer_dist {
            bail!("--prefer-dist can not be used together with --prefer-install");
        }
        match pref.as_str() {
            "dist" => flag_dist = true,
            "source" => flag_source = true,
            "auto" => (source, dist) = (false, false),
            other => bail!("--prefer-install accepts one of \"dist\", \"source\" or \"auto\", got {other}"),
        }
    }
    if flag_source || flag_dist {
        (source, dist) = (flag_source, flag_dist);
    }
    Ok((source, dist))
}

/// `formatRequirements()`: `name:constraint` strings to (name, constraint).
pub fn format_requirements(reqs: &[String]) -> Result<Vec<(String, String)>> {
    let pairs = komposer_semver::parse_name_version_pairs(reqs, is_platform_package);
    let mut out: Vec<(String, String)> = vec![];
    for (name, version) in pairs {
        let Some(version) = version else { bail!("Option --with requires a version constraint for {name}, e.g. {name}:^1.0") };
        out.retain(|(n, _)| *n != name);
        out.push((name, version));
    }
    Ok(out)
}

/// `BasePackage::packageNameToRegexp()`.
pub fn package_name_regex(pattern: &str) -> Regex {
    Regex::new(&format!("(?i)^{}$", regex::escape(pattern).replace(r"\*", ".*"))).unwrap()
}

pub fn env_flag(var: &str) -> bool {
    std::env::var(var).is_ok_and(|v| !v.is_empty() && v != "0")
}

fn deprecated_flags(c: &Composer, dev: bool, no_suggest: bool) {
    if dev {
        c.io.warning("You are using the deprecated option \"--dev\". It has no effect and will break in Composer 3.");
    }
    if no_suggest {
        c.io.warning("You are using the deprecated option \"--no-suggest\". It has no effect and will break in Composer 3.");
    }
}
