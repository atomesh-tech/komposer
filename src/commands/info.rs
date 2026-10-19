//! `licenses`, `fund`, `suggests`, `browse`, and the console helpers the inspection commands share.

use crate::cli::{BrowseArgs, LicensesArgs, SuggestsArgs};
use crate::composer::Composer;
use crate::suggest::Suggestions;
use anyhow::{Result, anyhow, bail};
use indexmap::IndexMap;
use komposer_package::io::Io;
use komposer_package::json::{self, Flags};
use komposer_package::repository::{ArrayRepository, LoadRequest, LoadResult, RepoKind, Repository};
use komposer_package::{Package, PackageRef, RootPackage};
use komposer_repository::platform::{PlatformRepository, detected_runtime};
use komposer_semver::{Constraint, Stability};
use regex::Regex;
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, LazyLock};

/// `LicensesCommand::execute()`.
pub fn licenses(io: &Arc<Io>, args: &LicensesArgs, _run_scripts: bool) -> Result<i32> {
    let c = Composer::load(io.clone())?;
    let root = &c.root.package;
    let mut packages = if args.locked {
        if !c.locker.is_locked()? {
            bail!("Valid composer.json and composer.lock files are required to run this command with --locked");
        }
        c.locker.locked_repository(!args.no_dev)?.packages()?
    } else {
        let local = c.installed()?.packages()?;
        if args.no_dev { filter_required_packages(&local, root) } else { local }
    };
    packages.sort_by(|a, b| a.name.cmp(&b.name));
    licenses_output(io, root, &packages, &args.format)?;
    Ok(0)
}

fn licenses_output(io: &Io, root: &Package, packages: &[PackageRef], format: &str) -> Result<()> {
    let joined = |p: &Package| Some(license_ids(p).join(", ")).filter(|l| !l.is_empty()).unwrap_or_else(|| "none".into());
    match format {
        "text" => {
            io.write(&format!("Name: <comment>{}</comment>", root.pretty_name));
            io.write(&format!("Version: <comment>{}</comment>", root.full_pretty_version()));
            io.write(&format!("Licenses: <comment>{}</comment>", joined(root)));
            io.write("Dependencies:");
            io.write("");
            let rows: Vec<Vec<String>> = packages.iter().map(|p| vec![p.pretty_name.clone(), p.full_pretty_version(), joined(p)]).collect();
            for line in compact_table(Some(&["Name", "Version", "Licenses"]), &rows) {
                io.write(&line);
            }
        }
        "json" => {
            let mut dependencies = Map::new();
            for p in packages {
                dependencies.insert(p.pretty_name.clone(), json!({"version": p.full_pretty_version(), "license": license_ids(p)}));
            }
            let out = json!({"name": root.pretty_name, "version": root.full_pretty_version(), "license": license_ids(root), "dependencies": php_map(dependencies)});
            io.write(&json::encode(&out, Flags::FILE));
        }
        "summary" => {
            let mut used: IndexMap<String, usize> = IndexMap::new();
            for p in packages {
                let ids = license_ids(p);
                for id in if ids.is_empty() { vec!["none".to_string()] } else { ids } {
                    *used.entry(id).or_default() += 1;
                }
            }
            // arsort(): most used first, ties in first-seen order
            used.sort_by(|_, a, _, b| b.cmp(a));
            let rows: Vec<Vec<String>> = used.into_iter().map(|(l, n)| vec![l, n.to_string()]).collect();
            for line in style_guide_table(&["License", "Number of dependencies"], &rows) {
                io.write(&line);
            }
            io.write("");
        }
        _ => bail!("Unsupported format \"{format}\".  See help for supported formats."),
    }
    Ok(())
}

/// `CompletePackage::getLicense()`.
pub(crate) fn license_ids(p: &Package) -> Vec<String> {
    p.meta.get("license").and_then(Value::as_array).into_iter().flatten().filter_map(|l| l.as_str().map(String::from)).collect()
}

/// `FundCommand::execute()`. Funding comes from the default branch of each package when the
/// repositories have it, else from the installed version.
pub fn fund(io: &Arc<Io>, args: &crate::cli::FundArgs, _run_scripts: bool) -> Result<i32> {
    if !matches!(args.format.as_str(), "text" | "json") {
        io.write_error(&format!("Unsupported format \"{}\". See help for supported formats.", args.format));
        return Ok(1);
    }
    let c = Composer::load(io.clone())?;
    let local: Vec<PackageRef> = c.installed()?.packages()?.into_iter().filter(|p| !p.is_alias()).collect();
    let mut pending: IndexMap<String, ()> = local.iter().map(|p| (p.name.clone(), ())).collect();
    let names: Vec<(String, Option<Constraint>)> = pending.keys().map(|n| (n.clone(), None)).collect();
    let dev_only = HashSet::from([Stability::Dev]);
    let request = LoadRequest { names: &names, acceptable_stabilities: &dev_only, stability_flags: &HashMap::new(), already_loaded: &HashMap::new() };
    let mut fundings = BTreeMap::new();
    for p in Composite(c.repositories()?).load_packages(&request)?.packages {
        if !p.is_alias() && p.default_branch && p.funding().is_some() && pending.shift_remove(&p.name).is_some() {
            insert_funding(&mut fundings, &p);
        }
    }
    for p in local.iter().filter(|p| pending.contains_key(&p.name) && p.funding().is_some()) {
        insert_funding(&mut fundings, p);
    }

    if args.format == "json" {
        let json: serde_json::Map<String, Value> = fundings
            .iter()
            .map(|(vendor, links)| {
                let links = links.iter().map(|(url, packages)| (url.clone(), Value::from(packages.clone()))).collect();
                (vendor.clone(), Value::Object(links))
            })
            .collect();
        let json = if json.is_empty() { Value::Array(vec![]) } else { Value::Object(json) };
        io.write(&komposer_package::json::encode(&json, komposer_package::json::Flags::FILE));
        return Ok(0);
    }
    if fundings.is_empty() {
        io.write("No funding links were found in your package dependencies. This doesn't mean they don't need your support!");
        return Ok(0);
    }
    io.write("The following packages were found in your dependencies which publish funding information:");
    let mut prev = String::new();
    for (vendor, links) in &fundings {
        io.write("");
        io.write(&format!("<comment>{vendor}</comment>"));
        for (url, packages) in links {
            let line = format!("  <info>{}</info>", packages.join(", "));
            if line != prev {
                io.write(&line);
                prev = line;
            }
            io.write(&format!("    {url}"));
        }
    }
    io.write("");
    io.write("Please consider following these links and sponsoring the work of package authors!");
    io.write("Thank you!");
    Ok(0)
}

type Fundings = BTreeMap<String, IndexMap<String, Vec<String>>>;

/// `FundCommand::insertFundingData()`: vendor => url => package names.
fn insert_funding(fundings: &mut Fundings, p: &Package) {
    static GITHUB: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^https://github.com/([^/]+)$").unwrap());
    let (vendor, name) = p.pretty_name.split_once('/').unwrap_or((&p.pretty_name, ""));
    for option in p.funding().into_iter().flatten() {
        let Some(url) = option.get("url").and_then(Value::as_str).filter(|u| !u.is_empty() && *u != "0") else { continue };
        let url = match GITHUB.captures(url) {
            Some(m) if option.get("type").and_then(Value::as_str) == Some("github") => format!("https://github.com/sponsors/{}", &m[1]),
            _ => url.to_string(),
        };
        fundings.entry(vendor.to_string()).or_default().entry(url).or_default().push(name.to_string());
    }
}

/// `SuggestsCommand::execute()`.
pub fn suggests(io: &Arc<Io>, args: &SuggestsArgs, _run_scripts: bool) -> Result<i32> {
    let c = Composer::load(io.clone())?;
    let root = Arc::new(c.root.package.clone());
    let mut installed = vec![root.clone()];
    if c.locker.is_locked()? {
        installed.extend(PlatformRepository::new(&Value::Object(c.locker.platform_overrides()?), detected_runtime())?.packages()?);
        installed.extend(c.locker.locked_repository(!args.no_dev)?.packages()?);
    } else {
        installed.extend(PlatformRepository::from_config(&c.config)?.packages()?);
        installed.extend(c.installed()?.packages()?);
    }
    let mut reporter = Suggestions::default();
    for p in installed.iter().chain([&root]) {
        if args.packages.is_empty() || args.packages.contains(&p.name) {
            reporter.add_from_package(p);
        }
    }
    let mode =
        if args.list { Mode::List } else { Mode::Grouped { by_package: args.by_package || !args.by_suggestion, by_suggestion: args.by_suggestion } };
    let only_dependents_of = (args.packages.is_empty() && !args.all).then_some(&*root);
    output_suggestions(io, &reporter, mode, &installed, only_dependents_of);
    Ok(0)
}

/// `SuggestedPackagesReporter::MODE_*`.
#[derive(Clone, Copy)]
enum Mode {
    List,
    Grouped { by_package: bool, by_suggestion: bool },
}

/// `SuggestedPackagesReporter::output()`.
fn output_suggestions(io: &Io, reporter: &Suggestions, mode: Mode, installed: &[PackageRef], only_dependents_of: Option<&Package>) {
    let suggestions = reporter.filtered(installed, only_dependents_of);
    let mut suggesters: BTreeMap<&str, IndexMap<&str, &str>> = BTreeMap::new();
    let mut suggested: BTreeMap<&str, IndexMap<&str, &str>> = BTreeMap::new();
    for s in &suggestions {
        suggesters.entry(&s.source).or_default().insert(&s.target, &s.reason);
        suggested.entry(&s.target).or_default().insert(&s.source, &s.reason);
    }
    let item = |name: &str, reason: &str| match reason {
        "" | "0" => format!(" - <info>{name}</info>"),
        _ => format!(" - <info>{name}</info>: {}", escape_reason(reason)),
    };
    match mode {
        Mode::List => {
            for name in suggested.keys() {
                io.write(&format!("<info>{name}</info>"));
            }
            return;
        }
        Mode::Grouped { by_package, by_suggestion } => {
            if by_package {
                for (suggester, list) in &suggesters {
                    io.write(&format!("<comment>{suggester}</comment> suggests:"));
                    list.iter().for_each(|(name, reason)| io.write(&item(name, reason)));
                    io.write("");
                }
            }
            if by_suggestion {
                if by_package {
                    io.write(&"-".repeat(78));
                }
                for (name, list) in &suggested {
                    io.write(&format!("<comment>{name}</comment> is suggested by:"));
                    list.iter().for_each(|(suggester, reason)| io.write(&item(suggester, reason)));
                    io.write("");
                }
            }
        }
    }
    if only_dependents_of.is_some() {
        let diff = reporter.filtered(installed, None).len() - suggestions.len();
        if diff > 0 {
            io.write(&format!("<info>{diff} additional suggestions</info> by transitive dependencies can be shown with <info>--all</info>"));
        }
    }
}

/// `escapeOutput()`: no control characters, no style tags.
fn escape_reason(reason: &str) -> String {
    reason.replace('\n', " ").chars().filter(|c| !c.is_ascii_control()).collect::<String>().replace('<', "\\<")
}

/// `HomeCommand::execute()`.
pub fn browse(io: &Arc<Io>, args: &BrowseArgs, _run_scripts: bool) -> Result<i32> {
    let loaded = Composer::load(io.clone());
    let repos: Vec<Arc<dyn Repository>> = match &loaded {
        Ok(c) => {
            let mut repos = vec![root_repo(&c.root).0, Arc::new(c.installed()?)];
            repos.extend(c.repositories()?);
            repos
        }
        Err(_) => super::show::remote_repos(io, None)?.into_iter().map(|r| r.repo).collect(),
    };
    let mut packages = args.packages.clone();
    if packages.is_empty() {
        io.write_error("No package specified, opening homepage for the root package");
        packages = vec![loaded.as_ref().map_err(|e| anyhow!("{e:#}"))?.root.package.name.clone()];
    }
    let mut code = 0;
    for name in &packages {
        let (mut exists, mut url) = (false, None);
        'repos: for repo in &repos {
            for p in repo.find_packages(name, None)? {
                exists = true;
                url = browse_url(&p, args.homepage);
                if url.is_some() {
                    break 'repos;
                }
            }
        }
        if !exists {
            code = 1;
            io.write_error(&format!("<warning>Package {name} not found</warning>"));
        }
        match url {
            Some(url) if args.show => io.write(&format!("<info>{url}</info>")),
            Some(url) => open_browser(io, &url),
            None => {
                code = 1;
                let what = if args.homepage { "Invalid or missing homepage" } else { "Invalid or missing repository URL" };
                io.write_error(&format!("<warning>{what} for {name}</warning>"));
            }
        }
    }
    Ok(code)
}

/// `handlePackage()`'s URL choice: `support.source`, else the source URL, else (or with `--homepage`)
/// the homepage; only valid URLs.
fn browse_url(p: &Package, homepage: bool) -> Option<String> {
    let support_source = p.meta.get("support").and_then(|s| s.get("source")).filter(|v| !v.is_null());
    let url = match support_source {
        Some(v) => v.as_str(),
        None => p.source.as_ref().map(|s| s.url.as_str()),
    };
    let url = url.filter(|u| !homepage && !u.is_empty() && *u != "0").or_else(|| p.meta.get("homepage").and_then(Value::as_str))?;
    // FILTER_VALIDATE_URL, roughly: a scheme, a host, no whitespace
    static URL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z][A-Za-z0-9+.-]*://[^\s/?#]+[^\s]*$").unwrap());
    URL.is_match(url).then(|| url.to_string())
}

/// `openBrowser()`.
fn open_browser(io: &Io, url: &str) {
    if cfg!(windows) {
        let _ = std::process::Command::new("cmd").args(["/C", "start", "web", "explorer", url]).status();
        return;
    }
    match ["xdg-open", "open"].into_iter().find(|cmd| crate::scripts::find_executable(cmd).is_some()) {
        Some(cmd) => {
            let _ = std::process::Command::new(cmd).arg(url).status();
        }
        None => io.write_error(&format!("No suitable browser opening command found, open yourself: {url}")),
    }
}

/// `CompositeRepository`: all repositories' results merged, none shadowing another.
pub(crate) struct Composite(pub Vec<Arc<dyn Repository>>);

impl Repository for Composite {
    fn repo_name(&self) -> String {
        format!("composite repo ({})", self.0.iter().map(|r| r.repo_name()).collect::<Vec<_>>().join(", "))
    }

    fn load_packages(&self, req: &LoadRequest) -> Result<LoadResult> {
        let mut out = LoadResult::default();
        for repo in &self.0 {
            let result = repo.load_packages(req)?;
            out.packages.extend(result.packages);
            for name in result.names_found {
                if !out.names_found.contains(&name) {
                    out.names_found.push(name);
                }
            }
        }
        Ok(out)
    }

    fn packages(&self) -> Result<Vec<PackageRef>> {
        let mut out = vec![];
        for repo in &self.0 {
            out.extend(repo.packages()?);
        }
        Ok(out)
    }

    fn find_packages(&self, name: &str, constraint: Option<&Constraint>) -> Result<Vec<PackageRef>> {
        let mut out = vec![];
        for repo in &self.0 {
            out.extend(repo.find_packages(name, constraint)?);
        }
        Ok(out)
    }
}

/// `RootPackageRepository` holding a clone of the root package.
pub(crate) fn root_repo(root: &RootPackage) -> (Arc<dyn Repository>, PackageRef) {
    let mut repo = ArrayRepository::new("root package repo", RepoKind::Root);
    let package = repo.add_package(root.package.clone());
    (Arc::new(repo), package)
}

/// `RepositoryUtils::filterRequiredPackages()`: what `requirer` needs, transitively (require-dev ignored).
pub(crate) fn filter_required_packages(packages: &[PackageRef], requirer: &Package) -> Vec<PackageRef> {
    fn collect(packages: &[PackageRef], requirer: &Package, bucket: &mut Vec<PackageRef>) {
        for candidate in packages {
            let required = candidate.names(true).iter().any(|n| requirer.requires.iter().any(|l| l.target == *n));
            if required && !bucket.iter().any(|b| Arc::ptr_eq(b, candidate)) {
                bucket.push(candidate.clone());
                collect(packages, candidate, bucket);
            }
        }
    }
    let mut bucket = vec![];
    collect(packages, requirer, &mut bucket);
    bucket
}

/// PHP encodes an empty array as `[]`, not `{}`.
pub(crate) fn php_map(m: Map<String, Value>) -> Value {
    if m.is_empty() { Value::Array(vec![]) } else { Value::Object(m) }
}

/// `mb_strwidth()`: East Asian wide and fullwidth characters take two columns.
pub(crate) fn str_width(s: &str) -> usize {
    s.chars().map(char_width).sum()
}

pub(crate) fn char_width(c: char) -> usize {
    #[rustfmt::skip]
    const WIDE: [(u32, u32); 81] = [
        (0x1100, 0x115F), (0x231A, 0x231B), (0x2329, 0x232A), (0x23E9, 0x23EC), (0x23F0, 0x23F0), (0x23F3, 0x23F3), (0x25FD, 0x25FE),
        (0x2614, 0x2615), (0x2648, 0x2653), (0x267F, 0x267F), (0x2693, 0x2693), (0x26A1, 0x26A1), (0x26AA, 0x26AB), (0x26BD, 0x26BE),
        (0x26C4, 0x26C5), (0x26CE, 0x26CE), (0x26D4, 0x26D4), (0x26EA, 0x26EA), (0x26F2, 0x26F3), (0x26F5, 0x26F5), (0x26FA, 0x26FA),
        (0x26FD, 0x26FD), (0x2705, 0x2705), (0x270A, 0x270B), (0x2728, 0x2728), (0x274C, 0x274C), (0x274E, 0x274E), (0x2753, 0x2755),
        (0x2757, 0x2757), (0x2795, 0x2797), (0x27B0, 0x27B0), (0x27BF, 0x27BF), (0x2B1B, 0x2B1C), (0x2B50, 0x2B50), (0x2B55, 0x2B55),
        (0x2E80, 0x303E), (0x3041, 0x3247), (0x3250, 0x4DBF), (0x4E00, 0xA4C6), (0xA960, 0xA97C), (0xAC00, 0xD7A3), (0xF900, 0xFAD9),
        (0xFE10, 0xFE19), (0xFE30, 0xFE6B), (0xFF01, 0xFF60), (0xFFE0, 0xFFE6), (0x16FE0, 0x1B2FB), (0x1F004, 0x1F004), (0x1F0CF, 0x1F0CF),
        (0x1F18E, 0x1F18E), (0x1F191, 0x1F19A), (0x1F200, 0x1F320), (0x1F32D, 0x1F335), (0x1F337, 0x1F37C), (0x1F37E, 0x1F393),
        (0x1F3A0, 0x1F3CA), (0x1F3CF, 0x1F3D3), (0x1F3E0, 0x1F3F0), (0x1F3F4, 0x1F3F4), (0x1F3F8, 0x1F43E), (0x1F440, 0x1F440),
        (0x1F442, 0x1F4FC), (0x1F4FF, 0x1F53D), (0x1F54B, 0x1F54E), (0x1F550, 0x1F567), (0x1F57A, 0x1F57A), (0x1F595, 0x1F596),
        (0x1F5A4, 0x1F5A4), (0x1F5FB, 0x1F64F), (0x1F680, 0x1F6C5), (0x1F6CC, 0x1F6CC), (0x1F6D0, 0x1F6D2), (0x1F6D5, 0x1F6D7),
        (0x1F6EB, 0x1F6EC), (0x1F6F4, 0x1F6FC), (0x1F7E0, 0x1F7EB), (0x1F90C, 0x1F93A), (0x1F93C, 0x1F945), (0x1F947, 0x1F9FF),
        (0x1FA70, 0x1FAD6), (0x20000, 0x3134A),
    ];
    let c = c as u32;
    if WIDE.iter().any(|&(a, b)| (a..=b).contains(&c)) { 2 } else { 1 }
}

/// `text` padded with spaces to `width` columns.
fn pad_width(text: &str, width: usize) -> String {
    format!("{text}{}", " ".repeat(width.saturating_sub(str_width(text))))
}

/// Symfony's `Table` in the `compact` style: every cell padded to its column's width plus one
/// space; the header row (if any) styled `<info>`.
pub(crate) fn compact_table(header: Option<&[&str]>, rows: &[Vec<String>]) -> Vec<String> {
    let header: Option<Vec<String>> = header.map(|h| h.iter().map(|c| c.to_string()).collect());
    let all: Vec<&Vec<String>> = header.iter().chain(rows).collect();
    let columns = all.iter().map(|r| r.len()).max().unwrap_or(0);
    let widths: Vec<usize> = (0..columns).map(|i| all.iter().map(|r| r.get(i).map_or(0, |c| str_width(c))).max().unwrap_or(0) + 1).collect();
    all.iter()
        .enumerate()
        .map(|(n, row)| {
            (0..columns)
                .map(|i| {
                    let cell = pad_width(row.get(i).map_or("", String::as_str), widths[i]);
                    if n == 0 && header.is_some() { format!("<info>{cell}</info>") } else { cell }
                })
                .collect()
        })
        .collect()
}

/// `SymfonyStyle::table()`: the `symfony-style-guide` table style with an `<info>` header.
fn style_guide_table(header: &[&str], rows: &[Vec<String>]) -> Vec<String> {
    let header: Vec<String> = header.iter().map(|c| c.to_string()).collect();
    let widths: Vec<usize> = (0..header.len()).map(|i| rows.iter().chain([&header]).map(|r| str_width(&r[i])).max().unwrap_or(0) + 2).collect();
    let separator = format!(" {} ", widths.iter().map(|w| "-".repeat(*w)).collect::<Vec<_>>().join(" "));
    let line = |row: &[String], tag: bool| {
        let cells: Vec<String> = row.iter().zip(&widths).map(|(c, w)| pad_width(&format!(" {c} "), *w)).collect();
        let cells: Vec<String> = if tag { cells.into_iter().map(|c| format!("<info>{c}</info>")).collect() } else { cells };
        format!(" {} ", cells.join(" "))
    };
    let mut out = vec![separator.clone(), line(&header, true), separator.clone()];
    out.extend(rows.iter().map(|r| line(r, false)));
    out.push(separator);
    out
}

/// `writeTreeLine()`: ASCII tree characters when the output is not decorated.
pub(crate) fn tree_line(io: &Io, line: &str) -> String {
    if io.is_decorated() {
        return line.to_string();
    }
    line.replace('└', "`-").replace('├', "|-").replace("──", "-").replace('│', "|")
}

/// `text` in the tree color of `level` (`initStyles()`: green, yellow, cyan, magenta, blue).
pub(crate) fn tree_color(io: &Io, level: usize, text: &str) -> String {
    if !io.is_decorated() {
        return text.to_string();
    }
    format!("\x1b[{}m{text}\x1b[39m", [32, 33, 36, 35, 34][level % 5])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::suggest::Suggestion;
    use komposer_package::io::Verbosity;

    fn pkg(name: &str, extra: Value) -> PackageRef {
        let mut config = json!({"name": name, "version": "1.0.0"});
        config.as_object_mut().unwrap().extend(extra.as_object().cloned().unwrap_or_default());
        komposer_package::loader::load(config.as_object().unwrap(), Default::default()).unwrap().pop().unwrap()
    }

    #[test]
    fn compact_table_pads_cells_like_symfony() {
        let rows = vec![vec!["a/b".into(), "1.0".into()], vec!["vendor/long".into(), "v2".into()]];
        assert_eq!(compact_table(None, &rows), ["a/b         1.0 ", "vendor/long v2  "]);
        let io = Io::buffered(Verbosity::Normal);
        let header = compact_table(Some(&["Name", "Version"]), &rows);
        assert_eq!(io.format(&header[0]), "Name        Version ");
    }

    #[test]
    fn summary_table_matches_symfony_style() {
        let io = Io::buffered(Verbosity::Normal);
        let mit = pkg("a/a", json!({"license": "MIT"}));
        let none = pkg("b/b", json!({}));
        let both = pkg("c/c", json!({"license": ["MIT", "GPL-2.0-only"]}));
        licenses_output(&io, &pkg("root/pkg", json!({})), &[mit, none, both], "summary").unwrap();
        assert_eq!(
            io.output(),
            " -------------- ------------------------ \n  License        Number of dependencies  \n -------------- ------------------------ \n  MIT            2                       \n  none           1                       \n  GPL-2.0-only   1                       \n -------------- ------------------------ \n\n"
        );
    }

    #[test]
    fn licenses_text_and_json() {
        // LicensesCommandTest::testBasicRun / testFormatJson, reduced
        let io = Io::buffered(Verbosity::Normal);
        let root = pkg("dummy/pkg", json!({"license": "MIT"}));
        let packages =
            [pkg("first/pkg", json!({"license": "MIT"})), pkg("second/pkg", json!({"license": ["LGPL-2.0-only"]})), pkg("third/pkg", json!({}))];
        licenses_output(&io, &root, &packages, "text").unwrap();
        let expected = "Name: dummy/pkg\nVersion: 1.0.0\nLicenses: MIT\nDependencies:\n\nName       Version Licenses      \nfirst/pkg  1.0.0   MIT           \nsecond/pkg 1.0.0   LGPL-2.0-only \nthird/pkg  1.0.0   none          \n";
        assert_eq!(io.output(), expected);

        let io = Io::buffered(Verbosity::Normal);
        licenses_output(&io, &root, &packages[2..], "json").unwrap();
        let out: Value = serde_json::from_str(&io.output()).unwrap();
        assert_eq!(
            out,
            json!({"name": "dummy/pkg", "version": "1.0.0", "license": ["MIT"], "dependencies": {"third/pkg": {"version": "1.0.0", "license": []}}})
        );
        assert!(licenses_output(&io, &root, &packages, "xml").is_err());
    }

    #[test]
    fn funding_is_grouped_by_vendor_and_url() {
        // FundCommandTest cases: github user URLs become sponsor pages, empty URLs are skipped
        let mut fundings = Fundings::new();
        insert_funding(
            &mut fundings,
            &pkg("foo/bar", json!({"funding": [{"type": "github", "url": "https://github.com/foo"}, {"type": "custom", "url": ""}]})),
        );
        insert_funding(
            &mut fundings,
            &pkg(
                "foo/baz",
                json!({"funding": [{"type": "github", "url": "https://github.com/foo"}, {"type": "github", "url": "https://github.com/foo/sponsor-page"}]}),
            ),
        );
        let foo = &fundings["foo"];
        assert_eq!(foo["https://github.com/sponsors/foo"], ["bar", "baz"]);
        assert_eq!(foo["https://github.com/foo/sponsor-page"], ["baz"]);
        assert_eq!(foo.len(), 2);
    }

    fn reporter(list: &[(&str, &str, &str)]) -> Suggestions {
        Suggestions {
            list: list.iter().map(|(s, t, r)| Suggestion { source: s.to_string(), target: t.to_string(), reason: r.to_string() }).collect(),
        }
    }

    #[test]
    fn suggestion_output_modes() {
        // SuggestsCommandTest / SuggestedPackagesReporterTest
        let r = reporter(&[("b/b", "x/x", "for x\nreasons"), ("a/a", "y/y", ""), ("a/a", "x/x", "<b>bold</b>")]);
        let run = |mode| {
            let io = Io::buffered(Verbosity::Normal);
            output_suggestions(&io, &r, mode, &[], None);
            io.output()
        };
        assert_eq!(run(Mode::List), "x/x\ny/y\n");
        assert_eq!(
            run(Mode::Grouped { by_package: true, by_suggestion: false }),
            "a/a suggests:\n - y/y\n - x/x: <b>bold</b>\n\nb/b suggests:\n - x/x: for x reasons\n\n"
        );
        let both = run(Mode::Grouped { by_package: true, by_suggestion: true });
        assert!(both.contains(&format!(
            "\n{}\nx/x is suggested by:\n - b/b: for x reasons\n - a/a: <b>bold</b>\n\ny/y is suggested by:\n - a/a\n\n",
            "-".repeat(78)
        )));

        // only direct dependencies of the root, with a hint about the rest; installed targets are dropped
        let root = pkg("root/pkg", json!({"require": {"a/a": "*"}}));
        let io = Io::buffered(Verbosity::Normal);
        output_suggestions(&io, &r, Mode::Grouped { by_package: true, by_suggestion: false }, &[pkg("y/y", json!({}))], Some(&root));
        assert_eq!(
            io.output(),
            "a/a suggests:\n - x/x: <b>bold</b>\n\n1 additional suggestions by transitive dependencies can be shown with --all\n"
        );
    }

    #[test]
    fn browse_url_choice() {
        // HomeCommandTest: support.source wins, --homepage switches, invalid URLs are rejected
        let p =
            pkg("a/a", json!({"homepage": "https://home.example", "source": {"type": "git", "url": "https://git.example/a.git", "reference": "x"}}));
        assert_eq!(browse_url(&p, false).as_deref(), Some("https://git.example/a.git"));
        assert_eq!(browse_url(&p, true).as_deref(), Some("https://home.example"));
        let p = pkg("a/a", json!({"support": {"source": "https://github.com/a/a/tree/1.0"}}));
        assert_eq!(browse_url(&p, false).as_deref(), Some("https://github.com/a/a/tree/1.0"));
        assert_eq!(browse_url(&p, true), None);
        assert_eq!(browse_url(&pkg("a/a", json!({"homepage": "not a url"})), true), None);
    }

    #[test]
    fn required_packages_follow_links_and_provides() {
        let root = pkg("root/pkg", json!({"require": {"a/a": "*", "virtual/v": "*"}, "require-dev": {"d/d": "*"}}));
        let a = pkg("a/a", json!({"require": {"b/b": "*"}}));
        let b = pkg("b/b", json!({"require": {"a/a": "*"}}));
        let provider = pkg("p/p", json!({"provide": {"virtual/v": "1.0"}}));
        let dev = pkg("d/d", json!({}));
        let names: Vec<String> = filter_required_packages(&[a, b, provider, dev], &root).iter().map(|p| p.name.clone()).collect();
        assert_eq!(names, ["a/a", "b/b", "p/p"]);
    }

    #[test]
    fn widths_count_wide_characters_twice() {
        assert_eq!(str_width("📐 Nette"), 8);
        assert_eq!(str_width("🛠  Nette"), 8);
        assert_eq!(str_width("日本"), 4);
    }
}
