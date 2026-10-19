//! `Problem`, `SolverProblemsException` and `Rule::getPrettyString()`: the human readable
//! "Your requirements could not be resolved" output.

use crate::pool::{Literal, Pool};
use crate::repository_set::RepositorySet;
use crate::request::{PrettyConstraint, Request};
use crate::rule::{Reason, RuleHandle, RuleSet, RuleType};
use anyhow::{Result, bail};
use indexmap::{IndexMap, IndexSet};
use komposer_package::repository::RepoKind;
use komposer_package::{Package, PackageRef, is_platform_package};
use komposer_semver::{Constraint, DEFAULT_BRANCH_ALIAS, Op, version_compare};
use regex::Regex;
use serde_json::Value;
use std::collections::HashSet;
use std::fmt;
use std::sync::LazyLock;

/// `Problem`: the rules that together make the request unsolvable, in sections.
#[derive(Clone, Debug, Default)]
pub struct Problem {
    seen: HashSet<RuleHandle>,
    sections: Vec<(usize, Vec<RuleHandle>)>,
    section: usize,
}

impl Problem {
    /// `addRule()`: duplicates are ignored.
    pub(crate) fn add_rule(&mut self, rule: RuleHandle) {
        if !self.seen.insert(rule) {
            return;
        }
        match self.sections.last_mut() {
            Some((section, rules)) if *section == self.section => rules.push(rule),
            _ => self.sections.push((self.section, vec![rule])),
        }
    }

    pub(crate) fn next_section(&mut self) {
        self.section += 1;
    }

    /// `getReasons()`: the rules of each section.
    pub fn reasons(&self) -> impl Iterator<Item = &[RuleHandle]> {
        self.sections.iter().map(|(_, rules)| rules.as_slice())
    }
}

/// `SolverProblemsException`.
#[derive(Clone, Debug)]
pub struct SolverProblems {
    pub problems: Vec<Problem>,
    /// The solver's rules, which the problems refer to.
    pub rules: RuleSet,
}

impl SolverProblems {
    /// `ERROR_DEPENDENCY_RESOLUTION_FAILED`, the exit code.
    pub const EXIT_CODE: i32 = 2;

    /// `getPrettyString()`. `ini_paths` is `IniHelper::getAll()`: the loaded php.ini (or `""`) then scanned
    /// files, used in the hint for missing extensions.
    pub fn pretty_string(
        &self,
        repository_set: &RepositorySet,
        request: &Request,
        pool: &Pool,
        verbose: bool,
        is_dev_extraction: bool,
        ini_paths: &[String],
    ) -> Result<String> {
        let ctx = Ctx::new(repository_set, request, pool, &self.rules, verbose);
        let mut missing_extensions: IndexSet<&str> = IndexSet::new();
        let mut caused_by_lock = false;
        let mut problems: IndexSet<String> = IndexSet::new();
        for problem in &self.problems {
            problems.insert(ctx.problem_pretty(problem)? + "\n");
            for rule in problem.reasons().flatten() {
                if let Some(required) = self.rules.reason(*rule).required_package().filter(|r| r.starts_with("ext-")) {
                    missing_extensions.insert(required);
                }
            }
            caused_by_lock = caused_by_lock || problem.reasons().flatten().any(|&r| ctx.is_caused_by_lock(r));
        }

        let mut text = String::from("\n");
        for (i, problem) in problems.iter().enumerate() {
            text += &format!("  Problem {}{problem}", i + 1);
        }

        let mut hints = vec![];
        if !is_dev_extraction && (text.contains("could not be found") || text.contains("no matching package found")) {
            hints.push("Potential causes:\n - A typo in the package name\n - The package is not available in a stable-enough version according to your minimum-stability setting\n   see <https://getcomposer.org/doc/04-schema.md#minimum-stability> for more details.\n - It's a private package and you forgot to add a custom repository to find it\n\nRead <https://getcomposer.org/doc/articles/troubleshooting.md> for further common problems.".to_string());
        }
        if !missing_extensions.is_empty() {
            hints.push(extension_hint(&missing_extensions, ini_paths));
        }
        if caused_by_lock && !is_dev_extraction && !request.update_allow_transitive_root_dependencies() {
            hints.push("Use the option --with-all-dependencies (-W) to allow upgrades, downgrades and removals for packages currently locked to specific versions.".to_string());
        }
        let plugin_api_mismatch = text.contains("found composer-plugin-api[2.0.0] but it does not match");
        if plugin_api_mismatch && text.contains("- ocramius/package-versions") {
            hints.push("<warning>ocramius/package-versions only provides support for Composer 2 in 1.8+, which requires PHP 7.4.</warning>\nIf you can not upgrade PHP you can require <info>composer/package-versions-deprecated</info> to resolve this with PHP 7.0+.".to_string());
        }
        if plugin_api_mismatch {
            hints.push("You are using Composer 2, which some of your plugins seem to be incompatible with. Make sure you update your plugins or report a plugin-issue to ask them to support Composer 2.".to_string());
        }
        if !hints.is_empty() {
            text += &format!("\n{}", hints.join("\n\n"));
        }
        Ok(text)
    }
}

impl fmt::Display for SolverProblems {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Failed resolving dependencies with {} problems, call getPrettyString to get formatted details", self.problems.len())
    }
}

impl std::error::Error for SolverProblems {}

/// `createExtensionHint()`.
fn extension_hint(extensions: &IndexSet<&str>, ini_paths: &[String]) -> String {
    let mut paths = if ini_paths.is_empty() { &[String::new()][..] } else { ini_paths };
    if paths[0].is_empty() {
        if paths.len() == 1 {
            return String::new();
        }
        paths = &paths[1..];
    }
    let args: Vec<String> = extensions.iter().map(|e| format!("--ignore-platform-req={e}")).collect();
    format!(
        "To enable extensions, verify that they are enabled in your .ini files:\n    - {}\nYou can also run `php --ini` in a terminal to see which files are used by PHP in CLI mode.\nAlternatively, you can run Composer with `{}` to temporarily ignore these required extensions.",
        paths.join("\n    - "),
        args.join(" ")
    )
}

impl RuleSet {
    /// `RuleSet::getPrettyString()`.
    pub fn pretty_string(&self, repository_set: &RepositorySet, request: &Request, pool: &Pool, verbose: bool) -> Result<String> {
        let ctx = Ctx::new(repository_set, request, pool, self, verbose);
        let mut out = String::from("\n");
        for t in [RuleType::Package, RuleType::Request, RuleType::Learned] {
            out += &format!("{:<8}: ", t.name());
            for &rule in self.of_type(t) {
                out += &ctx.rule_pretty(rule)?;
                out.push('\n');
            }
            out += "\n\n";
        }
        Ok(out)
    }

    /// `Rule::getPrettyString()`.
    pub fn rule_pretty_string(
        &self,
        rule: RuleHandle,
        repository_set: &RepositorySet,
        request: &Request,
        pool: &Pool,
        verbose: bool,
    ) -> Result<String> {
        Ctx::new(repository_set, request, pool, self, verbose).rule_pretty(rule)
    }
}

/// Everything the pretty printing needs.
struct Ctx<'a> {
    set: &'a RepositorySet,
    request: &'a Request,
    pool: &'a Pool,
    rules: &'a RuleSet,
    verbose: bool,
    /// `getPresentMap(true)`: ids of the present packages
    installed: HashSet<Literal>,
}

impl<'a> Ctx<'a> {
    fn new(set: &'a RepositorySet, request: &'a Request, pool: &'a Pool, rules: &'a RuleSet, verbose: bool) -> Ctx<'a> {
        let installed = request.present_packages().iter().filter_map(|p| pool.id_of(p)).collect();
        Ctx { set, request, pool, rules, verbose, installed }
    }

    fn package(&self, literal: Literal) -> &'a PackageRef {
        self.pool.package(literal)
    }

    /// `Problem::getPrettyString()`.
    fn problem_pretty(&self, problem: &Problem) -> Result<String> {
        // sections are merged in reverse, like Composer does
        let mut reasons: Vec<RuleHandle> = problem.sections.iter().rev().flat_map(|(_, rules)| rules.iter().copied()).collect();

        if let [rule] = reasons[..] {
            match self.rules.reason(rule) {
                Reason::RootRequire(req) if self.pool.what_provides(&req.0, Some(&req.1.constraint)).is_empty() => {
                    let (a, b) = self.missing_package_reason(&req.0, Some(&req.1))?;
                    return Ok(format!("\n    {a}{b}"));
                }
                Reason::LockedFilterListRemoved(p) => {
                    let (a, b) = missing_locked_package_reason(self.pool, p)?;
                    return Ok(format!("\n    {a}{b}"));
                }
                _ => {}
            }
        }

        let keys: Vec<(u8, String)> = reasons.iter().map(|&r| (rule_priority(self.rules.reason(r)), self.sortable_string(r))).collect();
        let mut order: Vec<usize> = (0..reasons.len()).collect();
        order.sort_by(|&a, &b| keys[b].0.cmp(&keys[a].0).then_with(|| php_compare(&keys[a].1, &keys[b].1)));
        reasons = order.into_iter().map(|i| reasons[i]).collect();
        self.format_deduplicated_rules(&reasons, "    ")
    }

    /// `getSortableString()`.
    fn sortable_string(&self, rule: RuleHandle) -> String {
        match self.rules.reason(rule) {
            Reason::RootRequire(req) => req.0.clone(),
            Reason::Fixed(p) | Reason::LockedFilterListRemoved(p) | Reason::PackageAlias(p) | Reason::PackageInverseAlias(p) => p.to_string(),
            Reason::PackageConflict { .. } | Reason::PackageRequires { .. } => {
                let source = self.source_package(rule);
                format!("{source}//{}", self.link_pretty_string(rule, source))
            }
            Reason::PackageSameName(name) => name.clone(),
            Reason::Learned(_) => self.rules.literals(rule).iter().map(|l| l.to_string()).collect::<Vec<_>>().join("-"),
        }
    }

    /// `Rule::getSourcePackage()`.
    fn source_package(&self, rule: RuleHandle) -> &'a Package {
        let literals = self.rules.literals(rule);
        let p1 = dedupe_default_branch_alias(self.package(literals[0]));
        if let Reason::PackageConflict { .. } = self.rules.reason(rule) {
            let p2 = dedupe_default_branch_alias(self.package(literals[1]));
            let link = self.rules.reason(rule).link().expect("conflict rules have a link");
            // package2 is the conflicter
            return if link.source == p1.name { p1 } else { p2 };
        }
        p1
    }

    /// The link constraint of a require/conflict rule. Its pretty string is the version for `self.version`
    /// links, which is what Composer's parsed link constraint carries.
    fn link_constraint(&self, rule: RuleHandle) -> PrettyConstraint {
        let reason = self.rules.reason(rule);
        let link = reason.link().expect("rule has a link");
        let pretty = match reason {
            Reason::PackageRequires { package, .. } | Reason::PackageConflict { package, .. } if link.pretty_constraint == "self.version" => {
                &package.base().pretty_version
            }
            _ => &link.pretty_constraint,
        };
        PrettyConstraint::new(link.constraint.clone(), pretty)
    }

    /// `Link::getPrettyString($source)` for the link of a require/conflict rule.
    fn link_pretty_string(&self, rule: RuleHandle, source: &Package) -> String {
        let link = self.rules.reason(rule).link().expect("rule has a link");
        format!("{} {} {} {}", source.pretty_string(), link.kind.description(), link.target, self.link_constraint(rule).pretty)
    }

    /// `Rule::isCausedByLock()`.
    fn is_caused_by_lock(&self, rule: RuleHandle) -> bool {
        let (name, constraint, check_unlocked) = match self.rules.reason(rule) {
            Reason::PackageRequires { .. } => {
                let link = self.rules.reason(rule).link().expect("require rules have a link");
                (&link.target, &link.constraint, true)
            }
            Reason::RootRequire(req) => (&req.0, &req.1.constraint, false),
            _ => return false,
        };
        if is_platform_package(name) {
            return false;
        }
        let Some(locked) = self.request.locked_repository_packages().iter().find(|p| p.name == *name) else { return false };
        self.pool.is_unacceptable_fixed_or_locked_package(locked)
            || !constraint.matches_version(&locked.version)
            // the required package was locked but has been unlocked and still matches
            || (check_unlocked && !self.request.is_locked_package(locked))
    }

    /// `Rule::getPrettyString()`.
    fn rule_pretty(&self, rule: RuleHandle) -> Result<String> {
        let literals = self.rules.literals(rule);
        let reason = self.rules.reason(rule);
        let literal_packages = |literals: &[Literal]| literals.iter().map(|&l| &**self.package(l)).collect::<Vec<&Package>>();
        Ok(match reason {
            Reason::RootRequire(req) => {
                let (name, constraint) = (&req.0, &req.1);
                let packages = self.pool.what_provides(name, Some(&constraint.constraint));
                if packages.is_empty() {
                    return Ok(format!("No package found to satisfy root composer.json require {name} {}", constraint.pretty));
                }
                let non_alias: Vec<&PackageRef> = packages.iter().map(|&l| self.package(l)).filter(|p| !p.is_alias()).collect();
                if let [p] = non_alias[..]
                    && self.request.is_locked_package(p)
                {
                    return Ok(format!(
                        "{} is locked to version {} and an update of this package was not requested.",
                        p.pretty_name, p.pretty_version
                    ));
                }
                format!(
                    "Root composer.json requires {name} {} -> satisfiable by {}.",
                    constraint.pretty,
                    self.list(&literal_packages(&packages), Some(&constraint.constraint), false)
                )
            }
            Reason::Fixed(p) => {
                let p = dedupe_default_branch_alias(p);
                if self.request.is_locked_package(p) {
                    format!("{} is locked to version {} and an update of this package was not requested.", p.pretty_name, p.pretty_version)
                } else {
                    format!("{} is present at version {} and cannot be modified by Composer", p.pretty_name, p.pretty_version)
                }
            }
            Reason::LockedFilterListRemoved(p) => {
                let p = dedupe_default_branch_alias(p);
                format!("{} {} was removed by a dependency policy (e.g. malware) and cannot be installed.", p.pretty_name, p.pretty_version)
            }
            Reason::PackageConflict { .. } => {
                let link = reason.link().expect("conflict rules have a link");
                let mut p1 = dedupe_default_branch_alias(self.package(literals[0]));
                let mut p2 = dedupe_default_branch_alias(self.package(literals[1]));
                let mut target = p1.pretty_string();
                // swap literals if they are not in the right order with package2 being the conflicter
                if link.source == p1.name {
                    (p1, p2) = (p2, p1);
                    target = format!("{} {}", p1.pretty_name, link.pretty_constraint);
                }
                // if the conflict is not directly against the package but something it provides/replaces, show that link
                if link.target != p1.name {
                    let provide = p1.provides.iter().find(|l| l.target == link.target).map(|l| ("provides", l));
                    let replace = p1.replaces.iter().find(|l| l.target == link.target).map(|l| ("replaces", l));
                    if let Some((kind, provided)) = replace.or(provide) {
                        target = format!(
                            "{} {} ({} {kind} {} {})",
                            link.target,
                            link.pretty_constraint,
                            p1.pretty_string(),
                            link.target,
                            provided.pretty_constraint
                        );
                    }
                }
                format!("{} conflicts with {target}.", p2.pretty_string())
            }
            Reason::PackageRequires { .. } => {
                let link = reason.link().expect("require rules have a link");
                let source = dedupe_default_branch_alias(self.package(literals[0]));
                let text = self.link_pretty_string(rule, source);
                if literals.len() > 1 {
                    format!("{text} -> satisfiable by {}.", self.list(&literal_packages(&literals[1..]), Some(&link.constraint), false))
                } else {
                    format!("{text} -> {}", self.missing_package_reason(&link.target, Some(&self.link_constraint(rule)))?.1)
                }
            }
            Reason::PackageSameName(replaced) => {
                let names: IndexSet<&str> = literals.iter().map(|&l| self.package(l).name.as_str()).collect();
                let all = literal_packages(literals);
                if names.len() <= 1 {
                    return Ok(format!(
                        "You can only install one version of a package, so only one of these can be installed: {}.",
                        self.list(&all, None, true)
                    ));
                }
                let why = if !names.contains(replaced.as_str()) {
                    format!("They {} replace {replaced} and thus cannot coexist.", if literals.len() == 2 { "both" } else { "all" })
                } else {
                    let replacers: Vec<&str> = names.iter().copied().filter(|n| n != replaced).collect();
                    let who = match replacers[..] {
                        [one] => format!("{one} replaces "),
                        _ => format!("[{}] replace ", replacers.join(", ")),
                    };
                    format!("{who}{replaced} and thus cannot coexist with it.")
                };
                let (installed, removable): (Vec<Literal>, Vec<Literal>) = literals.iter().partition(|l| self.installed.contains(&l.abs()));
                if !installed.is_empty() && !removable.is_empty() {
                    format!(
                        "{} cannot be installed as that would require removing {}. {why}",
                        self.list(&literal_packages(&removable), None, true),
                        self.list(&literal_packages(&installed), None, true)
                    )
                } else {
                    format!("Only one of these can be installed: {}. {why}", self.list(&all, None, true))
                }
            }
            Reason::Learned(_) => {
                let text = if let [literal] = literals[..] {
                    self.pool.literal_to_pretty_string(literal, &self.installed)
                } else {
                    let mut groups: IndexMap<&str, Vec<&Package>> = IndexMap::new();
                    for &l in literals {
                        let group = match (self.installed.contains(&l.abs()), l > 0) {
                            (true, true) => "keep",
                            (true, false) => "remove",
                            (false, true) => "install",
                            (false, false) => "don't install",
                        };
                        groups.entry(group).or_default().push(dedupe_default_branch_alias(self.package(l)));
                    }
                    let texts: Vec<String> = groups
                        .iter()
                        .map(|(group, packages)| {
                            format!("{group}{} {}", if packages.len() > 1 { " one of" } else { "" }, self.list(packages, None, false))
                        })
                        .collect();
                    texts.join(" | ")
                };
                format!("Conclusion: {text} (conflict analysis result)")
            }
            Reason::PackageAlias(_) | Reason::PackageInverseAlias(_) => {
                let inverse = matches!(reason, Reason::PackageInverseAlias(_));
                let (alias, of) = if inverse { (literals[1], literals[0]) } else { (literals[0], literals[1]) };
                let alias = self.package(alias);
                // "9999999-dev is an alias of dev-master" would be useless
                if alias.version == DEFAULT_BRANCH_ALIAS {
                    return Ok(String::new());
                }
                let of = dedupe_default_branch_alias(self.package(of));
                let tail = if inverse { "and must be installed with it." } else { "and thus requires it to be installed too." };
                format!("{} is an alias of {} {tail}", alias.pretty_string(), of.pretty_string())
            }
        })
    }

    /// `Problem::formatDeduplicatedRules()`: merges "pkg x requires" lines differing only by version.
    fn format_deduplicated_rules(&self, rules: &[RuleHandle], indent: &str) -> Result<String> {
        static HEAD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(\S+) (\S+) (requires|conflicts)").unwrap());
        static NAME_VERSION: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\S+ \S+ ").unwrap());
        static PLURAL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(%s%s (?:require|conflict))s").unwrap());

        let mut messages: IndexSet<String> = IndexSet::new();
        let mut templates: IndexMap<String, IndexMap<String, IndexMap<String, String>>> = IndexMap::new();
        for &rule in rules {
            let message = self.rule_pretty(rule)?;
            let dedupable = matches!(self.rules.reason(rule), Reason::PackageRequires { .. } | Reason::PackageConflict { .. });
            if let Some(m) = HEAD.captures(&message).filter(|_| dedupable) {
                let template = NAME_VERSION.replace(&message.replace('%', "%%"), "%s%s ").into_owned();
                let version = komposer_semver::normalize(&m[2]).unwrap_or_else(|_| m[2].to_string());
                let versions = templates.entry(template.clone()).or_default().entry(m[1].to_string()).or_default();
                versions.insert(version, m[2].to_string());
                for (version, pretty) in self.pool.removed_versions_by_package(self.source_package(rule)) {
                    versions.insert(version.clone(), pretty.clone());
                }
                messages.insert(template);
            } else if !message.is_empty() {
                messages.insert(message);
            }
        }

        let mut result = vec![];
        for message in &messages {
            let Some(packages) = templates.get(message) else {
                result.push(message.clone());
                continue;
            };
            for (package, versions) in packages {
                let mut versions: Vec<(&String, &String)> = versions.iter().collect();
                versions.sort_by(|a, b| version_compare(a.0, b.0));
                let versions: Vec<&str> =
                    if self.verbose { versions.iter().map(|v| v.1.as_str()).collect() } else { condense_version_list(&versions, 1, 16) };
                let rest = |template: &str| template[4..].replace("%%", "%");
                if versions.len() > 1 {
                    let template = PLURAL.replace(message, "$1");
                    result.push(format!("{package}[{}]{}", versions.join(", "), rest(&template)));
                } else {
                    result.push(format!("{package} {}{}", versions[0], rest(message)));
                }
            }
        }
        Ok(format!("\n{indent}- {}", result.join(&format!("\n{indent}- "))))
    }

    /// `Problem::getMissingPackageReason()`: why `name` has no candidate, as (prefix, reason).
    fn missing_package_reason(&self, name: &str, constraint: Option<&PrettyConstraint>) -> Result<(String, String)> {
        let text = constraint_to_text(constraint);
        let lower = name.to_lowercase();
        if is_platform_package(name) {
            // php, php-*, hhvm
            if lower.starts_with("php") || name == "hhvm" {
                let msg = format!("- Root composer.json requires {name}{text} but ");
                let version = self.platform_package_version(name);
                if name == "hhvm" {
                    return Ok(if self.pool.what_provides(name, None).is_empty() {
                        (msg, "HHVM was not detected on this machine, make sure it is in your PATH.".into())
                    } else {
                        (msg, "your HHVM version does not satisfy that requirement.".into())
                    });
                }
                return Ok(match version {
                    None => (
                        msg,
                        format!(
                            "the {name} package is disabled by your platform config. Enable it again with \"composer config platform.{name} --unset\"."
                        ),
                    ),
                    Some(v) => (msg, format!("your {name} version ({v}) does not satisfy that requirement.")),
                });
            }

            // php extensions
            if lower.starts_with("ext-") {
                if name.contains(' ') {
                    return Ok(("- ".into(), format!("PHP extension {name} should be required as {}.", name.replace(' ', "-"))));
                }
                let ext = &name[4..];
                let msg = format!("- Root composer.json requires PHP extension {name}{text} but ");
                let Some(version) = self.platform_package_version(name) else {
                    let providers = self.providers_list(name, 5)?.map(|p| format!("\n\n      Alternatively you can require one of these packages that provide the extension (or parts of it):\n      <warning>Keep in mind that the suggestions are automated and may not be valid or safe to use</warning>\n{p}")).unwrap_or_default();
                    if self.extension_loaded(ext) {
                        return Ok((
                            msg,
                            format!(
                                "the {name} package is disabled by your platform config. Enable it again with \"composer config platform.{name} --unset\".{providers}"
                            ),
                        ));
                    }
                    return Ok((msg, format!("it is missing from your system. Install or enable PHP's {ext} extension.{providers}")));
                };
                return Ok((msg, format!("it has the wrong version installed ({version}).")));
            }

            // linked libs
            if lower.starts_with("lib-") {
                let msg = format!("- Root composer.json requires linked library {name}{text} but ");
                if lower == "lib-icu" {
                    let error = if self.extension_loaded("intl") {
                        "it has the wrong version installed, try upgrading the intl extension."
                    } else {
                        "it is missing from your system, make sure the intl extension is loaded."
                    };
                    return Ok((msg, error.into()));
                }
                let providers = self.providers_list(name, 5)?.map(|p| format!("\n\n      Alternatively you can require one of these packages that provide the library (or parts of it):\n      <warning>Keep in mind that the suggestions are automated and may not be valid or safe to use</warning>\n{p}")).unwrap_or_default();
                return Ok((
                    msg,
                    format!(
                        "it has the wrong version installed or is missing from your system, make sure to load the extension providing it.{providers}"
                    ),
                ));
            }
        }

        let mut locked_package = None;
        for p in self.request.locked_packages() {
            if p.name == name {
                if self.pool.is_unacceptable_fixed_or_locked_package(&p) {
                    return Ok((
                        "- ".into(),
                        format!(
                            "{} is fixed to {} (lock file version) by a partial update but that version is rejected by your minimum-stability. Make sure you list it as an argument for the update command.",
                            p.pretty_name, p.pretty_version
                        ),
                    ));
                }
                locked_package = Some(p);
                break;
            }
        }

        let prefix = format!("- Root composer.json requires {name}{text}, ");
        let c = constraint.map(|c| &c.constraint);
        if let Some(pc) = constraint
            && matches!(pc.constraint, Constraint::Single { op: Op::Eq, .. })
        {
            static BRANCH_HASH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^dev-.*#.*").unwrap());
            static ALIAS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r" +as +([^,\s|]+)$").unwrap());
            if BRANCH_HASH.is_match(&pc.pretty) {
                let branch = ALIAS.replace(&pc.pretty, "").into_owned();
                let either = Constraint::Multi {
                    constraints: vec![Constraint::single(Op::Eq, branch.clone()), Constraint::single(Op::Eq, branch.replace('#', "+"))],
                    conjunctive: false,
                };
                let packages = self.set.find_packages(name, Some(&either), 0)?;
                if !packages.is_empty() {
                    return Ok((
                        prefix,
                        format!(
                            "found {}. The # character in branch names is replaced by a + character. Make sure to require it as \"{}\".",
                            self.found_list(&packages, c),
                            pc.pretty.replace('#', "+")
                        ),
                    ));
                }
            }
        }

        // the package exists in normal conditions, so it must be rejected by something else
        let found = self.set.find_packages_in(name, c, 0)?;
        if !found.is_empty() {
            let packages: Vec<PackageRef> = found.iter().map(|(_, p)| p.clone()).collect();
            let list = self.found_list(&packages, c);
            let conflict = if has_multiple_names(&packages) { "these conflict" } else { "it conflicts" };
            if let Some(root) = self.set.root_requires().get(name)
                && !packages.iter().any(|p| root.constraint.matches_version(&p.version))
            {
                return Ok((prefix, format!("found {list} but {conflict} with your root composer.json require ({}).", root.pretty)));
            }
            for n in packages[0].names(true) {
                if let Some(temp) = self.set.temporary_constraints().get(&n)
                    && !packages.iter().any(|p| temp.constraint.matches_version(&p.version))
                {
                    return Ok((
                        format!("- Root composer.json requires {n}{text}, "),
                        format!("found {list} but {conflict} with your temporary update constraint ({n}:{}).", temp.pretty),
                    ));
                }
            }
            if let Some(locked) = &locked_package {
                let fixed = Constraint::single(Op::Eq, locked.version.clone());
                if !packages.iter().any(|p| fixed.matches_version(&p.version)) {
                    return Ok((
                        prefix,
                        format!(
                            "found {list} but the package is fixed to {} (lock file version) by a partial update and that version does not match. Make sure you list it as an argument for the update command.",
                            locked.pretty_version
                        ),
                    ));
                }
            }
            if let Some(c) = c {
                if self.pool.is_abandoned_removed(name, c) {
                    return Ok((
                        prefix,
                        format!(
                            "found {list} but these were not loaded, because they are abandoned and you configured \"policy.abandoned.block\" to true."
                        ),
                    ));
                }
                if self.pool.is_security_removed(name, c) {
                    return Ok((
                        prefix,
                        format!(
                            "found {list} but these were not loaded, because they are affected by security advisories {}",
                            self.advisories_sentence(name, c, &packages)?
                        ),
                    ));
                }
                if self.pool.is_filter_list_removed(name, c) {
                    let (filters, remedy) = filter_list_sentence(self.pool, name, c);
                    return Ok((prefix, format!("found {list} but these were not loaded, because they were {filters}{remedy}")));
                }
            }
            if found.iter().all(|(repo, _)| self.set.repositories()[*repo].kind() == RepoKind::Lock) {
                return Ok((
                    prefix,
                    format!(
                        "found {list} in the lock file but not in remote repositories, make sure you avoid updating this package to keep the one from the lock file."
                    ),
                ));
            }
            return Ok((
                prefix,
                format!(
                    "found {list} but these were not loaded, likely because {} with another require.",
                    if has_multiple_names(&packages) { "they conflict" } else { "it conflicts" }
                ),
            ));
        }

        // the package is found when bypassing stability checks
        let found = self.set.find_packages_in(name, c, RepositorySet::ALLOW_UNACCEPTABLE_STABILITIES)?;
        if !found.is_empty() {
            // a valid package could be found in a lower priority repository
            let all_repos = self.set.find_packages_in(name, c, RepositorySet::ALLOW_SHADOWED_REPOSITORIES)?;
            if !all_repos.is_empty() {
                return self.check_for_lower_prio_repo(name, &found, &all_repos, "minimum-stability", constraint);
            }
            let packages: Vec<PackageRef> = found.into_iter().map(|(_, p)| p).collect();
            let does = if has_multiple_names(&packages) { "these do" } else { "it does" };
            return Ok((prefix, format!("found {} but {does} not match your minimum-stability.", self.found_list(&packages, c))));
        }

        // the package is found when bypassing the constraint and stability checks
        let found = self.set.find_packages_in(name, None, RepositorySet::ALLOW_UNACCEPTABLE_STABILITIES)?;
        if !found.is_empty() {
            let all_repos = self.set.find_packages_in(name, c, RepositorySet::ALLOW_SHADOWED_REPOSITORIES)?;
            if !all_repos.is_empty() {
                return self.check_for_lower_prio_repo(name, &found, &all_repos, "constraint", constraint);
            }
            let mut suffix = String::new();
            if let Some(Constraint::Single { version, .. }) = c
                && version == "dev-master"
                && let Some(p) = found.iter().map(|(_, p)| p).find(|p| p.version == "dev-default" || p.version == "dev-main")
            {
                suffix = format!(" Perhaps dev-master was renamed to {}?", p.pretty_version);
            }
            // a name match with the root package
            if self.set.repositories()[found[0].0].kind() == RepoKind::Root {
                suffix = " See https://getcomposer.org/dep-on-root for details and assistance.".into();
            }
            let packages: Vec<PackageRef> = found.into_iter().map(|(_, p)| p).collect();
            let does = if has_multiple_names(&packages) { "these do" } else { "it does" };
            return Ok((prefix, format!("found {} but {does} not match the constraint.{suffix}", self.found_list(&packages, c))));
        }

        static VALID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_./-]+$").unwrap());
        static VALID_CHARS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[A-Za-z0-9_./-]+").unwrap());
        if !VALID.is_match(name) {
            let illegal = VALID_CHARS.replace_all(name, "");
            return Ok((
                format!("- Root composer.json requires {name}, it "),
                format!("could not be found, it looks like its name is invalid, \"{illegal}\" is not allowed in package names."),
            ));
        }

        if let Some(providers) = self.providers_list(name, 15)? {
            return Ok((
                format!("- Root composer.json requires {name}{text}, it "),
                format!(
                    "could not be found in any version, but the following packages provide it:\n{providers}      Consider requiring one of these to satisfy the {name} requirement."
                ),
            ));
        }
        Ok((
            format!("- Root composer.json requires {name}, it "),
            "could not be found in any version, there may be a typo in the package name.".into(),
        ))
    }

    /// `formatPackagesUnique()`: `getPackageList()` with the optimizer's removed versions.
    fn list(&self, packages: &[&Package], constraint: Option<&Constraint>, removed_version_group: bool) -> String {
        package_list(packages, self.verbose, self.pool, constraint, removed_version_group)
    }

    fn found_list(&self, packages: &[PackageRef], constraint: Option<&Constraint>) -> String {
        let packages: Vec<&Package> = packages.iter().map(|p| &**p).collect();
        self.list(&packages, constraint, false)
    }

    /// The security advisories part of the missing package reason.
    fn advisories_sentence(&self, name: &str, constraint: &Constraint, packages: &[PackageRef]) -> Result<String> {
        let packagist = |id: &str| format!("<href={}>{id}</>", escape(&format!("https://packagist.org/security-advisories/{id}")));
        let advisories = self.set.matching_security_advisories(packages, false, true)?;
        let (list, ids): (Vec<String>, Vec<String>) = match advisories.advisories.get(name).filter(|a| !a.is_empty()) {
            Some(advisories) => advisories
                .iter()
                .map(|a| {
                    let shown = match a.link.as_deref().filter(|l| !l.is_empty()) {
                        Some(link) => format!("<href={}>{}</>", escape(link), a.advisory_id),
                        None if a.advisory_id.starts_with("PKSA-") => packagist(&a.advisory_id),
                        None => a.advisory_id.clone(),
                    };
                    (shown, a.advisory_id.clone())
                })
                .unzip(),
            None => self
                .pool
                .security_advisory_ids(name, constraint)
                .into_iter()
                .map(|id| (if id.starts_with("PKSA-") { packagist(&id) } else { id.clone() }, id))
                .unzip(),
        };
        let details = if ids.iter().all(|id| id.starts_with("PKSA-")) {
            " Go to https://packagist.org/security-advisories/ to find advisory details."
        } else {
            " Review the advisory details above for more information."
        };
        Ok(format!(
            "(\"{}\").{details} To ignore the advisories, add their IDs to the \"policy.advisories.ignore-id\" config or add the package to \"policy.advisories.ignore\". To turn the feature off entirely, you can set \"policy.advisories.block\" to false.",
            list.join("\", \"")
        ))
    }

    /// `computeCheckForLowerPrioRepo()`.
    fn check_for_lower_prio_repo(
        &self,
        name: &str,
        higher: &[(usize, PackageRef)],
        all_repos: &[(usize, PackageRef)],
        reason: &str,
        constraint: Option<&PrettyConstraint>,
    ) -> Result<(String, String)> {
        let text = constraint_to_text(constraint);
        let c = constraint.map(|c| &c.constraint);
        let next_repo = all_repos[0].0;
        let next: Vec<PackageRef> = all_repos.iter().take_while(|(r, _)| *r == next_repo).map(|(_, p)| p.clone()).collect();
        let repo = &self.set.repositories()[next_repo];
        let higher_packages: Vec<PackageRef> = higher.iter().map(|(_, p)| p.clone()).collect();

        if let Some((top_repo, top)) = higher.first()
            && self.set.repositories()[*top_repo].kind() == RepoKind::Root
        {
            return Ok((
                format!("- Root composer.json requires {name}{text}, it is "),
                format!(
                    "satisfiable by {} from {} but {} {} is the root package and cannot be modified. See https://getcomposer.org/dep-on-root for details and assistance.",
                    self.found_list(&next, c),
                    repo.repo_name(),
                    top.pretty_name,
                    top.pretty_version
                ),
            ));
        }

        if repo.kind() == RepoKind::Lock {
            let singular = higher.len() == 1;
            let mut suggestion = format!(
                "Make sure you either fix the {reason} or avoid updating this package to keep the one present in the lock file ({}).",
                self.found_list(&next, c)
            );
            // symlinked path repos cannot be locked so do not suggest keeping it locked
            if is_symlinked_path(&next[0]) {
                suggestion = format!(
                    "Make sure you fix the {reason} as packages installed from symlinked path repos are updated even in partial updates and the one from the lock file can thus not be used."
                );
            }
            return Ok((
                format!("- Root composer.json requires {name}{text}, "),
                format!(
                    "found {} but {} not match your {reason} and {} therefore not installable. {suggestion}",
                    self.found_list(&higher_packages, c),
                    if singular { "it does" } else { "these do" },
                    if singular { "is" } else { "are" }
                ),
            ));
        }

        Ok((
            format!("- Root composer.json requires {name}{text}, it is "),
            format!(
                "satisfiable by {} from {} but {} from {} has higher repository priority. The packages from the higher priority repository do not match your {reason} and are therefore not installable. That repository is canonical so the lower priority repo's packages are not installable. See https://getcomposer.org/repoprio for details and assistance.",
                self.found_list(&next, c),
                repo.repo_name(),
                self.found_list(&higher_packages, c),
                self.set.repositories()[higher[0].0].repo_name()
            ),
        ))
    }

    /// `getPlatformPackageVersion()`: the version of a platform package in the pool, `None` if it is missing.
    fn platform_package_version(&self, name: &str) -> Option<String> {
        let available = self.pool.what_provides(name, None);
        // Composer prefers the package from the platform repository, which is the one by that name
        let selected = available.iter().map(|&l| self.package(l)).find(|p| p.name == name).or(available.first().map(|&l| self.package(l)))?;
        if selected.name != name {
            // a package providing/replacing it
            if let Some(link) = selected.provides.iter().chain(&selected.replaces).find(|l| l.target == name) {
                let description = link.kind.description();
                return Some(format!("{} {}d by {}", link.pretty_constraint, &description[..description.len() - 1], selected.pretty_string()));
            }
        }
        let mut version = selected.pretty_version.clone();
        if selected.extra.get("config.platform") == Some(&Value::Bool(true)) {
            let description = selected.meta.get("description").and_then(Value::as_str).unwrap_or("");
            version += &format!("; {}", description.replace("Package ", ""));
        }
        Some(version)
    }

    /// PHP's `extension_loaded()`: komposer knows the platform from its platform repository only.
    fn extension_loaded(&self, ext: &str) -> bool {
        let name = format!("ext-{ext}");
        self.set
            .repositories()
            .iter()
            .filter(|r| r.kind() == RepoKind::Platform)
            .any(|r| r.is_platform_package_disabled(&name) || r.find_packages(&name, None).is_ok_and(|p| !p.is_empty()))
    }

    /// `getProvidersList()`.
    fn providers_list(&self, name: &str, max: usize) -> Result<Option<String>> {
        let providers = self.set.providers(name)?;
        if providers.is_empty() {
            return Ok(None);
        }
        let shown = if providers.len() > max + 1 { &providers[..max] } else { &providers[..] };
        let mut out = String::new();
        for (provider, description) in shown {
            let description = match description.as_deref().filter(|d| !d.is_empty()) {
                Some(d) => format!(" {}", truncate(d, 100)),
                None => String::new(),
            };
            out += &format!("      - {provider}{description}\n");
        }
        if providers.len() > max + 1 {
            out += &format!("      ... and {} more.\n", providers.len() - max);
        }
        Ok(Some(out))
    }
}

/// `Problem::getMissingLockedPackageReason()`.
pub fn missing_locked_package_reason(pool: &Pool, package: &Package) -> Result<(String, String)> {
    let prefix = format!("- Package {} {} (in the lock file) ", package.name, package.pretty_version);
    let constraint = Constraint::single(Op::Eq, package.version.clone());
    if !pool.is_filter_list_removed(&package.name, &constraint) {
        bail!("Filter list removed locked package must have version removed from pool.");
    }
    let (filters, remedy) = filter_list_sentence(pool, &package.name, &constraint);
    Ok((prefix, format!("was not loaded, because it was {filters}{remedy}")))
}

/// The filter list descriptions and the remedies of a filter-list removal.
fn filter_list_sentence(pool: &Pool, name: &str, constraint: &Constraint) -> (String, String) {
    let filters = pool.filter_list_entry_descriptions(name, constraint);
    let paths = |key: &str| filters.keys().map(|l| format!("\"policy.{l}.{key}\"")).collect::<Vec<_>>().join(" and ");
    let descriptions: Vec<&str> = filters.values().map(String::as_str).collect();
    (
        descriptions.join(", "),
        format!(
            ". To ignore filters for this package, add the package to the {} config. To turn the feature off entirely, you can set {} to false.",
            paths("ignore"),
            paths("block")
        ),
    )
}

/// PHP's `$a <=> $b` on strings: numeric strings (like a learned rule's `-12`) compare as numbers.
fn php_compare(a: &str, b: &str) -> std::cmp::Ordering {
    match (a.trim().parse::<f64>(), b.trim().parse::<f64>()) {
        (Ok(x), Ok(y)) if !a.trim().is_empty() && !b.trim().is_empty() => x.total_cmp(&y),
        _ => a.cmp(b),
    }
}

/// `getRulePriority()`.
fn rule_priority(reason: &Reason) -> u8 {
    match reason {
        Reason::Fixed(_) | Reason::LockedFilterListRemoved(_) => 3,
        Reason::RootRequire(_) => 2,
        Reason::PackageConflict { .. } | Reason::PackageRequires { .. } => 1,
        _ => 0,
    }
}

/// `deduplicateDefaultBranchAlias()`: the `9999999-dev` alias shows as the branch it aliases.
fn dedupe_default_branch_alias(p: &Package) -> &Package {
    match &p.alias_of {
        Some(of) if p.pretty_version == DEFAULT_BRANCH_ALIAS => of,
        _ => p,
    }
}

/// `Problem::getPackageList()`, e.g. `a/b[1.0, 1.1 (alias of dev-main)], c/d[2.0]`, including the versions
/// the pool optimizer removed (matching `constraint`, or that a package stands for with `removed_version_group`).
pub(crate) fn package_list(
    packages: &[&Package],
    verbose: bool,
    pool: &Pool,
    constraint: Option<&Constraint>,
    removed_version_group: bool,
) -> String {
    let mut prepared: IndexMap<&str, (&str, IndexMap<&str, String>)> = IndexMap::new();
    let mut has_default_branch: HashSet<&str> = HashSet::new();
    for p in packages {
        let entry = prepared.entry(&p.name).or_insert_with(|| (&p.pretty_name, IndexMap::new()));
        entry.0 = &p.pretty_name;
        let pretty = match &p.alias_of {
            Some(of) => format!("{} (alias of {})", p.pretty_version, of.pretty_version),
            None => p.pretty_version.clone(),
        };
        entry.1.insert(&p.version, pretty);
        if let Some(c) = constraint {
            for (version, pretty) in pool.removed_versions(&p.name, c) {
                entry.1.insert(version, pretty.clone());
            }
        }
        if removed_version_group {
            for (version, pretty) in pool.removed_versions_by_package(p) {
                entry.1.insert(version, pretty.clone());
            }
        }
        if p.default_branch {
            has_default_branch.insert(&p.name);
        }
    }

    let mut out = vec![];
    for (name, (pretty_name, mut versions)) in prepared {
        // remove the implicit default branch alias to avoid cruft in the display
        if has_default_branch.contains(name) {
            versions.shift_remove(DEFAULT_BRANCH_ALIAS);
        }
        let mut versions: Vec<(&str, &String)> = versions.iter().map(|(k, v)| (*k, v)).collect();
        versions.sort_by(|a, b| version_compare(a.0, b.0));
        let shown: Vec<&str> = if verbose { versions.iter().map(|v| v.1.as_str()).collect() } else { condense_version_list(&versions, 4, 16) };
        out.push(format!("{pretty_name}[{}]", shown.join(", ")));
    }
    out.join(", ")
}

/// `condenseVersionList()`: per major version, only the first and last of long lists.
fn condense_version_list<'v, K: AsRef<str>>(versions: &[(K, &'v String)], max: usize, max_dev: usize) -> Vec<&'v str> {
    if versions.len() <= max {
        return versions.iter().map(|v| v.1.as_str()).collect();
    }
    static MAJOR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(\d+)\..*").unwrap());
    let mut by_major: IndexMap<String, Vec<&str>> = IndexMap::new();
    for (version, pretty) in versions {
        let version = version.as_ref();
        let major = if version.to_lowercase().starts_with("dev-") { "dev".to_string() } else { MAJOR.replace(version, "$1").into_owned() };
        by_major.entry(major).or_default().push(pretty.as_str());
    }
    let mut out = vec![];
    for (major, list) in by_major {
        if list.len() > if major == "dev" { max_dev } else { max } {
            out.extend([list[0], "...", list[list.len() - 1]]);
        } else {
            out.extend(list);
        }
    }
    out
}

/// `hasMultipleNames()`.
fn has_multiple_names(packages: &[PackageRef]) -> bool {
    packages.iter().any(|p| p.name != packages[0].name)
}

/// `constraintToText()`: ` 1.0 (exact version match: 1.0, 1.0.0 or 1.0.0.0)` and the like.
fn constraint_to_text(constraint: Option<&PrettyConstraint>) -> String {
    let Some(c) = constraint else { return String::new() };
    if let Constraint::Single { op: Op::Eq, version } = &c.constraint
        && !version.starts_with("dev-")
    {
        static NUMERIC: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d+(?:\.\d+)*$").unwrap());
        if !NUMERIC.is_match(&c.pretty) {
            return format!(" {} (exact version match)", c.pretty);
        }
        let mut versions = vec![c.pretty.clone()];
        for _ in 0..3usize.saturating_sub(c.pretty.matches('.').count()) {
            let next = format!("{}.0", versions[versions.len() - 1]);
            versions.push(next);
        }
        let all = match &versions[..] {
            [only] => only.clone(),
            [init @ .., last] => format!("{} or {last}", init.join(", ")),
            [] => unreachable!(),
        };
        return format!(" {} (exact version match: {all})", c.pretty);
    }
    format!(" {}", c.pretty)
}

/// Path repository packages are symlinked unless `"symlink": false` is set.
pub(crate) fn is_symlinked_path(p: &Package) -> bool {
    p.dist.as_ref().is_some_and(|d| d.kind == "path") && p.transport_options.get("symlink") != Some(&Value::Bool(false))
}

/// `OutputFormatter::escape()`.
fn escape(text: &str) -> String {
    text.replace('<', "\\<").replace('>', "\\>")
}

/// `substr($s, 0, $max)` without splitting a character.
fn truncate(s: &str, max: usize) -> &str {
    let mut end = s.len().min(max);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_lists_sort_and_condense_versions() {
        let packages: Vec<Package> = ["1.10.0", "1.9.0", "1.2.0", "2.0.0", "1.3.0", "1.4.0"]
            .iter()
            .map(|v| Package::new("a/A", &komposer_semver::normalize(v).unwrap(), v))
            .collect();
        let refs: Vec<&Package> = packages.iter().collect();
        let pool = Pool::default();
        assert_eq!(package_list(&refs[..3], false, &pool, None, false), "a/A[1.2.0, 1.9.0, 1.10.0]");
        assert_eq!(package_list(&refs, false, &pool, None, false), "a/A[1.2.0, ..., 1.10.0, 2.0.0]");
        assert_eq!(package_list(&refs, true, &pool, None, false), "a/A[1.2.0, 1.3.0, 1.4.0, 1.9.0, 1.10.0, 2.0.0]");
    }

    #[test]
    fn exact_constraints_as_text() {
        let exact = |pretty: &str| PrettyConstraint::new(komposer_semver::parse_constraints(pretty).unwrap(), pretty);
        assert_eq!(constraint_to_text(Some(&exact("1.0"))), " 1.0 (exact version match: 1.0, 1.0.0 or 1.0.0.0)");
        assert_eq!(constraint_to_text(Some(&exact("1.0.0.0"))), " 1.0.0.0 (exact version match: 1.0.0.0)");
        assert_eq!(constraint_to_text(Some(&exact("v1.0"))), " v1.0 (exact version match)");
        assert_eq!(constraint_to_text(Some(&exact("^1.0"))), " ^1.0");
        assert_eq!(constraint_to_text(None), "");
        assert_eq!(php_compare("-12", "-3"), std::cmp::Ordering::Less);
        assert_eq!(php_compare("1-2", "1-10"), std::cmp::Ordering::Greater);
    }
}
