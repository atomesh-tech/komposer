//! `show` and `outdated` (`ShowCommand`, `OutdatedCommand`).

use super::info::{Composite, char_width, filter_required_packages, license_ids, php_map, root_repo, str_width, tree_color, tree_line};
use crate::cli::{Cli, Command, OutdatedArgs, ShowArgs};
use crate::commands::install::{package_name_regex, platform_filter};
use crate::composer::Composer;
use anyhow::{Result, anyhow, bail};
use clap::Parser;
use indexmap::IndexMap;
use komposer_package::io::Io;
use komposer_package::json::{self, Flags};
use komposer_package::repository::{ArrayRepository, LoadRequest, RepoKind, Repository};
use komposer_package::{Link, LinkType, Location, Package, PackageRef, RootPackage, is_platform_package};
use komposer_repository::composer::ComposerRepository;
use komposer_repository::platform::{PlatformRepository, detected_runtime};
use komposer_semver::{Constraint, DEFAULT_BRANCH_ALIAS, Stability, parse_constraints, version_compare};
use komposer_solver::{DefaultPolicy, PlatformRequirementFilter, RepositorySet, VersionSelector};
use regex::Regex;
use serde_json::{Map, Value, json};
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, LazyLock};

/// `ShowCommand::execute()`.
pub fn show(io: &Arc<Io>, args: &ShowArgs, _run_scripts: bool) -> Result<i32> {
    run(io, Composer::load(io.clone()), args)
}

/// `OutdatedCommand::execute()`: a proxy for `show --latest --outdated`.
pub fn outdated(io: &Arc<Io>, args: &OutdatedArgs, run_scripts: bool) -> Result<i32> {
    let flags = [
        (!args.all, "--outdated"),
        (args.direct, "--direct"),
        (args.strict, "--strict"),
        (args.major_only, "--major-only"),
        (args.minor_only, "--minor-only"),
        (args.patch_only, "--patch-only"),
        (args.locked, "--locked"),
        (args.no_dev, "--no-dev"),
        (args.sort_by_age, "--sort-by-age"),
        (args.ignore_platform_reqs, "--ignore-platform-reqs"),
    ];
    let mut argv: Vec<String> = ["komposer", "show", "--latest"].map(String::from).into();
    argv.extend(flags.iter().filter(|(on, _)| *on).map(|(_, flag)| flag.to_string()));
    argv.extend(args.ignore_platform_req.iter().map(|r| format!("--ignore-platform-req={r}")));
    argv.extend(args.ignore.iter().map(|i| format!("--ignore={i}")));
    argv.push(format!("--format={}", args.format));
    argv.extend(args.package.iter().flat_map(|p| ["--".to_string(), p.clone()]));
    match Cli::try_parse_from(argv)?.command {
        Some(Command::Show(show_args)) => show(io, &show_args, run_scripts),
        _ => unreachable!("a show command line parses as show"),
    }
}

/// The lists `show` prints, in output order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Section {
    Platform,
    Locked,
    Available,
    Installed,
}

impl Section {
    fn name(self) -> &'static str {
        match self {
            Section::Platform => "platform",
            Section::Locked => "locked",
            Section::Available => "available",
            Section::Installed => "installed",
        }
    }
}

/// A listed package, or just a name for repositories that only list names.
enum Entry {
    Name(String),
    Package(PackageRef),
}

/// A configured (or default) repository and, for composer repositories, the one listing its names.
pub(crate) struct Remote {
    name: String,
    pub(crate) repo: Arc<dyn Repository>,
    lister: Option<ComposerRepository>,
}

/// `RepositoryManager::getRepositories()`, or `RepositoryFactory::defaultReposWithDefaultManager()`
/// without a project.
pub(crate) fn remote_repos(io: &Arc<Io>, composer: Option<&Composer>) -> Result<Vec<Remote>> {
    let (config, http) = match composer {
        Some(c) => (c.config.clone(), c.http.clone()),
        None => {
            let config = komposer_package::config::Config::create(&std::env::current_dir()?)?;
            let http = komposer_repository::http::HttpClient::new(io.clone(), &config)?;
            (config, http)
        }
    };
    let configs = config.repositories();
    let repos = komposer_repository::factory::create_repositories(&http, &configs)?;
    let mut out = vec![];
    for ((i, cfg), repo) in configs.iter().enumerate().zip(repos) {
        let lister = match cfg.get("type").and_then(Value::as_str) {
            Some("composer") => {
                let mut inner = cfg.clone();
                if let Some(m) = inner.as_object_mut() {
                    for key in ["only", "exclude", "canonical"] {
                        m.shift_remove(key);
                    }
                }
                Some(ComposerRepository::new(&inner, http.clone())?)
            }
            _ => None,
        };
        let name = match cfg.get("url").and_then(Value::as_str) {
            Some("https://repo.packagist.org") => "packagist.org".to_string(),
            _ => komposer_repository::factory::generate_repository_name(&i.to_string(), true, cfg, &[]),
        };
        out.push(Remote { name, repo, lister });
    }
    Ok(out)
}

fn run(io: &Arc<Io>, loaded: Result<Composer>, args: &ShowArgs) -> Result<i32> {
    let composer = loaded.as_ref().ok();
    let require = || loaded.as_ref().map_err(|e| anyhow!("{e:#}"));
    let json = args.format == "json";
    if args.installed && !args.self_ {
        io.write_error("<warning>You are using the deprecated option \"installed\". Only installed packages are shown by default now. The --all option can be used to show all packages.</warning>");
    }
    let mut latest = args.latest || args.outdated;
    if !args.outdated && !args.ignore.is_empty() {
        io.write_error("<warning>You are using the option \"ignore\" for action other than \"outdated\", it will be ignored.</warning>");
    }
    let invalid = [
        (
            args.direct && (args.all || args.available || args.platform),
            "The --direct (-D) option is not usable in combination with --all, --platform (-p) or --available (-a)",
        ),
        (args.tree && (args.all || args.available), "The --tree (-t) option is not usable in combination with --all or --available (-a)"),
        (
            [args.patch_only, args.minor_only, args.major_only].iter().filter(|o| **o).count() > 1,
            "Only one of --major-only, --minor-only or --patch-only can be used at once",
        ),
        (args.tree && latest, "The --tree (-t) option is not usable in combination with --latest (-l)"),
        (args.tree && args.path, "The --tree (-t) option is not usable in combination with --path (-P)"),
    ];
    if let Some((_, message)) = invalid.iter().find(|(bad, _)| *bad) {
        io.write_error(message);
        return Ok(1);
    }
    if !matches!(args.format.as_str(), "text" | "json") {
        io.write_error(&format!("Unsupported format \"{}\". See help for supported formats.", args.format));
        return Ok(1);
    }
    let filter = platform_filter(args.ignore_platform_reqs, &args.ignore_platform_req);
    let platform = Arc::new(match composer {
        Some(c) => PlatformRepository::from_config(&c.config)?,
        None => PlatformRepository::new(&json!([]), detected_runtime())?,
    });
    let platform_repo: Arc<dyn Repository> = platform.clone();
    let no_project = |remotes: &[Remote]| {
        let names: Vec<&str> = remotes.iter().map(|r| r.name.as_str()).collect();
        io.write_error(&format!("No composer.json found in the current directory, showing available packages from {}", names.join(", ")));
    };

    // the InstalledRepository, and the repositories listed (`$repos`, flattened) with their section
    let mut installed: Vec<Arc<dyn Repository>> = vec![];
    let mut listed: Vec<(Section, Arc<dyn Repository>, Option<ComposerRepository>)> = vec![];
    let mut single = None;
    if args.self_ && !args.installed && !args.locked {
        let c = require()?;
        if args.name_only {
            io.write(&c.root.package.name);
            return Ok(0);
        }
        if args.package.is_some() {
            bail!("You cannot use --self together with a package name");
        }
        let (repo, root) = root_repo(&c.root);
        single = Some(root);
        installed.push(repo.clone());
        listed.push((Section::Installed, repo, None));
    } else if args.platform {
        installed.push(platform_repo.clone());
        listed.push((Section::Platform, platform_repo.clone(), None));
    } else if args.available || args.all {
        let remotes = remote_repos(io, composer)?;
        match composer {
            Some(c) if args.available => installed.extend([platform_repo.clone(), Arc::new(c.installed()?)]),
            Some(c) => {
                let local: Arc<dyn Repository> = Arc::new(c.installed()?);
                if c.locker.is_locked()? {
                    let locked: Arc<dyn Repository> = Arc::new(c.locker.locked_repository(true)?);
                    installed.push(locked.clone());
                    listed.push((Section::Locked, locked, None));
                }
                installed.extend([local.clone(), platform_repo.clone()]);
                listed.extend([(Section::Installed, local, None), (Section::Platform, platform_repo.clone(), None)]);
            }
            None => {
                no_project(&remotes);
                installed.push(platform_repo.clone());
                if args.all {
                    listed.push((Section::Platform, platform_repo.clone(), None));
                }
            }
        }
        listed.extend(remotes.into_iter().map(|r| (Section::Available, r.repo, r.lister)));
    } else if args.locked {
        let c = composer
            .filter(|c| c.locker.is_locked().unwrap_or(false))
            .ok_or_else(|| anyhow!("A valid composer.json and composer.lock files is required to run this command with --locked"))?;
        let mut locked = c.locker.locked_repository(!args.no_dev)?;
        if args.self_ {
            locked.add_package(c.root.package.clone());
        }
        let locked: Arc<dyn Repository> = Arc::new(locked);
        installed.push(locked.clone());
        listed.push((Section::Locked, locked, None));
    } else {
        let c = require()?;
        let root: Arc<dyn Repository> = if args.self_ { root_repo(&c.root).0 } else { Arc::new(ArrayRepository::new("", RepoKind::Installed)) };
        let local = c.installed()?;
        let local: Arc<dyn Repository> = if args.no_dev {
            let mut repo = ArrayRepository::new("", RepoKind::Installed);
            filter_required_packages(&local.packages()?, &c.root.package).into_iter().for_each(|p| repo.add(p));
            Arc::new(repo)
        } else {
            Arc::new(local)
        };
        installed.extend([root.clone(), local.clone()]);
        listed.extend([(Section::Installed, root, None), (Section::Installed, local, None)]);
        let has_non_platform = |links: &[Link]| links.iter().any(|l| !is_platform_package(&l.target));
        if packages_of(&installed)?.is_empty() && (has_non_platform(&c.root.package.requires) || has_non_platform(&c.root.package.dev_requires)) {
            io.write_error("<warning>No dependencies installed. Try running composer install or update.</warning>");
        }
    }

    if latest && composer.is_none() {
        io.write_error("No composer.json found in the current directory, disabling \"latest\" option");
        latest = false;
    }
    let lookup = Lookup { installed: packages_of(&installed)?, repos: Arc::new(Composite(listed.iter().map(|(_, r, _)| r.clone()).collect())) };
    let latest_repos = match composer.filter(|_| latest) {
        Some(c) => {
            let remote: Arc<dyn Repository> = Arc::new(Composite(c.repositories()?));
            let mut set = RepositorySet::new(c.root.minimum_stability).with_stability_flags(c.root.stability_flags.iter().cloned().collect());
            set.add_repository(remote.clone());
            Some((c, remote, set))
        }
        None => None,
    };
    // without PHP, only what config.platform declares can be checked
    let filter = match &latest_repos {
        Some((c, ..)) => crate::installer::effective_platform_filter(&c.config, &filter, &args.ignore_platform_req, io),
        None => filter,
    };
    let finder = match &latest_repos {
        Some((c, remote, set)) => Some(Latest {
            selector: VersionSelector::new(set, Some(platform.as_ref() as &dyn Repository))?,
            remote: remote.clone(),
            root: &c.root,
            filter: &filter,
            io: io.as_ref(),
            only: (args.major_only, args.minor_only, args.patch_only),
        }),
        None => None,
    };

    // a single package or version
    let mut versions = IndexMap::new();
    let package = match (single, args.package.as_deref()) {
        (Some(p), _) => {
            versions.insert(p.pretty_version.clone(), p.version.clone());
            Some(p)
        }
        (None, Some(name)) if !name.contains('*') => {
            let constraint = args.version.as_deref().map(parse_constraints).transpose()?;
            let (found, all) = lookup.get_package(name, constraint.as_ref())?;
            versions = all;
            let Some(p) = found else {
                let mut hint = String::new();
                if args.locked {
                    hint += " in lock file";
                }
                if is_platform_package(name) && !args.platform {
                    hint += ", try using --platform (-p) to show platform packages";
                }
                if !args.all && !args.available {
                    hint += ", try using --available (-a) to show all available packages";
                }
                bail!("Package \"{name}\" not found{hint}.");
            };
            if args.direct && !root_requires(composer).contains(&p.name) {
                bail!("Package \"{}\" is installed but not a direct dependent of the root package.", p.name);
            }
            Some(p)
        }
        _ => None,
    };
    if let Some(p) = package {
        if args.tree {
            print_trees(io, json, &[lookup.package_tree(&p)?]);
            return Ok(0);
        }
        let latest_package = finder.as_ref().map(|f| f.find(&p)).transpose()?.flatten();
        let outdated = latest_package.as_ref().is_some_and(|l| l.full_pretty_version() != p.full_pretty_version() && !l.is_abandoned());
        let code = i32::from(args.outdated && args.strict && outdated);
        if args.path {
            io.write(&format!("{} {}", p.name, install_path(Some(require()?), &p).as_deref().unwrap_or("null")));
        } else if json {
            print_info_json(io, composer, &p, &versions, &lookup, latest_package.as_ref());
        } else {
            print_info(io, composer, &p, &versions, &lookup, latest_package.as_ref());
        }
        return Ok(code);
    }

    if args.tree {
        let requires = root_requires(composer);
        let mut packages = lookup.installed.clone();
        packages.sort_by_cached_key(|p| p.to_string());
        let trees = packages.iter().filter(|p| requires.contains(&p.name)).map(|p| lookup.package_tree(p)).collect::<Result<Vec<_>>>()?;
        print_trees(io, json, &trees);
        return Ok(0);
    }

    // list packages
    let name_filter = args.package.as_deref().map(package_name_regex);
    let direct_filter = args.direct.then(|| root_requires(composer));
    let mut path = args.path;
    if path && composer.is_none() {
        io.write_error("No composer.json found in the current directory, disabling \"path\" option");
        path = false;
    }
    let mut sections: BTreeMap<Section, BTreeMap<String, Entry>> = BTreeMap::new();
    for (section, repo, lister) in &listed {
        let list = sections.entry(*section).or_default();
        if let Some(lister) = lister {
            for name in lister.package_names(args.package.as_deref())? {
                list.insert(name.clone(), Entry::Name(name));
            }
            continue;
        }
        for p in repo.packages()? {
            let newer = match list.get(&p.name) {
                Some(Entry::Package(seen)) => version_compare(&seen.version, &p.version) == Ordering::Less,
                _ => true,
            };
            let p = unalias(&p);
            if newer && name_filter.as_ref().is_none_or(|r| r.is_match(&p.name)) && direct_filter.as_ref().is_none_or(|d| d.contains(&p.name)) {
                list.insert(p.name.clone(), Entry::Package(p));
            }
        }
        if *section == Section::Platform {
            for p in platform.disabled_packages() {
                list.insert(p.name.clone(), Entry::Package(p.clone()));
            }
        }
    }
    sections.retain(|_, list| !list.is_empty());

    let ignored = names_regex(&args.ignore);
    let root_reqs = root_requires(composer);
    let width = terminal_width();
    let mut code = 0;
    // PHP leaks these flags from the last section built into the output of every section
    let (mut write_version, mut write_description) = (false, false);
    let mut view: Vec<(Section, Vec<Row>, Layout)> = vec![];
    for (section, entries) in &sections {
        let show_version = *section != Section::Available;
        let mut latest_packages: HashMap<&str, PackageRef> = HashMap::new();
        if let Some(finder) = finder.as_ref().filter(|_| show_version) {
            let packages: Vec<&PackageRef> = entries.values().filter_map(Entry::package).filter(|p| !ignored.is_match(&p.pretty_name)).collect();
            finder.prefetch(&packages);
            for p in packages {
                if let Some(l) = finder.find(p)? {
                    latest_packages.insert(&p.pretty_name, l);
                }
            }
        }
        let write_path = !args.name_only && path;
        write_version = !args.name_only && !path && show_version;
        let write_latest = write_version && latest;
        write_description = !args.name_only && !path;
        let write_release_date = write_latest && (args.sort_by_age || json);
        let mut entries: Vec<&Entry> = entries.values().collect();
        if args.sort_by_age {
            entries.sort_by(|a, b| match (a, b) {
                (Entry::Package(a), Entry::Package(b)) => timestamp(a.time.as_deref()).cmp(&timestamp(b.time.as_deref())),
                _ => Ordering::Equal,
            });
        }

        let mut layout = Layout { write_latest, write_release_date, ..Layout::default() };
        let mut rows = vec![];
        let mut has_outdated = false;
        for entry in entries {
            let p = match entry {
                Entry::Name(name) => {
                    layout.name = layout.name.max(name.len());
                    rows.push(Map::from_iter([("name".to_string(), json!(name))]));
                    continue;
                }
                Entry::Package(p) => p,
            };
            let latest_package = latest_packages.get(p.pretty_name.as_str());
            let up_to_date = match latest_package {
                Some(l) => l.full_pretty_version() == p.full_pretty_version() && !l.is_abandoned(),
                // with --major-only, nothing above the current major means up to date
                None => args.major_only,
            };
            if args.outdated && (up_to_date || ignored.is_match(&p.pretty_name)) {
                continue;
            }
            has_outdated |= args.outdated || args.strict;

            let mut row = Map::new();
            row.insert("name".into(), json!(p.pretty_name));
            row.insert("direct-dependency".into(), json!(root_reqs.contains(&p.name)));
            if !json || !args.name_only {
                row.insert("homepage".into(), json!(meta_str(p, "homepage")));
                row.insert("source".into(), json!(view_source_url(p)));
            }
            layout.name = layout.name.max(p.pretty_name.len());
            let shown = |version: String| if json { version } else { version.trim_start_matches('v').to_string() };
            if write_version {
                let version = shown(p.full_pretty_version());
                layout.version = layout.version.max(version.len());
                row.insert("version".into(), json!(version));
            }
            if write_release_date {
                let (age, date) = match &p.time {
                    Some(t) => {
                        let age = relative_time(t, now()).replace(" ago", " old");
                        (if age.contains(" old") { age } else { format!("from {age}") }, t.clone())
                    }
                    None => (String::new(), String::new()),
                };
                layout.release = layout.release.max(age.len());
                row.insert("release-age".into(), json!(age));
                row.insert("release-date".into(), json!(date));
            }
            if write_latest {
                let (version, status) = match latest_package {
                    Some(l) => (shown(l.full_pretty_version()), update_status(l, p)),
                    None => ("[none matched]".to_string(), "up-to-date"),
                };
                layout.latest = layout.latest.max(version.len());
                row.insert("latest".into(), json!(version));
                row.insert("latest-status".into(), json!(status));
                if let Some(l) = latest_package {
                    row.insert("latest-release-date".into(), json!(l.time.as_deref().unwrap_or("")));
                }
            }
            if write_description {
                row.insert("description".into(), json!(meta_str(p, "description")));
            }
            if write_path {
                row.insert("path".into(), json!(install_path(composer, p)));
            }
            let mut abandoned = json!(false);
            if let Some(l) = latest_package.filter(|l| l.is_abandoned()) {
                let replacement = l.replacement_package().map_or("No replacement was suggested".into(), |r| format!("Use {r} instead"));
                row.insert("warning".into(), json!(format!("Package {} is abandoned, you should avoid using it. {replacement}.", p.pretty_name)));
                abandoned = l.replacement_package().map_or(json!(true), |r| json!(r));
            }
            row.insert("abandoned".into(), abandoned);
            rows.push(row);
        }
        view.push((*section, rows, layout));
        if args.strict && has_outdated {
            code = 1;
            break;
        }
    }

    if json {
        let data: Map<String, Value> =
            view.into_iter().map(|(s, rows, _)| (s.name().to_string(), Value::Array(rows.into_iter().map(Value::Object).collect()))).collect();
        io.write(&json::encode(&php_map(data), Flags::FILE));
        return Ok(code);
    }
    if latest && view.iter().any(|(_, rows, _)| !rows.is_empty()) {
        if io.is_decorated() {
            io.write_error("<info>Color legend:</info>");
            io.write_error("- <highlight>patch or minor</highlight> release available - update recommended");
            io.write_error("- <comment>major</comment> release available - update possible");
            if !args.outdated {
                io.write_error("- <info>up to date</info> version");
            }
        } else {
            io.write_error("Legend:");
            io.write_error("! patch or minor release available - update recommended");
            io.write_error("~ major release available - update possible");
            if !args.outdated {
                io.write_error("= up to date version");
            }
        }
    }
    for (section, rows, layout) in &view {
        let mut layout = *layout;
        let fits = |extra: usize| layout.name + layout.version + extra <= width;
        let version_fits = fits(3);
        let latest_fits = fits(layout.latest + 3);
        let release_fits = fits(layout.latest + layout.release + 3);
        let description_fits = fits(layout.latest + layout.release + 24);
        if latest_fits && !io.is_decorated() {
            layout.latest += 2;
        }
        let columns = |latest: bool| Columns {
            indent: if args.all { "  " } else { "" },
            version: write_version && version_fits,
            latest,
            description: write_description && description_fits,
            release_date: layout.write_release_date && release_fits,
            layout,
            width,
        };
        if args.all {
            match section {
                Section::Available => io.write("<comment>available</comment>:"),
                _ => io.write(&format!("<info>{}</info>:", section.name())),
            }
        }
        let rows: Vec<&Row> = rows.iter().collect();
        if layout.write_latest && !args.direct {
            let (direct, transitive): (Vec<_>, Vec<_>) = rows.iter().partition(|r| r.get("direct-dependency") == Some(&Value::Bool(true)));
            for (title, list) in
                [("Direct dependencies required in composer.json:", direct), ("Transitive dependencies not required in composer.json:", transitive)]
            {
                io.write_error("");
                io.write_error(&format!("<info>{title}</>"));
                if list.is_empty() {
                    io.write_error("Everything up to date");
                } else {
                    print_packages(io, &list, &columns(latest_fits));
                }
            }
        } else if layout.write_latest && rows.is_empty() {
            io.write_error("All your direct dependencies are up to date");
        } else {
            print_packages(io, &rows, &columns(layout.write_latest && latest_fits));
        }
        if args.all {
            io.write("");
        }
    }
    Ok(code)
}

impl Entry {
    fn package(&self) -> Option<&PackageRef> {
        match self {
            Entry::Package(p) => Some(p),
            Entry::Name(_) => None,
        }
    }
}

/// A package of a list, as `--format=json` prints it.
type Row = Map<String, Value>;

/// Column widths of one section.
#[derive(Clone, Copy, Default)]
struct Layout {
    name: usize,
    version: usize,
    latest: usize,
    release: usize,
    write_latest: bool,
    write_release_date: bool,
}

/// What `printPackages()` writes.
struct Columns {
    indent: &'static str,
    version: bool,
    latest: bool,
    description: bool,
    release_date: bool,
    layout: Layout,
    width: usize,
}

/// `printPackages()`.
fn print_packages(io: &Io, rows: &[&Row], c: &Columns) {
    let pad = |text: &str, pad: bool, width: usize| if pad { format!("{text:<width$}") } else { text.to_string() };
    let (pad_name, pad_version) = (c.version || c.latest || c.release_date || c.description, c.latest || c.release_date || c.description);
    let (pad_latest, pad_release) = (c.description || c.release_date, c.description);
    let l = &c.layout;
    for row in rows {
        let get = |key: &str| row.get(key).and_then(Value::as_str);
        let mut line = format!("{}{}", c.indent, pad(get("name").unwrap_or(""), pad_name, l.name));
        if let Some(version) = get("version").filter(|_| c.version) {
            line += &format!(" {}", pad(version, pad_version, l.version));
        }
        if let (Some(latest), Some(status), true) = (get("latest"), get("latest-status"), c.latest) {
            let style = status_style(status);
            let text = if io.is_decorated() { latest.to_string() } else { format!("{} {latest}", status_marker(status)) };
            line += &format!(" <{style}>{}</{style}>", pad(&text, pad_latest, l.latest));
            if let Some(age) = get("release-age").filter(|_| c.release_date) {
                line += &format!(" {}", pad(age, pad_release, l.release));
            }
        }
        if let Some(description) = get("description").filter(|_| c.description) {
            let mut remaining = c.width as i64 - (l.name + l.version + l.release) as i64 - 4;
            if c.latest {
                remaining -= l.latest as i64;
            }
            let description = first_line(description);
            let description = if remaining <= 0 { String::new() } else { strim_width(description, remaining as usize) };
            line += &format!(" {description}");
        }
        if let Some(path) = row.get("path") {
            line += &format!(" {}", path.as_str().unwrap_or("null"));
        }
        io.write(&line);
        if let Some(warning) = get("warning") {
            io.write(&format!("<warning>{warning}</warning>"));
        }
    }
}

/// `strtok($text, "\r\n")`: the first non-empty line.
fn first_line(text: &str) -> &str {
    text.split(['\r', '\n']).find(|l| !l.is_empty()).unwrap_or("")
}

/// `mb_strimwidth($text, 0, $width, '...')`.
fn strim_width(text: &str, width: usize) -> String {
    if str_width(text) <= width {
        return text.to_string();
    }
    let mut used = 0;
    let kept: String = text
        .chars()
        .take_while(|c| {
            used += char_width(*c);
            used + 3 <= width
        })
        .collect();
    kept + "..."
}

/// `BaseCommand::getTerminalWidth()`: `COLUMNS`, else what `stty` reports, at least 80.
fn terminal_width() -> usize {
    let stty = || {
        let out =
            std::process::Command::new("stty").arg("size").stdin(std::process::Stdio::inherit()).stderr(std::process::Stdio::null()).output().ok()?;
        String::from_utf8_lossy(&out.stdout).split_whitespace().nth(1)?.parse::<i64>().ok()
    };
    let width = match std::env::var("COLUMNS") {
        Ok(c) => c.trim().parse::<i64>().unwrap_or(0),
        Err(_) => stty().filter(|w| *w > 0).unwrap_or(80),
    };
    width.max(80) as usize
}

/// `getUpdateStatus()`.
fn update_status(latest: &Package, package: &Package) -> &'static str {
    if latest.full_pretty_version() == package.full_pretty_version() {
        return "up-to-date";
    }
    let constraint = if package.version.starts_with("dev-") { package.version.clone() } else { format!("^{}", package.version) };
    if !latest.version.is_empty() && komposer_semver::satisfies(&latest.version, &constraint) {
        // it needs an immediate semver-compliant upgrade
        "semver-safe-update"
    } else {
        // it needs an upgrade but has potential BC breaks so is not urgent
        "update-possible"
    }
}

/// `updateStatusToVersionStyle()`: green, red, yellow.
fn status_style(status: &str) -> &'static str {
    match status {
        "up-to-date" => "info",
        "semver-safe-update" => "highlight",
        _ => "comment",
    }
}

/// The marker shown instead of colors when the output is not decorated.
fn status_marker(status: &str) -> &'static str {
    match status {
        "up-to-date" => "=",
        "semver-safe-update" => "!",
        _ => "~",
    }
}

/// `findLatestPackage()`'s target constraint for the installed (normalized) version; `None` when
/// there is nothing to look for (dev branches with `--major-only`).
fn latest_target(version: &str, (major, minor, patch): (bool, bool, bool)) -> Option<Option<String>> {
    if version.starts_with("dev-") {
        // dev branches are always on the latest major; a new commit counts as a minor update
        return (!major).then(|| Some(version.to_string()));
    }
    static MAJOR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(?P<zero>(?:0\.)+)?(?P<first>\d+)\.").unwrap());
    static TRAILING_ZEROS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?:\.0)+$").unwrap());
    let mut target = None;
    if major && let Some(m) = MAJOR.captures(version) {
        let next = m["first"].parse::<u64>().unwrap_or(0).saturating_add(1);
        target = Some(format!(">={}{next},<9999999-dev", m.name("zero").map_or("", |z| z.as_str())));
    }
    if minor {
        target = Some(format!("^{version}"));
    }
    if patch {
        let mut trimmed = TRAILING_ZEROS.replace(version, "").into_owned();
        let parts = if trimmed.starts_with('0') { 4 } else { 3 };
        while trimmed.matches('.').count() + 1 < parts {
            trimmed += ".0";
        }
        target = Some(format!("~{trimmed}"));
    }
    Some(target)
}

/// `findLatestPackage()` and the repository set it searches.
struct Latest<'a> {
    selector: VersionSelector<'a>,
    remote: Arc<dyn Repository>,
    root: &'a RootPackage,
    filter: &'a PlatformRequirementFilter,
    io: &'a Io,
    /// --major-only, --minor-only, --patch-only
    only: (bool, bool, bool),
}

impl Latest<'_> {
    fn find(&self, p: &Package) -> Result<Option<PackageRef>> {
        let root = self.root;
        let stability = root.stability_flags.iter().find(|(n, _)| *n == p.name).map_or(root.minimum_stability, |(_, s)| *s);
        let best_stability = if root.prefer_stable { p.stability } else { stability };
        let Some(target) = latest_target(&p.version, self.only) else { return Ok(None) };
        let installed = &p.version;
        let show_warnings = |c: &Package| {
            self.io.is_verbose()
                || (!c.version.starts_with("dev-") && !installed.starts_with("dev-") && version_compare(&c.version, installed) != Ordering::Greater)
        };
        let best = self.selector.find_best_candidate(&p.name, target.as_deref(), best_stability, self.filter, 0, Some(self.io), &show_warnings)?;
        Ok(best.map(|b| unalias(&b)))
    }

    /// Loads the metadata of all packages concurrently, so the lookups one by one hit the cache.
    fn prefetch(&self, packages: &[&PackageRef]) {
        let names: Vec<(String, Option<Constraint>)> = packages.iter().filter(|p| !p.is_platform()).map(|p| (p.name.clone(), None)).collect();
        let all = [Stability::Stable, Stability::RC, Stability::Beta, Stability::Alpha, Stability::Dev];
        let acceptable: HashSet<Stability> = all.into_iter().filter(|s| *s <= self.root.minimum_stability).collect();
        let flags = self.root.stability_flags.iter().cloned().collect();
        let request = LoadRequest { names: &names, acceptable_stabilities: &acceptable, stability_flags: &flags, already_loaded: &HashMap::new() };
        let _ = self.remote.load_packages(&request);
    }
}

/// The repositories `getPackage()` searches, and the packages of the `InstalledRepository`.
struct Lookup {
    installed: Vec<PackageRef>,
    repos: Arc<dyn Repository>,
}

impl Lookup {
    /// `InstalledRepository::hasPackage()`.
    fn has_package(&self, p: &Package) -> bool {
        let id = p.unique_name();
        self.installed.iter().any(|i| i.unique_name() == id)
    }

    /// `getPackage()`: the installed match (without a constraint) or the policy's pick, and all
    /// matching versions (pretty => normalized).
    fn get_package(&self, name: &str, constraint: Option<&Constraint>) -> Result<(Option<PackageRef>, IndexMap<String, String>)> {
        let name = name.to_lowercase();
        let mut set = RepositorySet::new(Stability::Dev);
        set.allow_installed_repositories(true);
        set.add_repository(self.repos.clone());
        let pool = if is_platform_package(&name) { set.create_pool_with_all_packages()? } else { set.create_pool_for_package(&name, None)? };
        let (mut matched, mut versions, mut literals) = (None, IndexMap::new(), vec![]);
        for &literal in pool.what_provides(&name, constraint).iter() {
            let mut p = pool.package(literal).clone();
            // avoid showing the 9999999-dev alias if the default branch has no branch-alias set
            if p.version == DEFAULT_BRANCH_ALIAS
                && let Some(of) = &p.alias_of
            {
                p = of.clone();
            }
            if constraint.is_none() && self.has_package(&p) {
                matched = Some(p.clone());
            }
            versions.insert(p.pretty_version.clone(), p.version.clone());
            literals.extend(pool.id_of(&p));
        }
        if matched.is_none() && !literals.is_empty() {
            let preferred = DefaultPolicy::new(false, false, None).select_preferred_packages(&pool, &literals, None);
            matched = preferred.first().map(|&l| pool.package(l).clone());
        }
        Ok((matched, versions))
    }

    /// `generatePackageTree()`.
    fn package_tree(&self, p: &Package) -> Result<Value> {
        let mut children = vec![];
        for link in sorted_requires(p) {
            let mut child = json!({"name": link.target, "version": link.pretty_constraint});
            let deep = self.add_tree(link, vec![p.name.clone(), link.target.clone()])?;
            if !deep.is_empty() {
                child["requires"] = Value::Array(deep);
            }
            children.push(child);
        }
        let mut tree = json!({"name": p.pretty_name, "version": p.pretty_version, "description": meta_str(p, "description")});
        if !children.is_empty() {
            tree["requires"] = Value::Array(children);
        }
        Ok(tree)
    }

    /// `addTree()`: the requirements of the package `link` resolves to.
    fn add_tree(&self, link: &Link, in_tree: Vec<String>) -> Result<Vec<Value>> {
        let constraint = if link.pretty_constraint == "self.version" { link.constraint.clone() } else { parse_constraints(&link.pretty_constraint)? };
        let Some(p) = self.get_package(&link.target, Some(&constraint))?.0 else { return Ok(vec![]) };
        let mut children = vec![];
        for req in sorted_requires(&p) {
            let mut child = json!({"name": req.target, "version": req.pretty_constraint});
            if !in_tree.contains(&req.target) {
                let mut current = in_tree.clone();
                current.push(req.target.clone());
                let deep = self.add_tree(req, current)?;
                if !deep.is_empty() {
                    child["requires"] = Value::Array(deep);
                }
            }
            children.push(child);
        }
        Ok(children)
    }
}

fn sorted_requires(p: &Package) -> Vec<&Link> {
    let mut requires: Vec<&Link> = p.requires.iter().collect();
    requires.sort_by(|a, b| a.target.cmp(&b.target));
    requires
}

fn print_trees(io: &Io, json: bool, trees: &[Value]) {
    if json {
        io.write(&json::encode(&json!({"installed": trees}), Flags::FILE));
    } else {
        display_tree(io, trees);
    }
}

/// `displayPackageTree()`.
fn display_tree(io: &Io, trees: &[Value]) {
    let s = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    for tree in trees {
        let mut line = format!("<info>{}</info> {}", s(tree, "name"), s(tree, "version"));
        if let Some(description) = tree.get("description").and_then(Value::as_str) {
            line += &format!(" {}", first_line(description));
        }
        io.write(&line);
        let requires = tree.get("requires").and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
        for (j, require) in requires.iter().enumerate() {
            let bar = if j + 1 == requires.len() { "└" } else { "├" };
            io.write(&tree_line(io, &format!("{bar}──{} {}", tree_color(io, 1, &s(require, "name")), s(require, "version"))));
            display_subtree(io, require, &[s(tree, "name"), s(require, "name")], &bar.replace('└', " "), 2);
        }
    }
}

/// `displayTree()`.
fn display_subtree(io: &Io, package: &Value, in_tree: &[String], previous_bar: &str, level: usize) {
    let previous = previous_bar.replace('├', "│");
    let requires = package.get("requires").and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
    for (i, require) in requires.iter().enumerate() {
        let name = require.get("name").and_then(Value::as_str).unwrap_or("");
        let version = require.get("version").and_then(Value::as_str).unwrap_or("");
        let bar = format!("{previous}  {}", if i + 1 == requires.len() { "└" } else { "├" });
        let circular = if in_tree.iter().any(|n| n == name) { "(circular dependency aborted here)" } else { "" };
        let info = format!("{bar}──{} {version} {circular}", tree_color(io, level, name));
        io.write(&tree_line(io, info.trim_end()));
        let mut current = in_tree.to_vec();
        current.push(name.to_string());
        display_subtree(io, require, &current, &bar.replace('└', " "), level + 1);
    }
}

/// `printPackageInfo()`.
fn print_info(io: &Io, c: Option<&Composer>, p: &PackageRef, versions: &IndexMap<String, String>, lookup: &Lookup, latest: Option<&PackageRef>) {
    let installed = !p.is_platform() && lookup.has_package(p);
    io.write(&format!("<info>name</info>     : {}", p.pretty_name));
    io.write(&format!("<info>descrip.</info> : {}", meta_str(p, "description").unwrap_or("")));
    io.write(&format!("<info>keywords</info> : {}", strings(p.meta.get("keywords")).join(", ")));

    let mut list: Vec<String> = versions.keys().cloned().collect();
    komposer_semver::rsort(&mut list);
    for i in lookup.installed.iter().filter(|i| i.name == p.name) {
        if let Some(pos) = list.iter().position(|v| *v == i.pretty_version) {
            list[pos] = format!("<info>* {}</info>", i.pretty_version);
        }
    }
    io.write(&format!("<info>versions</info> : {}", list.join(", ")));
    let released = |t: &str| format!("{}, {}", t.get(..10).unwrap_or(t), relative_time(t, now()));
    if let Some(time) = p.time.as_deref().filter(|_| installed) {
        io.write(&format!("<info>released</info> : {}", released(time)));
    }
    if let Some(l) = latest {
        let style = status_style(update_status(l, p));
        let time = l.time.as_deref().map(|t| format!(" released {}", released(t))).unwrap_or_default();
        io.write(&format!("<info>latest</info>   : <{style}>{}</{style}>{time}", l.pretty_version));
    }
    io.write(&format!("<info>type</info>     : {}", p.package_type));
    for id in license_ids(p) {
        let text = match spdx_license(&id) {
            Some((canonical, name, true)) => format!("{name} ({id}) (OSI approved) {}", license_url(canonical)),
            Some((canonical, name, false)) => format!("{name} ({id}) {}", license_url(canonical)),
            None => id,
        };
        io.write(&format!("<info>license</info>  : {text}"));
    }
    io.write(&format!("<info>homepage</info> : {}", meta_str(p, "homepage").unwrap_or("")));
    let location = |l: &Option<Location>| match l {
        Some(l) => format!("[{}] <comment>{}</comment> {}", l.kind, l.url, l.reference.as_deref().unwrap_or("")),
        None => "[] <comment></comment> ".into(),
    };
    io.write(&format!("<info>source</info>   : {}", location(&p.source)));
    io.write(&format!("<info>dist</info>     : {}", location(&p.dist)));
    if installed {
        io.write(&format!("<info>path</info>     : {}", install_path(c, p).as_deref().unwrap_or("null")));
    }
    io.write(&format!("<info>names</info>    : {}", p.names(true).join(", ")));

    let current = latest.unwrap_or(p);
    if current.is_abandoned() {
        let replacement = current.replacement_package().map(|r| format!(" The author suggests using the {r} package instead.")).unwrap_or_default();
        io.write_error(&format!("<warning>Attention: This package is abandoned and no longer maintained.{replacement}</warning>"));
    }
    if let Some(support) = p.meta.get("support").and_then(Value::as_object).filter(|s| !s.is_empty()) {
        io.write("\n<info>support</info>");
        for (kind, value) in support {
            io.write(&format!("<comment>{kind}</comment> : {}", scalar(value)));
        }
    }
    if let Some(autoload) = p.autoload.as_object().filter(|a| !a.is_empty()) {
        io.write("\n<info>autoload</info>");
        for (kind, rules) in autoload {
            io.write(&format!("<comment>{kind}</comment>"));
            match kind.as_str() {
                "psr-0" | "psr-4" => {
                    for (namespace, path) in rules.as_object().into_iter().flatten() {
                        let path = match path {
                            Value::Array(paths) => paths.iter().map(scalar).collect::<Vec<_>>().join(", "),
                            v => Some(scalar(v)).filter(|s| !s.is_empty() && s != "0").unwrap_or_else(|| ".".into()),
                        };
                        io.write(&format!("{} => {path}", if namespace.is_empty() { "*" } else { namespace }));
                    }
                }
                "classmap" => io.write(&strings(Some(rules)).join(", ")),
                _ => {}
            }
        }
        let include = strings(Some(&p.include_path));
        if !include.is_empty() {
            io.write("<comment>include-path</comment>");
            io.write(&include.join(", "));
        }
    }

    let links = |kind: LinkType, title: &str| {
        let links = p.links(kind);
        if !links.is_empty() {
            io.write(&format!("\n<info>{title}</info>"));
            links.iter().for_each(|l| io.write(&format!("{} <comment>{}</comment>", l.target, l.pretty_constraint)));
        }
    };
    links(LinkType::Require, "requires");
    links(LinkType::DevRequire, "requires (dev)");
    if !p.suggests.is_empty() {
        io.write("\n<info>suggests</info>");
        for (target, reason) in &p.suggests {
            io.write(&format!("{target} <comment>{}</comment>", scalar(reason)));
        }
    }
    links(LinkType::Provide, "provides");
    links(LinkType::Conflict, "conflicts");
    links(LinkType::Replace, "replaces");
}

/// `printPackageInfoAsJson()`.
fn print_info_json(io: &Io, c: Option<&Composer>, p: &PackageRef, versions: &IndexMap<String, String>, lookup: &Lookup, latest: Option<&PackageRef>) {
    let mut out = Map::new();
    out.insert("name".into(), json!(p.pretty_name));
    out.insert("description".into(), json!(meta_str(p, "description")));
    out.insert("keywords".into(), json!(strings(p.meta.get("keywords"))));
    out.insert("type".into(), json!(p.package_type));
    out.insert("homepage".into(), json!(meta_str(p, "homepage")));
    out.insert("names".into(), json!(p.names(true)));
    let mut sorted: Vec<(&String, &String)> = versions.iter().collect();
    sorted.sort_by(|a, b| version_compare(a.1, b.1));
    out.insert("versions".into(), json!(sorted.iter().rev().map(|(pretty, _)| pretty).collect::<Vec<_>>()));
    let licenses: Vec<Value> = license_ids(p)
        .into_iter()
        .map(|id| match spdx_license(&id) {
            Some((canonical, name, _)) => json!({"name": name, "osi": id, "url": license_url(canonical)}),
            None => json!(id),
        })
        .collect();
    if !licenses.is_empty() {
        out.insert("licenses".into(), Value::Array(licenses));
    }
    if let Some(l) = latest {
        out.insert("latest".into(), json!(l.pretty_version));
    }
    for (key, location) in [("source", &p.source), ("dist", &p.dist)] {
        if let Some(l) = location {
            out.insert(key.into(), json!({"type": l.kind, "url": l.url, "reference": l.reference}));
        }
    }
    if !p.is_platform() && lookup.has_package(p) {
        match install_path(c, p) {
            None => {
                out.insert("path".into(), Value::Null);
            }
            Some(path) if !path.is_empty() => {
                out.insert("path".into(), json!(path));
            }
            Some(_) => {}
        }
        if let Some(time) = &p.time {
            out.insert("released".into(), json!(time));
        }
    }
    if let Some(l) = Some(latest.unwrap_or(p)).filter(|l| l.is_abandoned()) {
        out.insert("replacement".into(), json!(l.replacement_package()));
    }
    if !p.suggests.is_empty() {
        out.insert("suggests".into(), Value::Object(p.suggests.clone()));
    }
    if let Some(support) = p.meta.get("support").filter(|s| s.as_object().is_some_and(|o| !o.is_empty())) {
        out.insert("support".into(), support.clone());
    }
    if let Some(autoload) = p.autoload.as_object().filter(|a| !a.is_empty()) {
        let mut rules = Map::new();
        for (kind, value) in autoload {
            match kind.as_str() {
                "psr-0" | "psr-4" => {
                    let psr: Map<String, Value> = value
                        .as_object()
                        .into_iter()
                        .flatten()
                        .map(|(ns, path)| {
                            let path = if komposer_package::loader::php_empty(path) { json!(".") } else { path.clone() };
                            (if ns.is_empty() { "*".to_string() } else { ns.clone() }, path)
                        })
                        .collect();
                    rules.insert(kind.clone(), php_map(psr));
                }
                "classmap" => {
                    rules.insert(kind.clone(), value.clone());
                }
                _ => {}
            }
        }
        out.insert("autoload".into(), php_map(rules));
    }
    if !strings(Some(&p.include_path)).is_empty() {
        out.insert("include_path".into(), p.include_path.clone());
    }
    for (kind, key) in [
        (LinkType::Require, "requires"),
        (LinkType::DevRequire, "devRequires"),
        (LinkType::Provide, "provides"),
        (LinkType::Conflict, "conflicts"),
        (LinkType::Replace, "replaces"),
    ] {
        let links: Map<String, Value> = p.links(kind).iter().map(|l| (l.target.clone(), json!(l.pretty_constraint))).collect();
        if !links.is_empty() {
            out.insert(key.into(), Value::Object(links));
        }
    }
    io.write(&json::encode(&Value::Object(out), Flags::FILE));
}

/// `SpdxLicenses::getLicenseByIdentifier()`: (identifier, full name, OSI approved).
fn spdx_license(id: &str) -> Option<(&'static str, &'static str, bool)> {
    static LICENSES: LazyLock<Vec<(String, String, bool)>> = LazyLock::new(|| {
        let data: serde_json::Map<String, Value> = serde_json::from_str(include_str!("../../res/spdx-licenses.json")).unwrap();
        data.into_iter().map(|(id, v)| (id, v[0].as_str().unwrap_or_default().to_string(), v[1].as_bool().unwrap_or(false))).collect()
    });
    LICENSES.iter().find(|(canonical, _, _)| canonical.eq_ignore_ascii_case(id)).map(|(c, n, osi)| (c.as_str(), n.as_str(), *osi))
}

fn license_url(id: &str) -> String {
    format!("https://spdx.org/licenses/{id}.html#licenseText")
}

/// `getRootRequires()`: names required (or dev-required) by the root package.
fn root_requires(composer: Option<&Composer>) -> Vec<String> {
    let Some(c) = composer else { return vec![] };
    let mut names: Vec<String> = vec![];
    for link in c.root.package.requires.iter().chain(&c.root.package.dev_requires) {
        if !names.contains(&link.target) {
            names.push(link.target.clone());
        }
    }
    names
}

/// All packages of the repositories, in order (`CompositeRepository::getPackages()`).
fn packages_of(repos: &[Arc<dyn Repository>]) -> Result<Vec<PackageRef>> {
    let mut out = vec![];
    for repo in repos {
        out.extend(repo.packages()?);
    }
    Ok(out)
}

/// `BasePackage::packageNamesToRegexp()` over the lowercased `--ignore` names.
fn names_regex(names: &[String]) -> Regex {
    let patterns: Vec<String> = names.iter().map(|n| regex::escape(&n.to_lowercase()).replace(r"\*", ".*")).collect();
    Regex::new(&format!("(?i)^(?:{})$", patterns.join("|"))).unwrap()
}

/// The package an alias (of an alias) points to.
fn unalias(p: &PackageRef) -> PackageRef {
    let mut p = p.clone();
    while let Some(of) = p.alias_of.clone() {
        p = of;
    }
    p
}

/// `realpath(getInstallPath())`: `None` where there is no install path, empty where it does not exist.
fn install_path(c: Option<&Composer>, p: &Package) -> Option<String> {
    let path = c?.install_path(p)?;
    Some(std::fs::canonicalize(path).map(|p| p.display().to_string()).unwrap_or_default())
}

/// `PackageInfo::getViewSourceUrl()`.
fn view_source_url(p: &Package) -> Option<String> {
    match p.meta.get("support").and_then(|s| s.get("source")).and_then(Value::as_str) {
        Some(url) if !url.is_empty() => Some(url.to_string()),
        _ => p.source.as_ref().map(|s| s.url.clone()),
    }
}

fn meta_str<'a>(p: &'a Package, key: &str) -> Option<&'a str> {
    p.meta.get(key).and_then(Value::as_str)
}

/// A list of scalars as strings.
fn strings(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array).into_iter().flatten().map(scalar).collect()
}

/// PHP's string conversion of a scalar.
fn scalar(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Bool(true) => "1".into(),
        Value::Bool(false) | Value::Null => String::new(),
        v => v.to_string(),
    }
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// Seconds since the epoch of an RFC 3339 date as the package loader writes them.
fn timestamp(time: Option<&str>) -> Option<i64> {
    let t = time?.as_bytes();
    let num = |a: usize, b: usize| std::str::from_utf8(t.get(a..b)?).ok()?.parse::<i64>().ok();
    let days = days_from_civil(num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let mut secs = days * 86400 + num(11, 13)? * 3600 + num(14, 16)? * 60 + num(17, 19)?;
    if let (Some(&sign), Some(h), Some(m)) = (t.get(19), num(20, 22), num(23, 25)) {
        let offset = h * 3600 + m * 60;
        secs += if sign == b'-' { offset } else { -offset };
    }
    Some(secs)
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    era * 146097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719468
}

/// (year, month, day, second of the day) of a timestamp, in UTC.
fn civil(ts: i64) -> (i64, i64, i64, i64) {
    let z = ts.div_euclid(86400) + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let (d, m) = (doy - (153 * mp + 2) / 5 + 1, if mp < 10 { mp + 3 } else { mp - 9 });
    (yoe + era * 400 + i64::from(m <= 2), m, d, ts.rem_euclid(86400))
}

/// `getRelativeTime()`, with dates compared in UTC.
fn relative_time(time: &str, now: i64) -> String {
    let Some(release) = timestamp(Some(time)) else { return String::new() };
    let (a, b) = (civil(release.min(now)), civil(release.max(now)));
    let date = |ts| {
        let (y, m, d, _) = civil(ts);
        (y, m, d)
    };
    if date(release) == date(now) {
        return "today".into();
    }
    let days = (release - now).abs() / 86400;
    // DateInterval's y and m: whole months between the two instants
    let mut months = (b.0 - a.0) * 12 + b.1 - a.1;
    if (b.2, b.3) < (a.2, a.3) {
        months -= 1;
    }
    let (y, m) = (months / 12, months % 12);
    match () {
        _ if days < 7 => "this week".into(),
        _ if days < 14 => "last week".into(),
        _ if m < 1 && days < 31 => format!("{} weeks ago", days / 7),
        _ if y < 1 => format!("{m} month{} ago", if m > 1 { "s" } else { "" }),
        _ => format!("{y} year{} ago", if y > 1 { "s" } else { "" }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use komposer_package::io::Verbosity;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    /// A temporary project directory, removed when dropped.
    struct TmpProject(PathBuf);

    impl std::ops::Deref for TmpProject {
        type Target = Path;
        fn deref(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TmpProject {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A project directory with composer.json, vendor/composer/installed.json and optionally composer.lock.
    fn project(composer_json: Value, installed: Value, lock: Option<Value>) -> TmpProject {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!("komposer-show-{}-{}", std::process::id(), N.fetch_add(1, AtomicOrdering::SeqCst)));
        std::fs::create_dir_all(dir.join("vendor/composer")).unwrap();
        std::fs::write(dir.join("composer.json"), serde_json::to_string_pretty(&composer_json).unwrap()).unwrap();
        let installed = json!({"packages": installed, "dev": true, "dev-package-names": []});
        std::fs::write(dir.join("vendor/composer/installed.json"), installed.to_string()).unwrap();
        if let Some(packages) = lock {
            std::fs::write(dir.join("composer.lock"), json!({"packages": packages, "packages-dev": [], "aliases": []}).to_string()).unwrap();
        }
        TmpProject(dir)
    }

    /// Runs `komposer show <argv>` on the project in `dir`.
    fn invoke(dir: &Path, argv: &[&str]) -> (Arc<Io>, Result<i32>) {
        let io = Arc::new(Io::buffered(Verbosity::Normal));
        let file = dir.join("composer.json");
        let contents = std::fs::read_to_string(&file).unwrap();
        let local = json::parse(&contents).unwrap().as_object().cloned().unwrap();
        let composer = Composer::from_parts(io.clone(), dir.to_path_buf(), file, contents, local);
        let Some(Command::Show(args)) = Cli::try_parse_from([&["komposer", "show"], argv].concat()).unwrap().command else { unreachable!() };
        let code = run(&io, composer, &args);
        (io, code)
    }

    /// (exit code, output without trailing spaces) of `komposer show <argv>`.
    fn run_in(dir: &Path, argv: &[&str]) -> (i32, String) {
        let (io, code) = invoke(dir, argv);
        let code = code.unwrap();
        // the warning about a missing PHP binary depends on the machine (and is shown once per process);
        // like Composer's BufferIO, the buffer keeps  and <warning> tags
        let output = io.output();
        let output: Vec<&str> = output.lines().filter(|l| !l.contains("PHP is not installed")).map(str::trim_end).collect();
        (code, output.join("\n").trim().to_string())
    }

    fn package(name: &str, version: &str, extra: Value) -> Value {
        let mut p = json!({"name": name, "version": version});
        p.as_object_mut().unwrap().extend(extra.as_object().cloned().unwrap_or_default());
        p
    }

    /// ShowCommandTest::testShow's project.
    fn show_project(require: Value) -> TmpProject {
        let available: Vec<Value> = [
            ("vendor/package", "v1.0.0", "generic description"),
            ("outdated/major", "v1.0.0", "outdated/major v1.0.0 description"),
            ("outdated/major", "v1.0.1", "outdated/major v1.0.1 description"),
            ("outdated/major", "v1.1.0", "outdated/major v1.1.0 description"),
            ("outdated/major", "v1.1.1", "outdated/major v1.1.1 description"),
            ("outdated/major", "v2.0.0", "outdated/major v2.0.0 description"),
            ("outdated/minor", "1.0.0", "outdated/minor v1.0.0 description"),
            ("outdated/minor", "1.0.1", "outdated/minor v1.0.1 description"),
            ("outdated/minor", "1.1.0", "outdated/minor v1.1.0 description"),
            ("outdated/minor", "1.1.1", "outdated/minor v1.1.1 description"),
            ("outdated/patch", "1.0.0", "outdated/patch v1.0.0 description"),
            ("outdated/patch", "1.0.1", "outdated/patch v1.0.1 description"),
        ]
        .iter()
        .map(|(n, v, d)| package(n, v, json!({"description": d})))
        .collect();
        let installed = json!([
            package("vendor/package", "v1.0.0", json!({"description": "description of installed package"})),
            package("outdated/major", "v1.0.0", json!({})),
            package("outdated/minor", "1.0.0", json!({})),
            package("outdated/patch", "1.0.0", json!({})),
        ]);
        project(
            json!({"name": "root/pkg", "version": "1.2.3", "repositories": [{"type": "package", "package": available}, {"packagist.org": false}], "require": require}),
            installed,
            Some(json!([package("vendor/locked", "3.0.0", json!({"description": "description of locked package"}))])),
        )
    }

    const LEGEND: &str = "Legend:\n! patch or minor release available - update recommended\n~ major release available - update possible";

    #[test]
    fn show_lists() {
        let dir = show_project(json!({}));
        let show = |argv: &[&str]| run_in(&dir, argv).1;
        assert_eq!(
            show(&[]),
            "outdated/major 1.0.0\noutdated/minor 1.0.0\noutdated/patch 1.0.0\nvendor/package 1.0.0 description of installed package"
        );
        assert!(show(&["--installed", "--self"]).contains("\nroot/pkg       1.2.3\nvendor/package 1.0.0 description of installed package"));
        assert_eq!(show(&["--locked", "--self"]), "root/pkg      1.2.3\nvendor/locked 3.0.0 description of locked package");
        assert_eq!(
            show(&["-a"]),
            "outdated/major outdated/major v2.0.0 description\noutdated/minor outdated/minor v1.1.1 description\noutdated/patch outdated/patch v1.0.1 description\nvendor/package generic description"
        );
        assert_eq!(show(&["--direct"]), "");
        assert_eq!(show(&["-N", "outdated/*"]), "outdated/major\noutdated/minor\noutdated/patch");
        let all = show(&["--all"]);
        assert!(all.ends_with("\n\nlocked:\n  vendor/locked 3.0.0 description of locked package\n\navailable:\n  outdated/major outdated/major v2.0.0 description\n  outdated/minor outdated/minor v1.1.1 description\n  outdated/patch outdated/patch v1.0.1 description\n  vendor/package generic description\n\ninstalled:\n  outdated/major 1.0.0\n  outdated/minor 1.0.0\n  outdated/patch 1.0.0\n  vendor/package 1.0.0 description of installed package"), "{all}");
        assert!(all.starts_with("platform:\n  composer"));
        let rows: Value = serde_json::from_str(&show(&["--format=json"])).unwrap();
        assert_eq!(
            rows["installed"][3],
            json!({"name": "vendor/package", "direct-dependency": false, "homepage": null, "source": null, "version": "v1.0.0", "description": "description of installed package", "abandoned": false})
        );

        let dir = show_project(json!({"outdated/major": "*"}));
        assert_eq!(run_in(&dir, &["--direct"]).1, "outdated/major 1.0.0");
    }

    #[test]
    fn outdated_lists() {
        let dir = show_project(json!({}));
        let outdated = |argv: &[&str]| run_in(&dir, &[&["--latest", "--outdated"], argv].concat()).1;
        let sections = |direct: &str, transitive: &str| {
            format!(
                "{LEGEND}\n\nDirect dependencies required in composer.json:\n{direct}\n\nTransitive dependencies not required in composer.json:\n{transitive}"
            )
        };
        assert_eq!(
            outdated(&[]),
            sections("Everything up to date", "outdated/major 1.0.0 ~ 2.0.0\noutdated/minor 1.0.0 ! 1.1.1\noutdated/patch 1.0.0 ! 1.0.1")
        );
        assert_eq!(outdated(&["--major-only"]), sections("Everything up to date", "outdated/major 1.0.0 ~ 2.0.0"));
        assert_eq!(
            outdated(&["--patch-only"]),
            sections("Everything up to date", "outdated/major 1.0.0 ! 1.0.1\noutdated/minor 1.0.0 ! 1.0.1\noutdated/patch 1.0.0 ! 1.0.1")
        );
        assert_eq!(run_in(&dir, &["--latest", "--outdated", "--strict"]).0, 1);
        assert_eq!(run_in(&dir, &["--latest", "--outdated", "--strict", "--ignore=outdated/*"]).0, 0);
        let all = run_in(&dir, &["--latest"]).1;
        assert!(all.contains("= up to date version") && all.contains("vendor/package 1.0.0 = 1.0.0 description of installed package"), "{all}");

        let dir = show_project(json!({"outdated/minor": "*"}));
        assert_eq!(
            run_in(&dir, &["--latest", "--outdated", "--minor-only"]).1,
            sections("outdated/minor 1.0.0 ! 1.1.1", "outdated/major 1.0.0 ! 1.1.1\noutdated/patch 1.0.0 ! 1.0.1")
        );
        let dir = show_project(json!({"vendor/package": "*", "outdated/major": "*"}));
        assert_eq!(run_in(&dir, &["--latest", "--outdated", "--direct"]).1, format!("{LEGEND}\noutdated/major 1.0.0 ~ 2.0.0"));
        let dir = show_project(json!({"vendor/package": "*"}));
        assert_eq!(run_in(&dir, &["--latest", "--outdated", "--direct"]).1, "All your direct dependencies are up to date");
    }

    #[test]
    fn single_package_views() {
        let dir = project(json!({"name": "vendor/package", "version": "1.2.3", "repositories": [{"packagist.org": false}]}), json!([]), None);
        let (code, out) = run_in(&dir, &["--self"]);
        assert_eq!(code, 0);
        assert_eq!(
            out,
            "name     : vendor/package\ndescrip. :\nkeywords :\nversions : * 1.2.3\ntype     : library\nhomepage :\nsource   : []\ndist     : []\npath     :\nnames    : vendor/package"
        );
        assert_eq!(run_in(&dir, &["--self", "--name-only"]).1, "vendor/package");

        let required = package("vendor/package", "1.0.0", json!({"require": {"vendor/required-package": "^1.0", "php": "8.2.0"}, "license": "MIT"}));
        let dir = project(
            json!({"version": "1.0.0", "require": {"vendor/package": "1.0.0"}, "repositories": [{"packagist.org": false}]}),
            json!([
                required,
                package("vendor/required-package", "1.2.0", json!({"require": {"vendor/sub": "^2.0"}})),
                package("vendor/sub", "2.5.0", json!({}))
            ]),
            None,
        );
        assert_eq!(
            run_in(&dir, &["vendor/package", "--tree"]).1,
            "vendor/package 1.0.0\n|--php 8.2.0\n`--vendor/required-package ^1.0\n   `--vendor/sub ^2.0"
        );
        let tree: Value = serde_json::from_str(&run_in(&dir, &["vendor/package", "--tree", "--format=json"]).1).unwrap();
        assert_eq!(tree["installed"][0]["description"], Value::Null);
        assert_eq!(tree["installed"][0]["requires"][1]["requires"][0], json!({"name": "vendor/sub", "version": "^2.0"}));
        assert!(run_in(&dir, &["--tree"]).1.starts_with("vendor/package 1.0.0\n|--php 8.2.0"));
        let info = run_in(&dir, &["vendor/package"]).1;
        assert!(info.contains("\nlicense  : MIT License (MIT) (OSI approved) https://spdx.org/licenses/MIT.html#licenseText\n"), "{info}");
        assert!(info.ends_with("names    : vendor/package\n\nrequires\nvendor/required-package ^1.0\nphp 8.2.0"), "{info}");
        let json: Value = serde_json::from_str(&run_in(&dir, &["vendor/package", "--format=json"]).1).unwrap();
        assert_eq!(json["requires"], json!({"vendor/required-package": "^1.0", "php": "8.2.0"}));
        assert_eq!(json["licenses"][0]["osi"], "MIT");

        let error = |argv: &[&str]| invoke(&dir, argv).1.unwrap_err().to_string();
        assert_eq!(error(&["not/existing"]), "Package \"not/existing\" not found, try using --available (-a) to show all available packages.");
        assert_eq!(error(&["not/existing", "--all"]), "Package \"not/existing\" not found.");
        assert_eq!(
            error(&["ext-nonexisting"]),
            "Package \"ext-nonexisting\" not found, try using --platform (-p) to show platform packages, try using --available (-a) to show all available packages."
        );
        assert_eq!(error(&["--direct", "vendor/sub"]), "Package \"vendor/sub\" is installed but not a direct dependent of the root package.");
        assert_eq!(error(&["--self", "vendor/package"]), "You cannot use --self together with a package name");
    }

    #[test]
    fn invalid_option_combinations_fail() {
        let dir = show_project(json!({}));
        for argv in [
            &["--direct", "--all"][..],
            &["--direct", "-a"],
            &["--direct", "-p"],
            &["--tree", "--all"],
            &["--tree", "--latest"],
            &["--tree", "--path"],
            &["--patch-only", "--minor-only"],
            &["--format=test"],
        ] {
            assert_eq!(run_in(&dir, argv).0, 1, "{argv:?}");
        }
    }

    #[test]
    fn latest_targets() {
        let target = |v: &str, only| latest_target(v, only);
        assert_eq!(target("dev-main", (true, false, false)), None);
        assert_eq!(target("dev-main", (false, false, false)), Some(Some("dev-main".into())));
        assert_eq!(target("1.2.0.0", (false, false, false)), Some(None));
        assert_eq!(target("1.2.0.0", (true, false, false)), Some(Some(">=2,<9999999-dev".into())));
        assert_eq!(target("0.1.0.0", (true, false, false)), Some(Some(">=0.2,<9999999-dev".into())));
        assert_eq!(target("0.0.1.0", (true, false, false)), Some(Some(">=0.0.2,<9999999-dev".into())));
        assert_eq!(target("1.2.0.0", (false, true, false)), Some(Some("^1.2.0.0".into())));
        assert_eq!(target("1.0.0.0", (false, false, true)), Some(Some("~1.0.0".into())));
        assert_eq!(target("12.5.40.0", (false, false, true)), Some(Some("~12.5.40".into())));
        assert_eq!(target("0.1.2.0", (false, false, true)), Some(Some("~0.1.2.0".into())));
    }

    #[test]
    fn update_statuses() {
        let p = |v: &str| Package::new("a/a", &komposer_semver::normalize(v).unwrap(), v);
        assert_eq!(update_status(&p("1.0.0"), &p("1.0.0")), "up-to-date");
        assert_eq!(update_status(&p("1.1.1"), &p("1.0.0")), "semver-safe-update");
        assert_eq!(update_status(&p("2.0.0"), &p("1.0.0")), "update-possible");
        assert_eq!(update_status(&p("0.2.0"), &p("0.1.0")), "update-possible");
        assert_eq!(update_status(&p("0.1.2"), &p("0.1.0")), "semver-safe-update");
    }

    #[test]
    fn relative_times() {
        let now = timestamp(Some("2026-10-18T12:00:00+00:00")).unwrap();
        let ago = |t: &str| relative_time(t, now);
        assert_eq!(ago("2026-10-18T01:00:00+00:00"), "today");
        assert_eq!(ago("2026-10-16T04:59:27+00:00"), "this week");
        assert_eq!(ago("2026-10-09T12:00:00+00:00"), "last week");
        assert_eq!(ago("2026-10-03T14:03:50+00:00"), "2 weeks ago");
        assert_eq!(ago("2026-08-25T14:40:53+00:00"), "1 month ago");
        assert_eq!(ago("2026-04-18T13:00:00+00:00"), "5 months ago");
        assert_eq!(ago("2025-02-07T04:58:58+00:00"), "1 year ago");
        assert_eq!(ago("2024-10-18T12:00:00+00:00"), "2 years ago");
        assert_eq!(ago("2026-10-18T14:00:00+02:00"), "today");
        assert_eq!(timestamp(Some("1970-01-02T00:00:00-01:00")), Some(90000));
    }

    #[test]
    fn descriptions_are_cut_to_the_terminal_width() {
        assert_eq!(strim_width("Library that provides collection", 20), "Library that prov...");
        assert_eq!(strim_width("short", 20), "short");
        assert_eq!(strim_width("📐 Nette Schema: validating", 12), "📐 Nette ...");
        assert_eq!(first_line("\nfirst\r\nsecond"), "first");
    }
}
