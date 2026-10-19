//! `depends` (why) and `prohibits` (why-not): `BaseDependencyCommand` over
//! `InstalledRepository::getDependents()`.

use super::info::{compact_table, root_repo, tree_color, tree_line};
use crate::cli::{DependsArgs, ProhibitsArgs};
use crate::composer::Composer;
use anyhow::{Result, bail};
use komposer_package::io::Io;
use komposer_package::repository::Repository;
use komposer_package::{Link, LinkType, Package, PackageRef, ROOT_DEFAULT_PRETTY_VERSION, is_platform_package};
use komposer_repository::platform::{PlatformRepository, detected_runtime};
use komposer_semver::{Constraint, Op, parse_constraints};
use komposer_solver::PlatformRequirementFilter;
use serde_json::Value;
use std::collections::HashSet;
use std::sync::Arc;

/// `DependsCommand::execute()`.
pub fn depends(io: &Arc<Io>, args: &DependsArgs, _run_scripts: bool) -> Result<i32> {
    run(&Composer::load(io.clone())?, &args.package, None, args.recursive || args.tree, args.tree, args.locked)
}

/// `ProhibitsCommand::execute()`.
pub fn prohibits(io: &Arc<Io>, args: &ProhibitsArgs, _run_scripts: bool) -> Result<i32> {
    run(&Composer::load(io.clone())?, &args.package, Some(&args.version), args.recursive || args.tree, args.tree, args.locked)
}

/// `BaseDependencyCommand::doExecute()`; inverted (why-not) when a version is given.
fn run(c: &Composer, needle: &str, version: Option<&str>, recursive: bool, tree: bool, locked: bool) -> Result<i32> {
    let io = &c.io;
    let inverted = version.is_some();
    let (root_repo, root) = root_repo(&c.root);
    let mut packages = root_repo.packages()?;
    if locked {
        if !c.locker.is_locked()? {
            bail!("A valid composer.lock file is required to run this command with --locked");
        }
        packages.extend(c.locker.locked_repository(true)?.packages()?);
        packages.extend(PlatformRepository::new(&Value::Object(c.locker.platform_overrides()?), detected_runtime())?.packages()?);
    } else {
        let local = c.installed()?.packages()?;
        if local.is_empty() && !(root.requires.is_empty() && root.dev_requires.is_empty()) {
            io.write("<warning>No dependencies installed. Try running composer install or update, or use --locked.</warning>");
            return Ok(1);
        }
        packages.extend(local);
        packages.extend(PlatformRepository::from_config(&c.config)?.packages()?);
    }
    let name = needle.to_lowercase();
    // without PHP, the platform requirements config.platform does not declare cannot be checked
    let platform_filter = match inverted || is_platform_package(&name) {
        true => crate::installer::effective_platform_filter(&c.config, &PlatformRequirementFilter::IgnoreNothing, &[], io),
        false => PlatformRequirementFilter::IgnoreNothing,
    };
    let mut repo = Installed { packages, root, platform_filter };

    let mut found = repo.with_replacers_and_providers(&name);
    if found.is_empty() && is_platform_package(&name) && repo.platform_filter.is_ignored(&name) {
        found.push(Arc::new(Package::new(needle, "", "")));
    }
    if found.is_empty() {
        bail!("Could not find package \"{needle}\" in your project");
    }
    let text_constraint = version.unwrap_or("*");
    let parsed = parse_constraints(text_constraint)?;
    match repo.find_package(&name, &parsed) {
        // the version asked for is not installed: add it from the repositories so its conflicts count
        None => {
            let mut matched = None;
            for remote in c.repositories()? {
                matched = remote.find_package(&name, &parsed)?;
                if matched.is_some() {
                    break;
                }
            }
            if let Some(m) = matched {
                repo.packages.push(m);
            } else if is_platform_package(&name) {
                // PHP compares the lower bound with `Bound::zero()` by identity, so this always applies
                let lower = parsed.lower_bound();
                repo.packages.push(Arc::new(Package::new(needle, &lower.version, &lower.version)));
            } else {
                io.write_error(&format!(
                    "<error>Package \"{needle}\" could not be found with constraint \"{text_constraint}\", results below will most likely be incomplete.</error>"
                ));
            }
        }
        Some(m) if is_platform_package(&name) => {
            let source = if m.extra.get("config.platform") == Some(&Value::Bool(true)) { " (version provided by config.platform)" } else { "" };
            io.write_error(&format!("<info>Package \"{needle} {text_constraint}\" found in version \"{}\"{source}.</info>", m.pretty_version));
        }
        Some(m) if inverted => {
            io.write(&format!(
                "<comment>Package \"{needle}\" {} is already installed! To find out why, run `composer why {needle}`</comment>",
                m.pretty_version
            ));
            return Ok(0);
        }
        Some(_) => {}
    }

    // replaced packages are the actual starting point of inverted lookups
    let mut needles = vec![name.clone()];
    if inverted {
        needles.extend(found.iter().flat_map(|p| p.replaces.iter().map(|l| l.target.clone())));
    }
    let constraint = (text_constraint != "*").then_some(parsed);
    let results = repo.dependents(&needles, constraint.as_ref(), inverted, recursive, None);

    let mut code = i32::from(inverted);
    if results.is_empty() {
        let versions = match constraint {
            Some(_) => format!(" in versions {}matching {text_constraint}", if inverted { "not " } else { "" }),
            None => String::new(),
        };
        io.write_error(&format!("<info>There is no installed package depending on \"{needle}\"{versions}</info>"));
        code = i32::from(!inverted);
    } else if tree {
        let top = &found[0];
        let description = top.meta.get("description").and_then(Value::as_str).unwrap_or("");
        io.write(&format!("<info>{}</info> {} {description}", top.pretty_name, top.pretty_version));
        print_tree(io, &results, "", 1);
    } else {
        for line in table(&results) {
            io.write(&line);
        }
    }

    if inverted && !is_platform_package(needle) {
        let root = &repo.root;
        let command = if root.dev_requires.iter().any(|l| l.target == needle) {
            "require --dev"
        } else if root.requires.iter().any(|l| l.target == needle) {
            "require"
        } else {
            "update"
        };
        io.write_error(&format!(
            "Not finding what you were looking for? Try calling `composer {command} \"{needle}:{text_constraint}\" --dry-run` to get another view on the problem."
        ));
    }
    Ok(code)
}

/// A `[package, link, children]` entry of `getDependents()`.
struct Dependent {
    package: PackageRef,
    link: Link,
    /// `Link::getDescription()`
    relation: &'static str,
    /// `None` where the recursion stopped at a circular dependency
    children: Option<Vec<Dependent>>,
}

impl Dependent {
    fn new(package: &PackageRef, link: &Link, children: Option<Vec<Dependent>>) -> Dependent {
        Dependent { package: package.clone(), link: link.clone(), relation: link.kind.description(), children }
    }

    /// `(string) $link`.
    fn id(&self) -> String {
        format!("{} {} {} ({})", self.link.source, self.relation, self.link.target, self.link.constraint)
    }

    fn version(&self) -> &str {
        Some(self.package.pretty_version.as_str()).filter(|v| *v != ROOT_DEFAULT_PRETTY_VERSION).unwrap_or("")
    }
}

/// A link that matches anything, with a free-form constraint text (`but 1.0 is installed`).
fn note_link(source: &str, target: &str, text: String) -> Link {
    Link { source: source.into(), target: target.into(), constraint: Constraint::Any, kind: LinkType::Require, pretty_constraint: text }
}

/// PHP's `+=` on link arrays keyed by target: the first link per target wins.
fn add_links<'a>(links: &mut Vec<&'a Link>, extra: &'a [Link]) {
    for l in extra {
        if !links.iter().any(|x| x.target == l.target) {
            links.push(l);
        }
    }
}

fn eq(version: &str) -> Constraint {
    Constraint::single(Op::Eq, version)
}

/// `InstalledRepository`: the root package first, then locked or installed and platform packages.
struct Installed {
    packages: Vec<PackageRef>,
    root: PackageRef,
    /// platform requirements not to report as unsatisfied
    platform_filter: PlatformRequirementFilter,
}

impl Installed {
    /// `findPackagesWithReplacersAndProviders()`.
    fn with_replacers_and_providers(&self, name: &str) -> Vec<PackageRef> {
        let provides = |p: &&PackageRef| p.provides.iter().chain(&p.replaces).any(|l| l.target == name);
        self.packages.iter().filter(|p| p.name == name || provides(p)).cloned().collect()
    }

    /// `findPackage()`.
    fn find_package(&self, name: &str, constraint: &Constraint) -> Option<&PackageRef> {
        self.packages.iter().find(|p| p.name == name && constraint.matches(&eq(&p.version)))
    }

    /// `findPackages()`.
    fn find_packages<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a PackageRef> {
        self.packages.iter().filter(move |p| p.name == name)
    }

    /// `getDependents()`: the links that cause (or, inverted, prevent) the needles to be installed,
    /// recursively up to the root when asked.
    fn dependents(
        &self,
        needles: &[String],
        constraint: Option<&Constraint>,
        invert: bool,
        recurse: bool,
        found: Option<&[String]>,
    ) -> Vec<Dependent> {
        let mut needles: Vec<String> = needles.iter().map(|n| n.to_lowercase()).collect();
        let found = found.map_or_else(|| needles.clone(), <[String]>::to_vec);
        let recursion =
            |name: &str, tree: &[String]| if recurse { self.dependents(&[name.to_string()], None, false, true, Some(tree)) } else { vec![] };
        let mut results = vec![];
        for package in &self.packages {
            let is_root = Arc::ptr_eq(package, &self.root);
            // each package gets its own tree so every needle shows its complete dependents
            let mut in_tree = found.clone();
            let mut links: Vec<&Link> = package.requires.iter().collect();
            if !invert {
                // replacements are valid reasons for a package to be installed
                add_links(&mut links, &package.replaces);
                for link in &package.replaces {
                    for needle in needles.clone() {
                        if link.source != needle || !constraint.is_none_or(|c| link.constraint.matches(c)) {
                            continue;
                        }
                        if in_tree.contains(&link.target) {
                            results.push(Dependent::new(package, link, None));
                            continue;
                        }
                        in_tree.push(link.target.clone());
                        results.push(Dependent::new(package, link, Some(recursion(&link.target, &in_tree))));
                        needles.push(link.target.clone());
                    }
                }
            }
            if is_root {
                add_links(&mut links, &package.dev_requires);
            }
            for link in links {
                for needle in &needles {
                    if link.target != *needle || !constraint.is_none_or(|c| link.constraint.matches(c) != invert) {
                        continue;
                    }
                    if in_tree.contains(&link.source) {
                        results.push(Dependent::new(package, link, None));
                        continue;
                    }
                    in_tree.push(link.source.clone());
                    results.push(Dependent::new(package, link, Some(recursion(&link.source, &in_tree))));
                }
            }

            let conflicting = |link: &Link| self.find_packages(&link.target).filter(|p| link.constraint.matches(&eq(&p.version)) == invert).count();
            // when inverting, conflicts of the needles against installed packages
            if invert && needles.contains(&package.name) {
                for link in &package.conflicts {
                    (0..conflicting(link)).for_each(|_| results.push(Dependent::new(package, link, None)));
                }
            }
            // conflicts against the needles may explain why a version was selected (or rejected)
            for link in package.conflicts.iter().filter(|l| needles.contains(&l.target)) {
                (0..conflicting(link)).for_each(|_| results.push(Dependent::new(package, link, None)));
            }

            // when inverting, requirements of the needle that installed packages do not satisfy
            if invert
                && let Some(c) = constraint
                && needles.contains(&package.name)
                && c.matches(&eq(&package.version))
            {
                self.unsatisfied_requirements(package, &mut results);
            }
        }
        results
    }

    fn unsatisfied_requirements(&self, package: &PackageRef, results: &mut Vec<Dependent>) {
        let root = &self.root;
        'links: for link in &package.requires {
            if is_platform_package(&link.target) {
                if self.platform_filter.is_ignored(&link.target) || self.find_package(&link.target, &link.constraint).is_some() {
                    continue;
                }
                let state = match self.find_package(&link.target, &Constraint::Any) {
                    Some(p) => format!("but {} is installed", p.pretty_version),
                    None => "but it is missing".into(),
                };
                let note = note_link(&package.name, &link.target, format!("{} {state}", link.pretty_constraint));
                results.push(Dependent::new(package, &note, None));
                continue;
            }
            for pkg in &self.packages {
                let names = pkg.names(true);
                if !names.contains(&link.target) {
                    continue;
                }
                let mut version = eq(&pkg.version);
                if link.target != pkg.name
                    && let Some(provided) = pkg.replaces.iter().chain(&pkg.provides).find(|p| p.target == link.target)
                {
                    version = provided.constraint.clone();
                }
                if !link.constraint.matches(&version) {
                    results.push(Dependent::new(package, link, None));
                    // the root requirements might be the actual problem
                    match root
                        .requires
                        .iter()
                        .chain(&root.dev_requires)
                        .find(|r| names.contains(&r.target) && !r.constraint.matches(&link.constraint))
                    {
                        Some(root_req) => results.push(Dependent::new(root, root_req, None)),
                        None => {
                            let note = note_link(&root.name, &link.target, format!("but {} is installed", pkg.pretty_version));
                            results.push(Dependent { relation: "does not require", ..Dependent::new(root, &note, None) });
                        }
                    }
                }
                continue 'links;
            }
        }
    }
}

/// `printTable()`: one row per distinct link, the levels closest to the root first.
fn table(results: &[Dependent]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut table: Vec<Vec<String>> = vec![];
    let mut level: Vec<&Dependent> = results.iter().collect();
    while !level.is_empty() {
        let mut rows = vec![];
        let mut queue = vec![];
        for d in level {
            if !seen.insert(d.id()) {
                continue;
            }
            let version = Some(d.version()).filter(|v| !v.is_empty()).unwrap_or("-");
            rows.push(vec![
                d.package.pretty_name.clone(),
                version.into(),
                d.relation.into(),
                format!("{} ({})", d.link.target, d.link.pretty_constraint),
            ]);
            queue.extend(d.children.iter().flatten());
        }
        rows.append(&mut table);
        table = rows;
        level = queue;
    }
    compact_table(None, &table)
}

/// `printTree()`.
fn print_tree(io: &Io, results: &[Dependent], prefix: &str, level: usize) {
    for (i, d) in results.iter().enumerate() {
        let last = i + 1 == results.len();
        let package = format!("{} {}", tree_color(io, level, &d.package.pretty_name), d.version());
        let link = format!("{} {} {}", d.relation, tree_color(io, level - 1, &d.link.target), d.link.pretty_constraint);
        let circular = if d.children.is_none() { "(circular dependency aborted here)" } else { "" };
        let line = format!("{prefix}{}{} ({link}) {circular}", if last { "└──" } else { "├──" }, package.trim_end());
        io.write(&tree_line(io, line.trim_end()));
        if let Some(children) = &d.children {
            print_tree(io, children, &format!("{prefix}{}", if last { "   " } else { "│  " }), level + 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use komposer_package::io::Verbosity;
    use serde_json::json;

    fn pkg(config: Value) -> PackageRef {
        komposer_package::loader::load(config.as_object().unwrap(), Default::default()).unwrap().pop().unwrap()
    }

    /// BaseDependencyCommandTest's why fixture: root requires package2 and package3, package1 requires
    /// package2, which requires package3.
    fn why_fixture() -> Installed {
        let root = pkg(
            json!({"name": "__root__", "version": "1.0.0+no-version-set", "require": {"vendor1/package2": "1.3.0", "vendor1/package3": "2.3.0"}, "require-dev": {"vendor2/package1": "2.*"}}),
        );
        let packages = vec![
            root.clone(),
            pkg(json!({"name": "vendor1/package1", "version": "1.3.0", "require": {"vendor1/package2": "^2"}})),
            pkg(json!({"name": "vendor1/package2", "version": "2.3.0", "require": {"vendor1/package3": "^1"}})),
            pkg(json!({"name": "vendor1/package3", "version": "2.1.0"})),
            pkg(json!({"name": "vendor2/package1", "version": "2.0.0"})),
        ];
        Installed { packages, root, platform_filter: PlatformRequirementFilter::IgnoreNothing }
    }

    fn render(repo: &Installed, needle: &str, constraint: Option<&str>, recurse: bool) -> Vec<String> {
        let constraint = constraint.map(|c| parse_constraints(c).unwrap());
        let results = repo.dependents(&[needle.to_string()], constraint.as_ref(), constraint.is_some(), recurse, None);
        table(&results).iter().map(|l| l.trim_end().to_string()).collect()
    }

    #[test]
    fn why_tables() {
        let repo = why_fixture();
        assert!(render(&repo, "vendor1/package1", None, false).is_empty());
        assert_eq!(
            render(&repo, "vendor1/package3", None, false),
            ["__root__         -     requires vendor1/package3 (2.3.0)", "vendor1/package2 2.3.0 requires vendor1/package3 (^1)"]
        );
        assert_eq!(
            render(&repo, "vendor1/package3", None, true),
            [
                "__root__         -     requires vendor1/package2 (1.3.0)",
                "vendor1/package1 1.3.0 requires vendor1/package2 (^2)",
                "__root__         -     requires vendor1/package3 (2.3.0)",
                "vendor1/package2 2.3.0 requires vendor1/package3 (^1)",
            ]
        );
        assert_eq!(render(&repo, "vendor2/package1", None, false), ["__root__ - requires (for development) vendor2/package1 (2.*)"]);
    }

    #[test]
    fn why_tree() {
        let repo = why_fixture();
        let io = Io::buffered(Verbosity::Normal);
        print_tree(&io, &repo.dependents(&["vendor1/package3".into()], None, false, true, None), "", 1);
        assert_eq!(
            io.output(),
            "|--__root__ (requires vendor1/package3 2.3.0)\n`--vendor1/package2 2.3.0 (requires vendor1/package3 ^1)\n   |--__root__ (requires vendor1/package2 1.3.0)\n   `--vendor1/package1 1.3.0 (requires vendor1/package2 ^2)\n"
        );
    }

    #[test]
    fn why_not_tables() {
        // BaseDependencyCommandTest::testWhyNotCommandOutputs fixture (php 8.3.2 via config.platform)
        let root = pkg(
            json!({"name": "__root__", "version": "1.0.0+no-version-set", "require": {"vendor1/package1": "1.*", "php": "^8"}, "require-dev": {"vendor2/package1": "2.*", "vendor2/package2": "^1"}}),
        );
        let mut repo = Installed {
            packages: vec![
                root.clone(),
                pkg(json!({"name": "vendor1/package1", "version": "1.3.0"})),
                pkg(json!({"name": "vendor2/package1", "version": "2.0.0"})),
                pkg(json!({"name": "vendor2/package2", "version": "1.0.0", "require": {"vendor2/package3": "1.4.*", "php": "^8.2"}})),
                pkg(json!({"name": "vendor2/package3", "version": "1.4.0"})),
                pkg(json!({"name": "php", "version": "8.3.2"})),
            ],
            root,
            platform_filter: PlatformRequirementFilter::IgnoreNothing,
        };
        assert_eq!(render(&repo, "vendor1/package1", Some("3.*"), false), ["__root__ - requires vendor1/package1 (1.*)"]);
        assert!(render(&repo, "vendor1/package1", Some("^1.4"), false).is_empty());
        assert!(render(&repo, "php", Some("^8"), false).is_empty());
        assert_eq!(
            render(&repo, "php", Some("9.1.0"), false),
            ["__root__         -     requires php (^8)", "vendor2/package2 1.0.0 requires php (^8.2)"]
        );

        // the asked for version is added from the repositories: its own requirements are checked too
        repo.packages.push(pkg(json!({"name": "vendor2/package3", "version": "1.5.0", "require": {"vendor1/package1": "^2", "ext-foo": "*"}})));
        assert_eq!(
            render(&repo, "vendor2/package3", Some("1.5.0"), false),
            [
                "vendor2/package2 1.0.0 requires vendor2/package3 (1.4.*)",
                "vendor2/package3 1.5.0 requires vendor1/package1 (^2)",
                "__root__         -     requires vendor1/package1 (1.*)",
                "vendor2/package3 1.5.0 requires ext-foo (* but it is missing)",
            ]
        );
        // without PHP, platform requirements nothing declares are not reported
        repo.platform_filter = PlatformRequirementFilter::ignore_list(&["ext-*"]);
        assert_eq!(render(&repo, "vendor2/package3", Some("1.5.0"), false).len(), 3);
        repo.root = pkg(json!({"name": "__root__", "version": "1.0.0+no-version-set"}));
        repo.packages[0] = repo.root.clone();
        assert!(
            render(&repo, "vendor2/package3", Some("1.5.0"), false)
                .contains(&"__root__         -     does not require vendor1/package1 (but 1.3.0 is installed)".to_string())
        );
    }

    #[test]
    fn circular_replaces_are_cut_short() {
        let root = pkg(json!({"name": "root/pkg", "version": "1.0.0+no-version-set", "require": {"laravel/framework": "^13"}}));
        let framework = pkg(
            json!({"name": "laravel/framework", "version": "13.0.0", "require": {"psr/log": "^3"}, "replace": {"illuminate/support": "self.version"}}),
        );
        let packages = vec![root.clone(), framework, pkg(json!({"name": "psr/log", "version": "3.0.0"}))];
        let repo = Installed { packages, root, platform_filter: PlatformRequirementFilter::IgnoreNothing };
        let io = Io::buffered(Verbosity::Normal);
        print_tree(&io, &repo.dependents(&["psr/log".into()], None, false, true, None), "", 1);
        assert_eq!(
            io.output(),
            "`--laravel/framework 13.0.0 (requires psr/log ^3)\n   |--root/pkg (requires laravel/framework ^13)\n   |--laravel/framework 13.0.0 (replaces illuminate/support self.version)\n   |  `--laravel/framework 13.0.0 (replaces illuminate/support self.version) (circular dependency aborted here)\n   `--laravel/framework 13.0.0 (replaces illuminate/support self.version) (circular dependency aborted here)\n"
        );
    }
}
