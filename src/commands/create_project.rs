//! `create-project`.

use crate::cli::CreateProjectArgs;
use crate::commands::install::{platform_filter, preferred_install};
use crate::commands::require::edit_json;
use crate::composer::Composer;
use crate::installer::{Installer, effective_platform_filter};
use crate::suggest::Suggestions;
use anyhow::{Context, Result, bail};
use komposer_installer::{InstallationManager, InstalledRepo};
use komposer_package::config::Config;
use komposer_package::io::Io;
use komposer_package::operation::Operation;
use komposer_package::repository::Repository;
use komposer_package::{DisplayMode, LinkType};
use komposer_repository::http::HttpClient;
use komposer_repository::platform::PlatformRepository;
use komposer_semver::{DEFAULT_BRANCH_ALIAS, Stability};
use komposer_solver::{PlatformRequirementFilter, RepositorySet, VersionSelector};
use regex::Regex;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub fn run(io: &Arc<Io>, args: &CreateProjectArgs, run_scripts: bool) -> Result<i32> {
    let cwd = std::env::current_dir()?;
    let mut config = Config::create(&cwd)?;
    let mut flags = args.flags.clone();
    // --keep-vcs needs a source install
    if args.keep_vcs && !flags.prefer_dist && flags.prefer_install.is_none() {
        flags.prefer_source = true;
    }
    let (prefer_source, prefer_dist) = preferred_install(&config, &flags)?;
    if args.dev {
        io.warning("You are using the deprecated option \"dev\". Dev packages are installed by default now.");
    }
    let user_filter = platform_filter(flags.ignore_platform_reqs, &flags.ignore_platform_req);
    let repositories: Vec<String> = if !args.repository.is_empty() { args.repository.clone() } else { args.repository_url.iter().cloned().collect() };

    let mut suggestions = Suggestions::default();
    let mut installed_from_vcs = false;
    if let Some(package_name) = &args.package {
        installed_from_vcs =
            install_root_package(io, &mut config, args, package_name, &repositories, &user_filter, prefer_source, prefer_dist, &mut suggestions)?;
    }

    let mut c = Composer::load(io.clone())?;
    let mut flush_lock = false;
    if !repositories.is_empty() && args.add_repository {
        // ponytail: JsonManipulator::addRepository is not ported; add repositories to composer.json by hand
        io.warning("--add-repository is not supported by komposer yet, add the repository to composer.json manually.");
    }
    if !args.require.is_empty() {
        for req in &args.require {
            let (name, constraint) = req.split_once(':').unwrap_or((req, "*"));
            edit_json(
                &c.file,
                |m| m.add_link("require", name, constraint, false),
                |data| {
                    if let Some(Value::Object(r)) = data.get_mut("require") {
                        r.insert(name.into(), constraint.into());
                    }
                },
            )?;
        }
        flush_lock = true;
        c = Composer::load(io.clone())?;
    }
    if flush_lock && Path::new("composer.lock").is_file() {
        std::fs::remove_file("composer.lock")?;
    }

    c.scripts(run_scripts).dispatch("post-root-package-install", &[])?;

    let (prefer_source, prefer_dist) = preferred_install(&c.config, &flags)?;
    if !args.no_install {
        let locked = c.locker.is_locked()?;
        let optimize = c.config.get_bool("optimize-autoloader");
        let authoritative = c.config.get_bool("classmap-authoritative");
        let apcu = c.config.get_bool("apcu-autoloader");
        let mut inst = Installer::new(&mut c);
        inst.run_scripts = run_scripts;
        inst.prefer_source = prefer_source;
        inst.prefer_dist = prefer_dist;
        inst.dev_mode = !args.no_dev;
        inst.platform_filter = user_filter.clone();
        inst.autoload_platform_ignore = (flags.ignore_platform_reqs, flags.ignore_platform_req.clone());
        inst.optimize_autoloader = optimize || authoritative;
        inst.classmap_authoritative = authoritative;
        inst.apcu = apcu;
        inst.no_blocking = flags.no_blocking;
        inst.audit = !(args.no_audit || crate::commands::install::env_flag("COMPOSER_NO_AUDIT"));
        inst.audit_format = flags.audit_format.clone();
        inst.update = !locked;
        inst.suggestions = std::mem::take(&mut suggestions);
        let status = inst.run()?;
        if status != 0 {
            return Ok(status);
        }
    }

    let mut has_vcs = installed_from_vcs;
    if !args.keep_vcs && installed_from_vcs {
        // non-interactive Composer removes the VCS metadata too
        for vcs in [".svn", "_svn", "CVS", "_darcs", ".arch-params", ".monotone", ".bzr", ".git", ".hg", ".fslckout", "_FOSSIL_"] {
            let dir = c.dir.join(vcs);
            if dir.is_dir()
                && let Err(e) = komposer_installer::fs::remove_directory(&dir.to_string_lossy())
            {
                io.error(&format!("An error occurred while removing the VCS metadata: {e}"));
            }
        }
        has_vcs = false;
    }
    if !has_vcs {
        let package = c.root.package.clone();
        for kind in LinkType::ALL {
            for link in package.links(kind).iter().filter(|l| l.pretty_constraint == "self.version") {
                let version = package.pretty_version.clone();
                edit_json(
                    &c.file,
                    |m| m.add_link(kind.key(), &link.target, &version, false),
                    |data| {
                        if let Some(Value::Object(r)) = data.get_mut(kind.key()) {
                            r.insert(link.target.clone(), version.clone().into());
                        }
                    },
                )?;
            }
        }
    }

    c.scripts(run_scripts).dispatch("post-create-project-cmd", &[])?;
    std::env::set_current_dir(&cwd)?;
    Ok(0)
}

/// `installRootPackage()`: download the project package into the target directory.
#[allow(clippy::too_many_arguments)]
fn install_root_package(
    io: &Arc<Io>,
    config: &mut Config,
    args: &CreateProjectArgs,
    package_name: &str,
    repositories: &[String],
    user_filter: &PlatformRequirementFilter,
    prefer_source: bool,
    prefer_dist: bool,
    suggestions: &mut Suggestions,
) -> Result<bool> {
    let cwd = std::env::current_dir()?;
    let pairs = komposer_semver::parse_name_version_pairs(&[package_name.to_string()], komposer_package::is_platform_package);
    let (name, pair_version) = pairs.into_iter().next().context("Not enough arguments (missing: \"package\").")?;
    let name = name.to_lowercase();
    let version = args.version.clone().or(pair_version);

    let directory = match &args.directory {
        Some(d) => d.clone(),
        None => cwd.join(name.rsplit('/').next().unwrap_or(&name)),
    };
    let directory = PathBuf::from(directory.to_string_lossy().trim_end_matches(['/', '\\']));
    let directory = if directory.is_absolute() { directory } else { cwd.join(directory) };

    *config = Config::create(&directory)?;
    if args.no_secure_http {
        config.merge_config(&serde_json::json!({"secure-http": false}));
    }
    let shortest = komposer_installer::fs::find_shortest_path(&cwd.to_string_lossy(), &directory.to_string_lossy(), true, false)?;
    io.write_error(&format!("<info>Creating a \"{package_name}\" project at \"{shortest}\"</info>"));

    if directory.exists() {
        if !directory.is_dir() {
            bail!("Cannot create project directory at \"{}\", it exists as a file.", directory.display());
        }
        if std::fs::read_dir(&directory)?.next().is_some() {
            bail!("Project directory \"{}\" is not empty.", directory.display());
        }
    }

    let stability = match (&args.stability, &version) {
        (Some(s), _) => s.clone(),
        (None, None) => "stable".into(),
        (None, Some(v)) => match Regex::new(r"(?i)^[^,\s]*?@(stable|RC|beta|alpha|dev)$").unwrap().captures(v) {
            Some(m) => m[1].to_string(),
            None => komposer_semver::parse_stability(v).as_str().to_string(),
        },
    };
    let stability: Stability = komposer_semver::normalize_stability(&stability)
        .map_err(|_| anyhow::anyhow!("Invalid stability provided ({stability}), must be one of: stable, RC, beta, alpha, dev"))?;

    let http = HttpClient::new(io.clone(), config)?;
    let mut set = RepositorySet::new(stability);
    if repositories.is_empty() {
        for repo in komposer_repository::factory::create_repositories(&http, &config.repositories())? {
            set.add_repository(repo);
        }
    } else {
        for repo in repositories {
            let mut repo_config = komposer_repository::factory::config_from_string(repo)?;
            if matches!(repo_config.as_object().map(|m| m.len()), Some(1))
                && (repo_config.get("packagist") == Some(&Value::Bool(false)) || repo_config.get("packagist.org") == Some(&Value::Bool(false)))
            {
                continue;
            }
            if repo_config.get("type").and_then(Value::as_str) == Some("path") && repo_config.pointer("/options/symlink").is_none() {
                repo_config["options"]["symlink"] = Value::Bool(false);
            }
            set.add_repository(komposer_repository::factory::create_repository(&http, &repo_config, repo)?);
        }
    }

    let platform = PlatformRepository::from_config(config)?;
    let filter = effective_platform_filter(config, user_filter, &args.flags.ignore_platform_req, io);
    let selector = VersionSelector::new(&set, Some(&platform as &dyn Repository))?;
    let show = |_: &komposer_package::Package| true;
    let Some(mut package) = selector.find_best_candidate(&name, version.as_deref(), stability, &filter, 0, Some(io), &show)? else {
        let what = match &version {
            Some(v) => format!("version {v}"),
            None => format!("stability {stability}"),
        };
        let message = format!("Could not find package {name} with {what}");
        let ignore_all = PlatformRequirementFilter::IgnoreAll;
        if !matches!(filter, PlatformRequirementFilter::IgnoreAll)
            && selector.find_best_candidate(&name, version.as_deref(), stability, &ignore_all, 0, None, &show)?.is_some()
        {
            bail!("{message} in a version installable using your PHP version, PHP extensions and Composer version.");
        }
        bail!("{message}.");
    };

    std::fs::create_dir_all(&directory)?;
    if package.is_alias() && package.pretty_version == DEFAULT_BRANCH_ALIAS {
        package = package.alias_of.clone().unwrap();
    }
    io.write_error(&format!("<info>Installing {} ({})</info>", package.name, package.full_pretty_version_with(false, DisplayMode::SourceRefIfDev)));
    if let Some(of) = &package.alias_of {
        package = of.clone();
    }

    let fetch = crate::installer::fetcher(http);
    let mut im = InstallationManager::new(config, io, &fetch);
    im.prefer_source = prefer_source;
    im.prefer_dist = prefer_dist;
    im.set_project_dir(&directory.to_string_lossy());
    let mut repo = InstalledRepo::in_memory();
    // Composer installs the root package without an event dispatcher
    im.execute(&mut repo, &[Operation::Install(package.clone())], true, false, None)?;
    suggestions.add_from_package(&package);

    let installed = repo.packages.all().iter().find(|p| p.name == package.name).cloned().unwrap_or(package.clone());
    let from_vcs = installed.installation_source.as_deref() == Some("source");

    io.write_error(&format!("<info>Created project in {}</info>", directory.display()));
    std::env::set_current_dir(&directory)?;
    // SAFETY: single-threaded here
    unsafe {
        if directory.join("composer.json").exists() && std::env::var_os("COMPOSER").is_some() {
            std::env::remove_var("COMPOSER");
        }
        std::env::set_var("COMPOSER_ROOT_VERSION", &installed.pretty_version);
    }
    Ok(from_vcs)
}
