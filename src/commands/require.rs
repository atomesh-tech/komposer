//! `require` and `remove`: edit composer.json, then run a partial update.

use crate::cli::{RemoveArgs, RequireArgs};
use crate::commands::install::{configure, env_flag, format_requirements, package_name_regex, platform_filter};
use crate::composer::{Composer, composer_file, lock_file};
use crate::installer::Installer;
use crate::json_manipulator::JsonManipulator;
use anyhow::{Context, Result, bail};
use komposer_package::io::Io;
use komposer_package::json;
use komposer_package::repository::Repository;
use komposer_repository::platform::PlatformRepository;
use komposer_semver::Stability;
use komposer_solver::{RepositorySet, UpdateAllowTransitive, VersionSelector};
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// `JsonConfigSource::manipulateJson()`: a formatting-preserving edit, or a full rewrite if that fails.
pub fn edit_json(path: &Path, edit: impl Fn(&mut JsonManipulator) -> bool, fallback: impl FnOnce(&mut Map<String, Value>)) -> Result<()> {
    let contents = std::fs::read_to_string(path)?;
    let mut m = JsonManipulator::new(&contents)?;
    if edit(&mut m) {
        std::fs::write(path, m.contents())?;
        return Ok(());
    }
    let mut data = json::parse_object(&contents).with_context(|| format!("{} must contain an object", path.display()))?;
    fallback(&mut data);
    json::write(path, &Value::Object(data))
}

/// Restores composer.json (and composer.lock) when a require/remove fails.
struct Backup {
    file: PathBuf,
    lock: PathBuf,
    composer: String,
    lock_contents: Option<String>,
    newly_created: bool,
}

impl Backup {
    fn revert(&self, io: &Io, action: &str) {
        if self.newly_created {
            io.write_error(&format!("\n<error>{action} failed, deleting {}.</error>", self.file.display()));
            let _ = std::fs::remove_file(&self.file);
            let _ = std::fs::remove_file(&self.lock);
            return;
        }
        let msg = match &self.lock_contents {
            Some(_) => format!(" and {} to their ", self.lock.display()),
            None => " to its ".into(),
        };
        io.write_error(&format!("\n<error>{action} failed, reverting {}{msg}original content.</error>", self.file.display()));
        let _ = std::fs::write(&self.file, &self.composer);
        if let Some(lock) = &self.lock_contents {
            let _ = std::fs::write(&self.lock, lock);
        }
    }
}

pub fn require(io: &Arc<Io>, args: &RequireArgs, run_scripts: bool) -> Result<i32> {
    if args.flags.no_suggest {
        io.warning("You are using the deprecated option \"--no-suggest\". It has no effect and will break in Composer 3.");
    }
    let file = composer_file()?;
    let newly_created = !file.exists();
    if newly_created || std::fs::metadata(&file)?.len() == 0 {
        std::fs::write(&file, "{\n}\n").with_context(|| format!("{} could not be created.", file.display()))?;
    }
    let lock = lock_file(&file);
    let backup = Backup {
        composer: std::fs::read_to_string(&file)?,
        lock_contents: std::fs::read_to_string(&lock).ok(),
        file: file.clone(),
        lock,
        newly_created,
    };

    let c = Composer::load(io.clone())?;
    if args.fixed {
        let package_type = c.root.raw.get("type").and_then(Value::as_str).filter(|t| !t.is_empty()).unwrap_or("library");
        if package_type != "project" && !args.dev {
            io.error(
                "The \"--fixed\" option is only allowed for packages with a \"project\" type or for dev dependencies to prevent possible misuses.",
            );
            if !c.root.raw.contains_key("type") {
                io.error("If your package is not a library, you can explicitly specify the \"type\" by using \"composer config type project\".");
            }
            return Ok(1);
        }
    }
    if args.packages.is_empty() {
        bail!("No package specified. Usage: komposer require vendor/package[:constraint]");
    }
    if let Some(a) = args.packages.iter().find(|p| p.eq_ignore_ascii_case("as")) {
        bail!("Cannot use \"{a}\" as a separate argument. Quote the inline alias as one argument, e.g. \"vendor/package:dev-main as 1.2.x-dev\".");
    }

    let preferred_stability = if c.root.prefer_stable { Stability::Stable } else { c.root.minimum_stability };
    let pairs = komposer_semver::parse_name_version_pairs(&args.packages, komposer_package::is_platform_package);
    let mut requirements: Vec<(String, String)> = vec![];
    let mut to_guess: Vec<String> = vec![];
    let too_strict = regex::Regex::new(r"^\d+(\.\d+)?$").unwrap();
    for (name, version) in pairs {
        let version = match version {
            Some(v) => {
                if too_strict.is_match(&v) {
                    io.warning(&format!("The \"{v}\" constraint for \"{name}\" appears too strict and will likely not match what you want. See https://getcomposer.org/constraints"));
                }
                v
            }
            // without an update the constraint can not be guessed from the solver's result
            None if args.no_update || args.fixed => {
                let v = best_version(&c, &name, preferred_stability, args)?;
                io.write_error(&format!("Using version <info>{v}</info> for <info>{name}</info>"));
                v
            }
            None => {
                to_guess.push(name.clone());
                "*".into()
            }
        };
        requirements.retain(|(n, _)| *n != name);
        requirements.push((name, version));
    }
    let requirements = format_requirements(&requirements.iter().map(|(n, v)| format!("{n}:{v}")).collect::<Vec<_>>())?;

    let (require_key, remove_key) = if args.dev { ("require-dev", "require") } else { ("require", "require-dev") };
    for (package, constraint) in &requirements {
        if package.to_lowercase() == c.root.package.name {
            io.error(&format!("Root package '{package}' cannot require itself in its composer.json"));
            return Ok(1);
        }
        if constraint != "self.version" {
            komposer_semver::parse_constraints(constraint)?;
        }
    }
    for (package, _) in &requirements {
        if c.root.raw.get(remove_key).and_then(Value::as_object).is_some_and(|m| m.keys().any(|k| k.eq_ignore_ascii_case(package))) {
            io.warning(&format!(
                "{package} is currently present in the {remove_key} key and you ran the command {} the --dev flag, which will move it to the {require_key} key.",
                if args.dev { "with" } else { "without" }
            ));
        }
    }

    let sort_packages = args.sort_packages || c.config.get_bool("sort-packages");
    let first_require =
        newly_created || ["require", "require-dev"].iter().all(|k| c.root.raw.get(*k).and_then(Value::as_object).is_none_or(Map::is_empty));
    if !args.flags.dry_run {
        update_file(&file, &requirements, require_key, remove_key, sort_packages)?;
    }
    io.write_error(&format!("<info>{} has been {}</info>", file.display(), if newly_created { "created" } else { "updated" }));
    if args.no_update {
        return Ok(0);
    }

    let result = do_require_update(io, args, &requirements, require_key, first_require, run_scripts);
    let status = match result {
        Ok(s) => s,
        Err(e) => {
            backup.revert(io, "Installation");
            return Err(e);
        }
    };
    if status != 0 && status != crate::installer::ERROR_AUDIT_FAILED {
        if status == komposer_solver::SolverProblems::EXIT_CODE
            && let Some((name, _)) = args.packages.iter().map(|p| (p, ())).find(|(p, _)| !p.contains([':', '=', ' ']))
        {
            io.write_error(&format!(
                "You can also try re-running composer require with an explicit version constraint, e.g. \"composer require {name}:*\" to figure out if any version is installable, or \"composer require {name}:^2.1\" if you know which you need."
            ));
        }
        backup.revert(io, "Installation");
        return Ok(status);
    }
    if status == 0 && !to_guess.is_empty() {
        return update_requirements_after_resolution(io, &to_guess, require_key, remove_key, sort_packages, args);
    }
    if args.flags.dry_run && newly_created {
        let _ = std::fs::remove_file(&file);
    }
    Ok(status)
}

fn do_require_update(
    io: &Arc<Io>,
    args: &RequireArgs,
    requirements: &[(String, String)],
    require_key: &str,
    first_require: bool,
    run_scripts: bool,
) -> Result<i32> {
    let mut c = Composer::load(io.clone())?;
    if args.flags.dry_run {
        // the file was not written, so apply the change to the loaded root package
        let root = &mut c.root;
        let kind = if require_key == "require" { komposer_package::LinkType::Require } else { komposer_package::LinkType::DevRequire };
        let remove_kind =
            if kind == komposer_package::LinkType::Require { komposer_package::LinkType::DevRequire } else { komposer_package::LinkType::Require };
        let links: Map<String, Value> = requirements.iter().map(|(n, v)| (n.clone(), Value::String(v.clone()))).collect();
        let new_links = komposer_package::loader::parse_links(&root.package.name, &root.package.pretty_version, kind, &links, false)?;
        for link in new_links {
            let list = root.package.links_mut(kind);
            list.retain(|l| l.target != link.target);
            list.push(link);
        }
        for (name, _) in requirements {
            root.package.links_mut(remove_kind).retain(|l| l.target != name.to_lowercase());
        }
        komposer_package::loader::extract_references(requirements, &mut root.references);
        komposer_package::loader::extract_stability_flags(requirements, root.minimum_stability, &mut root.stability_flags);
    }

    let mut transitive_mode = UpdateAllowTransitive::OnlyListed;
    let mut flags = String::new();
    if args.update_with_all_dependencies || env_flag("COMPOSER_WITH_ALL_DEPENDENCIES") {
        transitive_mode = UpdateAllowTransitive::WithDeps;
        flags.push_str(" --with-all-dependencies");
    } else if args.update_with_dependencies || env_flag("COMPOSER_WITH_DEPENDENCIES") {
        transitive_mode = UpdateAllowTransitive::WithDepsNoRootRequire;
        flags.push_str(" --with-dependencies");
    }
    let names: Vec<String> = requirements.iter().map(|(n, _)| n.clone()).collect();
    io.write_error(&format!("<info>Running composer update {}{flags}</info>", names.join(" ")));

    let locked = c.locker.is_locked()?;
    let minimal = args.minimal_changes || env_flag("COMPOSER_MINIMAL_CHANGES") || c.config.get_bool("update-with-minimal-changes");
    let mut inst = Installer::new(&mut c);
    configure(&mut inst, &args.flags, false)?;
    inst.run_scripts = run_scripts;
    inst.dev_mode = !args.update_no_dev;
    inst.update = true;
    inst.install = !args.no_install;
    inst.update_allow_transitive = transitive_mode;
    inst.prefer_stable = args.prefer_stable || env_flag("COMPOSER_PREFER_STABLE");
    inst.prefer_lowest = args.prefer_lowest || env_flag("COMPOSER_PREFER_LOWEST");
    inst.audit = !(args.no_audit || env_flag("COMPOSER_NO_AUDIT"));
    inst.minimal_update = minimal;
    if !first_require && locked {
        inst.update_allow_list = Some(names.iter().map(|n| n.to_lowercase()).collect());
    }
    inst.run()
}

/// `updateRequirementsAfterResolution()`: replace the temporary `*` by the recommended constraint.
fn update_requirements_after_resolution(
    io: &Arc<Io>,
    names: &[String],
    require_key: &str,
    remove_key: &str,
    sort: bool,
    args: &RequireArgs,
) -> Result<i32> {
    let mut c = Composer::load(io.clone())?;
    let repo: Arc<dyn Repository> = if c.locker.is_locked()? { Arc::new(c.locker.locked_repository(true)?) } else { Arc::new(c.installed()?) };
    let set = RepositorySet::new(Stability::Stable);
    let selector = VersionSelector::new(&set, None)?;
    let mut requirements = vec![];
    for name in names {
        let Some(package) = repo.find_package(name, &komposer_semver::Constraint::Any)? else { continue };
        let package = package.base();
        let version = if args.fixed { package.pretty_version.clone() } else { selector.find_recommended_require_version(package) };
        io.write_error(&format!("Using version <info>{version}</info> for <info>{name}</info>"));
        if version.starts_with("dev-") && !matches!(version.as_str(), "dev-main" | "dev-master" | "dev-trunk" | "dev-latest") {
            io.warning(&format!(
                "Version {version} looks like it may be a feature branch which is unlikely to keep working in the long run and may be in an unstable state"
            ));
        }
        requirements.push((name.clone(), version));
    }
    if !args.flags.dry_run {
        update_file(&c.file, &requirements, require_key, remove_key, sort)?;
        if c.locker.is_locked()? && c.config.get_bool("lock") {
            let contents = std::fs::read_to_string(&c.file)?;
            c.locker.update_hash(&contents)?;
        }
    }
    Ok(0)
}

/// `updateFile()`: add the links cleanly, or rewrite composer.json.
fn update_file(file: &Path, new: &[(String, String)], require_key: &str, remove_key: &str, sort: bool) -> Result<()> {
    edit_json(
        file,
        |m| {
            for (package, constraint) in new {
                if !m.add_link(require_key, package, constraint, sort) || !m.remove_sub_node(remove_key, package) {
                    return false;
                }
            }
            m.remove_main_key_if_empty(remove_key)
        },
        |data| {
            for (package, version) in new {
                let req = data.entry(require_key).or_insert_with(|| Value::Object(Map::new()));
                if let Value::Object(r) = req {
                    r.insert(package.clone(), Value::String(version.clone()));
                }
                if let Some(Value::Object(r)) = data.get_mut(remove_key) {
                    r.shift_remove(package);
                    if r.is_empty() {
                        data.shift_remove(remove_key);
                    }
                }
            }
        },
    )
}

/// `findBestVersionAndNameForPackage()`: the recommended constraint for the best available version.
fn best_version(c: &Composer, name: &str, preferred: Stability, args: &RequireArgs) -> Result<String> {
    let filter = platform_filter(args.flags.ignore_platform_reqs, &args.flags.ignore_platform_req);
    let platform = PlatformRepository::from_config(&c.config)?;
    let mut set = RepositorySet::new(c.root.minimum_stability).with_stability_flags(c.root.stability_flags.iter().cloned().collect());
    for repo in c.repositories()? {
        set.add_repository(repo);
    }
    let platform_ref: Option<&dyn Repository> = komposer_repository::platform::php_detected().then_some(&platform as &dyn Repository);
    let selector = VersionSelector::new(&set, platform_ref)?;
    let show_all = |_: &komposer_package::Package| true;
    if let Some(p) = selector.find_best_candidate(name, None, preferred, &filter, 0, Some(&c.io), &show_all)? {
        return Ok(if args.fixed { p.pretty_version.clone() } else { selector.find_recommended_require_version(&p) });
    }
    if filter.is_ignored(name) {
        return Ok("*".into());
    }
    if !set.providers(name)?.is_empty() {
        return Ok("*".into());
    }
    let ignore_all = komposer_solver::PlatformRequirementFilter::IgnoreAll;
    let ignoring_all = matches!(filter, komposer_solver::PlatformRequirementFilter::IgnoreAll);
    if !ignoring_all && selector.find_best_candidate(name, None, preferred, &ignore_all, 0, None, &show_all)?.is_some() {
        bail!("Package {name} has requirements incompatible with your PHP version, PHP extensions and Composer version");
    }
    let minimum = c.root.minimum_stability;
    if selector.find_best_candidate(name, None, preferred, &filter, RepositorySet::ALLOW_UNACCEPTABLE_STABILITIES, None, &show_all)?.is_some() {
        bail!(
            "Could not find a version of package {name} matching your minimum-stability ({minimum}). Require it with an explicit version constraint allowing its desired stability."
        );
    }
    if !ignoring_all
        && let Some(candidate) =
            selector.find_best_candidate(name, None, preferred, &ignore_all, RepositorySet::ALLOW_UNACCEPTABLE_STABILITIES, None, &show_all)?
    {
        let mut additional = String::new();
        if selector.find_best_candidate(name, None, preferred, &ignore_all, 0, None, &show_all)?.is_none() {
            additional = format!(
                "\n\nAdditionally, the package was only found with a stability of \"{}\" while your minimum stability is \"{minimum}\".",
                candidate.stability
            );
        }
        bail!("Could not find package {name} in any version matching your PHP version, PHP extensions and Composer version{additional}");
    }
    let similar = find_similar(c, &set, name);
    if !similar.is_empty() {
        if similar.iter().any(|s| s == name) {
            bail!(
                "Could not find package {name}. It was however found via repository search, which indicates a consistency issue with the repository."
            );
        }
        bail!(
            "Could not find package {name}.\n\nDid you mean {}?\n    {}",
            if similar.len() > 1 { "one of these" } else { "this" },
            similar.join("\n    ")
        );
    }
    bail!(
        "Could not find a matching version of package {name}. Check the package spelling, your version constraint and that the package is available in a stability which matches your minimum-stability ({minimum})."
    );
}

/// `findSimilar()`: up to 5 not installed search results, closest names first.
fn find_similar(c: &Composer, set: &RepositorySet, name: &str) -> Vec<String> {
    let installed = c.installed().ok();
    let mut found: Vec<(usize, String)> = vec![];
    for repo in set.repositories() {
        // search errors are ignored
        for result in repo.search(name, komposer_package::repository::SearchMode::Fulltext, None).unwrap_or_default() {
            let Some(candidate) = result.get("name").and_then(Value::as_str) else { continue };
            let is_installed = installed.as_ref().is_some_and(|i| i.find_packages(candidate, None).is_ok_and(|p| !p.is_empty()));
            if !is_installed && !found.iter().any(|(_, n)| n == candidate) {
                found.push((levenshtein(name, candidate), candidate.to_string()));
            }
        }
    }
    found.sort_by_key(|(d, _)| *d);
    found.into_iter().take(5).map(|(_, n)| n).collect()
}

/// PHP `levenshtein()`.
fn levenshtein(a: &str, b: &str) -> usize {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, cb) in b.iter().enumerate() {
            cur[j + 1] = (prev[j] + usize::from(ca != cb)).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        prev = cur;
    }
    prev[b.len()]
}

pub fn remove(io: &Arc<Io>, args: &RemoveArgs, run_scripts: bool) -> Result<i32> {
    if args.packages.is_empty() && !args.unused {
        bail!("Not enough arguments (missing: \"packages\").");
    }
    let mut packages: Vec<String> = args.packages.iter().map(|p| p.to_lowercase()).collect();
    if args.unused {
        let c = Composer::load(io.clone())?;
        if !c.locker.is_locked()? {
            bail!("A valid composer.lock file is required to run this command with --unused");
        }
        let mut locked = c.locker.locked_repository(false)?.packages()?;
        let mut required: Vec<String> = c.root.package.requires.iter().chain(&c.root.package.dev_requires).map(|l| l.target.clone()).collect();
        loop {
            let before = locked.len();
            locked.retain(|p| {
                if p.names(true).iter().any(|n| required.contains(n)) {
                    required.extend(p.requires.iter().map(|l| l.target.clone()));
                    return false;
                }
                true
            });
            if locked.len() == before {
                break;
            }
        }
        packages.extend(locked.iter().map(|p| p.name.clone()));
        if packages.is_empty() {
            io.write_error("<info>No unused packages to remove</info>");
            return Ok(0);
        }
    }

    let file = composer_file()?;
    let backup = std::fs::read_to_string(&file)?;
    let composer = json::parse_object(&backup).with_context(|| format!("{} must contain an object", file.display()))?;
    let (kind, alt) = if args.dev { ("require-dev", "require") } else { ("require", "require-dev") };
    if args.update_with_dependencies {
        io.warning("You are using the deprecated option \"update-with-dependencies\". This is now default behaviour. The --no-update-with-dependencies option can be used to remove a package without its dependencies.");
    }
    let names_in =
        |key: &str| -> Vec<String> { composer.get(key).and_then(Value::as_object).map(|m| m.keys().cloned().collect()).unwrap_or_default() };
    let (in_kind, in_alt) = (names_in(kind), names_in(alt));
    let mut to_remove: Vec<(&str, String)> = vec![];
    for package in &packages {
        if let Some(n) = in_kind.iter().find(|n| n.to_lowercase() == *package) {
            to_remove.push((kind, n.clone()));
        } else if let Some(n) = in_alt.iter().find(|n| n.to_lowercase() == *package) {
            io.warning(&format!("{n} could not be found in {kind} but it is present in {alt}"));
        } else {
            let re = package_name_regex(package);
            let matches: Vec<&String> = in_kind.iter().filter(|n| re.is_match(n)).collect();
            if !matches.is_empty() {
                to_remove.extend(matches.into_iter().map(|n| (kind, n.clone())));
            } else if in_alt.iter().any(|n| re.is_match(n)) {
                for n in in_alt.iter().filter(|n| re.is_match(n)) {
                    io.warning(&format!("{n} could not be found in {kind} but it is present in {alt}"));
                }
            } else {
                io.warning(&format!("{package} is not required in your composer.json and has not been removed"));
            }
        }
    }
    if !args.flags.dry_run {
        for (key, name) in &to_remove {
            remove_link(&file, key, name)?;
        }
    }
    io.write_error(&format!("<info>{} has been updated</info>", file.display()));
    if args.no_update {
        return Ok(0);
    }

    let mut c = Composer::load(io.clone())?;
    if args.flags.dry_run {
        for (key, name) in &to_remove {
            let kind = if *key == "require" { komposer_package::LinkType::Require } else { komposer_package::LinkType::DevRequire };
            c.root.package.links_mut(kind).retain(|l| l.target != name.to_lowercase());
        }
    }
    if !args.flags.dry_run
        && let Some(Value::Object(allow)) = c.config.raw("allow-plugins")
    {
        let removed: Vec<&String> = allow.keys().filter(|k| packages.contains(k)).collect();
        if !removed.is_empty() {
            if removed.len() == allow.len() {
                remove_config_setting(&file, "allow-plugins")?;
            } else {
                for plugin in removed {
                    remove_config_setting(&file, &format!("allow-plugins.{plugin}"))?;
                }
            }
        }
    }

    // remove updates the dependencies of removed packages by default
    let (transitive_mode, flags) = if args.update_with_all_dependencies || env_flag("COMPOSER_WITH_ALL_DEPENDENCIES") {
        (UpdateAllowTransitive::WithDeps, " --with-all-dependencies")
    } else if args.no_update_with_dependencies {
        (UpdateAllowTransitive::OnlyListed, " --with-dependencies")
    } else {
        (UpdateAllowTransitive::WithDepsNoRootRequire, "")
    };
    io.write_error(&format!("<info>Running composer update {}{flags}</info>", packages.join(" ")));

    let locked = c.locker.is_locked()?;
    let minimal = args.minimal_changes || env_flag("COMPOSER_MINIMAL_CHANGES") || c.config.get_bool("update-with-minimal-changes");
    let mut inst = Installer::new(&mut c);
    configure(&mut inst, &args.flags, false)?;
    inst.run_scripts = run_scripts;
    inst.dev_mode = !args.update_no_dev;
    inst.update = true;
    inst.install = !args.no_install;
    inst.update_allow_transitive = transitive_mode;
    inst.audit = !(args.no_audit || env_flag("COMPOSER_NO_AUDIT"));
    inst.minimal_update = minimal;
    if locked {
        inst.update_allow_list = Some(packages.clone());
    }
    let status = inst.run()?;
    if status != 0 {
        io.write_error(&format!("\n<error>Removal failed, reverting {} to its original content.</error>", file.display()));
        std::fs::write(&file, &backup)?;
    }
    if !args.flags.dry_run {
        let installed = c.installed()?;
        for package in &packages {
            if !installed.find_packages(package, None)?.is_empty() {
                io.error(&format!(
                    "Removal failed, {package} is still present, it may be required by another package. See `composer why {package}`."
                ));
                return Ok(2);
            }
        }
    }
    Ok(status)
}

/// `JsonConfigSource::removeLink()`.
fn remove_link(file: &Path, kind: &str, name: &str) -> Result<()> {
    edit_json(
        file,
        |m| m.remove_sub_node(kind, name),
        |data| {
            if let Some(Value::Object(r)) = data.get_mut(kind) {
                r.shift_remove(name);
            }
        },
    )?;
    edit_json(
        file,
        |m| m.remove_main_key_if_empty(kind),
        |data| {
            if data.get(kind).and_then(Value::as_object).is_some_and(Map::is_empty) {
                data.shift_remove(kind);
            }
        },
    )
}

/// `JsonConfigSource::removeConfigSetting()`.
fn remove_config_setting(file: &Path, name: &str) -> Result<()> {
    edit_json(
        file,
        |m| m.remove_config_setting(name),
        |data| {
            if let Some(Value::Object(config)) = data.get_mut("config") {
                match name.split_once('.') {
                    Some((key, sub)) => {
                        if let Some(Value::Object(inner)) = config.get_mut(key) {
                            inner.shift_remove(sub);
                        }
                    }
                    None => {
                        config.shift_remove(name);
                    }
                }
            }
        },
    )
}
