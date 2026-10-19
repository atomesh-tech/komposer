//! `bump`: `BumpCommand` and `Package\Version\VersionBumper`.

use crate::cli::BumpArgs;
use crate::commands::install::package_name_regex;
use crate::commands::require::edit_json;
use crate::composer::{Composer, composer_file};
use anyhow::Result;
use komposer_package::io::Io;
use komposer_package::repository::Repository;
use komposer_package::{Link, Package, dumper, is_platform_package};
use komposer_semver::{Constraint, DEFAULT_BRANCH_ALIAS, get_intervals, is_subset_of};
use regex::Regex;
use serde_json::{Map, Value};
use std::path::Path;
use std::sync::Arc;

pub fn bump(io: &Arc<Io>, args: &BumpArgs, run_scripts: bool) -> Result<i32> {
    let _ = run_scripts;
    if let Some(code) = check_file(io, &composer_file()?) {
        return Ok(code);
    }
    let mut c = Composer::load(io.clone())?;
    do_bump(&mut c, args.dev_only, args.no_dev_only, args.dry_run, &args.packages, "--dev-only")
}

/// The readable/writable checks of `doBump()`.
fn check_file(io: &Io, file: &Path) -> Option<i32> {
    let name = file.display();
    if std::fs::read(file).is_err() {
        io.write_error(&format!("<error>{name} is not readable.</error>"));
        return Some(1);
    }
    if std::fs::OpenOptions::new().append(true).open(file).is_err() {
        io.write_error(&format!("<error>{name} is not writable.</error>"));
        return Some(1);
    }
    None
}

/// `BumpCommand::doBump()`, also what `update --bump-after-update` runs (with
/// `--bump-after-update=dev` as the hint). Returns 1 for a dry run that would change something.
pub fn do_bump(c: &mut Composer, dev_only: bool, no_dev_only: bool, dry_run: bool, filter: &[String], dev_only_hint: &str) -> Result<i32> {
    let io = c.io.clone();
    let file = composer_file()?;
    if let Some(code) = check_file(&io, &file) {
        return Ok(code);
    }
    let name = file.display().to_string();
    // Composer's `$hasLockfileDisabled` is true when the lock is enabled; with `lock: false` it
    // reads the locker anyway, which points at /dev/null and fails
    let repo: Arc<dyn Repository> = if !c.config.get_bool("lock") {
        Arc::new(c.locker.locked_repository(true)?)
    } else if c.locker.is_locked()? {
        if !c.locker.is_fresh()? {
            io.error("The lock file is not up to date with the latest changes in composer.json. Run the appropriate `update` to fix that before you use the `bump` command.");
            return Ok(2);
        }
        Arc::new(c.locker.locked_repository(true)?)
    } else {
        Arc::new(c.installed()?)
    };

    if c.root.package.package_type != "project" && !dev_only {
        io.warning("Warning: Bumping dependency constraints is not recommended for libraries as it will narrow down your dependencies and may cause problems for your users.");
        if !c.root.raw.contains_key("type") {
            io.warning("If your package is not a library, you can explicitly specify the \"type\" by using \"composer config type project\".");
            io.warning(&format!("Alternatively you can use {dev_only_hint} to only bump dependencies within \"require-dev\"."));
        }
    }

    let mut tasks: Vec<(&str, &[Link])> = vec![];
    if !dev_only {
        tasks.push(("require", &c.root.package.requires));
    }
    if !no_dev_only {
        tasks.push(("require-dev", &c.root.package.dev_requires));
    }
    // proxied update arguments may carry constraints
    let strip = Regex::new(r"[:= ].+").unwrap();
    let patterns: Vec<Regex> = filter.iter().map(|f| package_name_regex(&strip.replace(f, "").to_lowercase())).collect();

    let mut updates: Vec<(&str, String, String)> = vec![];
    for (key, links) in tasks {
        for link in links {
            if (!patterns.is_empty() && !patterns.iter().any(|p| p.is_match(&link.target))) || is_platform_package(&link.target) {
                continue;
            }
            let Some(package) = repo.find_package(&link.target, &Constraint::Any)? else { continue };
            let bumped = bump_requirement(&link.constraint, &link.pretty_constraint, package.base());
            if bumped != link.pretty_constraint {
                updates.push((key, link.target.clone(), bumped));
            }
        }
    }

    if !dry_run {
        let edit =
            |m: &mut crate::json_manipulator::JsonManipulator| updates.iter().all(|(key, package, version)| m.add_link(key, package, version, false));
        edit_json(&file, edit, |data: &mut Map<String, Value>| {
            for (key, package, version) in &updates {
                if let Value::Object(links) = data.entry(*key).or_insert_with(|| Value::Object(Map::new())) {
                    links.insert(package.clone(), version.clone().into());
                }
            }
        })?;
    }

    if updates.is_empty() {
        io.write(&format!("<info>No requirements to update in {name}.</info>"));
        return Ok(0);
    }
    if dry_run {
        io.write(&format!("<info>{name} would be updated with:</info>"));
        for (key, package, version) in &updates {
            io.write(&format!("<info> - {key}.{package}: {version}</info>"));
        }
        return Ok(1);
    }
    io.write(&format!("<info>{name} has been updated ({} changes).</info>", updates.len()));
    if c.config.get_bool("lock") && c.locker.is_locked()? {
        c.locker.update_hash(&std::fs::read_to_string(&file)?)?;
    }
    Ok(0)
}

/// `VersionBumper::bumpRequirement()`: raises the lower bound of `constraint` to the package's
/// version (`^1.0` + 1.2.1 → `^1.2.1`), keeping the constraint's shape.
pub fn bump_requirement(constraint: &Constraint, pretty: &str, package: &Package) -> String {
    if pretty.starts_with("dev-") {
        return pretty.into();
    }
    let mut version = package.version.clone();
    if version.starts_with("dev-") {
        // dev packages without branch alias cannot be processed
        match komposer_package::loader::branch_alias(&dumper::dump(package)) {
            Ok(Some(alias)) if alias != DEFAULT_BRANCH_ALIAS => version = alias,
            _ => return pretty.into(),
        }
    }
    // complex constraints with branch names are not bumped
    if !get_intervals(constraint).branches.names.is_empty() {
        return pretty.into();
    }
    let major = Regex::new(r"^([1-9][0-9]*|0\.\d+).*").unwrap().replace(&version, "$1").into_owned();
    let without_suffix = Regex::new(r"(?:\.(?:0|9999999))+(-dev)?$").unwrap().replace(&version, "").into_owned();
    let new_pretty = format!("^{without_suffix}");
    // not a simple stable version, abort
    if !Regex::new(r"^\^\d+(\.\d+)*$").unwrap().is_match(&new_pretty) {
        return pretty.into();
    }

    // `$major` goes into the pattern unescaped, like Composer does; the separators before and
    // after a constraint are lookarounds there and checked by hand here
    let pattern =
        Regex::new(&format!(r"^(?P<c>\^v?{major}(?:\.\d+)*|~v?{major}(?:\.\d+){{1,3}}|v?{major}(?:\.[*x])+|>=v?\d(?:\.\d+)*|\*)(?:[, |@]|$)"))
            .unwrap();
    let mut matches = vec![];
    let mut pos = 0;
    while pos < pretty.len() {
        let after_separator = pos == 0 || matches!(pretty.as_bytes()[pos - 1], b',' | b' ' | b'|');
        match pattern.captures(&pretty[pos..]).filter(|_| after_separator && pretty.is_char_boundary(pos)) {
            Some(c) => {
                let m = c.name("c").unwrap().as_str();
                matches.push((pos, m.to_string()));
                pos += m.len();
            }
            None => pos += 1,
        }
    }
    if matches.is_empty() {
        return pretty.into();
    }
    let dots = |s: &str| s.matches('.').count();
    let mut modified = pretty.to_string();
    for (offset, m) in matches.iter().rev() {
        let suffix = if dots(m) == 2 && dots(&without_suffix) == 1 { ".0" } else { "" };
        let replacement = if m.starts_with('~') && dots(m) != 1 {
            // as many version bits as the constraint has, so it does not get more specific
            let mut bits: Vec<&str> = without_suffix.split('.').collect();
            bits.resize(bits.len().max(dots(m) + 1), "0");
            format!("~{}", bits[..dots(m) + 1].join("."))
        } else if m == "*" || m.starts_with(">=") {
            format!(">={without_suffix}{suffix}")
        } else {
            format!("{new_pretty}{suffix}")
        };
        modified.replace_range(*offset..offset + m.len(), &replacement);
    }
    // if it is strictly equal to the previous one then no need to change anything
    match komposer_semver::parse_constraints(&modified) {
        Ok(new) if is_subset_of(&new, constraint) && is_subset_of(constraint, &new) => pretty.into(),
        _ => modified,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bump_requirement_cases() {
        // VersionBumperTest::provideBumpRequirementTests
        let cases: &[(&str, &str, &str, Option<&str>)] = &[
            ("^1.0", "1.2.1", "^1.2.1", None),
            ("^v1.0", "1.2.1", "^1.2.1", None),
            ("^1.0", "1.0.0", "^1.0", None),
            ("^1.2", "1.2.0", "^1.2", None),
            ("^1.0.0", "1.2.0", "^1.2.0", None),
            ("^1.0.0", "1.2.1", "^1.2.1", None),
            ("^1.2 || ^2.3", "1.3.2", "^1.3.2 || ^2.3", None),
            ("^1.2 || ^2.3", "2.4.0", "^1.2 || ^2.4", None),
            ("^1.2 || ^2.3 || ^2", "2.4.0", "^1.2 || ^2.4 || ^2.4", None),
            ("^1.2 || ^2.3.3 || ^2", "2.4.0", "^1.2 || ^2.4.0 || ^2.4", None),
            ("^3@dev", "3.2.x-dev", "^3.2@dev", None),
            ("~2", "2.1-beta.1", "~2", None),
            ("dev-main", "dev-foo", "dev-main", None),
            ("^3.2", "dev-main", "^3.2", None),
            ("^3.2", "dev-main", "^3.3", Some("3.3.x-dev")),
            ("2.*", "2.4.0", "^2.4", None),
            ("v2.*", "2.4.0", "^2.4", None),
            ("2.x", "2.4.0", "^2.4", None),
            ("2.x.x", "2.4.0", "^2.4.0", None),
            ("2.4.*", "2.4.3", "2.4.*", None),
            ("2.4.3.*", "2.4.3.2", "2.4.3.*", None),
            ("~2", "2.4.3", "~2", None),
            ("~2.2", "2.4.3", "^2.4.3", None),
            ("~2.2.3", "2.2.6.2", "~2.2.6", None),
            ("~2.2.3", "2.2.6", "~2.2.6", None),
            ("~2.0.0", "2.0.0", "~2.0.0", None),
            ("~2025.1.561", "2025.1.583", "~2025.1.583", None),
            ("~2.2.3.1", "2.2.4", "~2.2.4.0", None),
            ("~2.2.3.1", "2.2.4.0", "~2.2.4.0", None),
            ("~2.2.3.1", "2.2.4.5", "~2.2.4.5", None),
            (">=3.0", "3.4.5", ">=3.4.5", None),
            (">=v3.0", "3.4.5", ">=3.4.5", None),
            (">2.2.3", "2.2.6", ">2.2.3", None),
            ("^0.3 || ^0.4", "0.4.3", "^0.3 || ^0.4.3", None),
            ("*", "1.2.3", ">=1.2.3", None),
        ];
        for (requirement, pretty_version, expected, branch_alias) in cases {
            let mut package = Package::new("foo/bar", &komposer_semver::normalize(pretty_version).unwrap(), pretty_version);
            if let Some(alias) = branch_alias {
                package.extra = serde_json::json!({"branch-alias": {*pretty_version: alias}});
            }
            let constraint = komposer_semver::parse_constraints(requirement).unwrap();
            assert_eq!(bump_requirement(&constraint, requirement, &package), *expected, "{requirement} + {pretty_version}");
        }
    }
}
