//! `check-platform-reqs`: `CheckPlatformReqsCommand`.

use crate::audit::Table;
use crate::cli::CheckPlatformReqsArgs;
use crate::composer::Composer;
use anyhow::Result;
use komposer_package::io::Io;
use komposer_package::json::{self, Flags};
use komposer_package::repository::Repository;
use komposer_package::repository::find_with_replacers_and_providers;
use komposer_package::{Link, PackageRef, is_platform_package};
use komposer_repository::platform::{PlatformRepository, detected_runtime, php_detected};
use komposer_semver::{Constraint, Op};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;

/// One result row: name, version, the failed link, status and provider (with style tags).
struct Row {
    name: String,
    version: String,
    link: Option<Link>,
    status: &'static str,
    provider: String,
}

/// `CheckPlatformReqsCommand::execute()`.
pub fn check_platform_reqs(io: &Arc<Io>, args: &CheckPlatformReqsArgs, run_scripts: bool) -> Result<i32> {
    let _ = run_scripts;
    let c = Composer::load(io.clone())?;
    let non_dev = if args.no_dev { "non-dev " } else { "" };
    let mut remove: Vec<String> = vec![];
    let mut packages: Vec<PackageRef> = if args.lock {
        io.write_error(&format!("<info>Checking {non_dev}platform requirements using the lock file</info>"));
        c.locker.locked_repository(!args.no_dev)?.packages()?
    } else {
        let installed = c.installed()?;
        let packages = installed.packages()?;
        if packages.is_empty() {
            io.write_error(&format!("<warning>No vendor dir present, checking {non_dev}platform requirements from the lock file</warning>"));
            c.locker.locked_repository(!args.no_dev)?.packages()?
        } else {
            if args.no_dev {
                remove = installed.dev_package_names().to_vec();
            }
            io.write_error(&format!("<info>Checking {non_dev}platform requirements for packages in the vendor dir</info>"));
            packages
        }
    };
    packages.push(Arc::new(c.root.package.clone()));

    let mut requires: BTreeMap<String, Vec<Link>> = BTreeMap::new();
    if !args.no_dev {
        for link in &c.root.package.dev_requires {
            requires.insert(link.target.clone(), vec![link.clone()]);
        }
    }
    for p in packages.iter().filter(|p| !remove.contains(&p.name)) {
        for link in &p.requires {
            requires.entry(link.target.clone()).or_default().push(link.clone());
        }
    }

    // the real platform: config.platform is ignored here
    packages.extend(PlatformRepository::new(&json!([]), detected_runtime())?.packages()?);
    let (rows, code) = check(&requires, &packages);

    if args.format == "json" {
        let rows: Vec<Value> = rows
            .iter()
            .map(|r| {
                let link = r
                    .link
                    .as_ref()
                    .map(|l| json!({"source": l.source, "type": l.kind.description(), "target": l.target, "constraint": l.pretty_constraint}));
                let provider = (!r.provider.is_empty()).then(|| Io::null().format(&r.provider));
                json!({"name": r.name, "version": r.version, "status": r.status, "failed_requirement": link, "provider": provider})
            })
            .collect();
        io.write(&json::encode(&Value::Array(rows), Flags::FILE));
    } else {
        let mut table = Table { compact: true, ..Table::default() };
        for r in &rows {
            let link = r.link.as_ref().map(ToString::to_string).unwrap_or_default();
            let described =
                r.link.as_ref().map(|l| format!("{} {} {} ({})", l.source, l.kind.description(), l.target, l.pretty_constraint)).unwrap_or_default();
            let tag = if r.status == "success" { "info" } else { "error" };
            let status = format!("<{tag}>{}</{tag}> {}", r.status, r.provider);
            table.rows.push(vec![r.name.clone(), r.version.clone(), link, described, status.trim_end().to_string()]);
        }
        table.render(io);
    }
    if !php_detected() {
        io.warning("PHP was not found on this machine, so php and its extensions are reported as missing. Install PHP to check them.");
    }
    Ok(code)
}

/// Checks every platform requirement against the candidates providing it.
fn check(requires: &BTreeMap<String, Vec<Link>>, packages: &[PackageRef]) -> (Vec<Row>, i32) {
    let mut rows = vec![];
    let mut code = 0;
    'requirement: for (require, links) in requires.iter().filter(|(r, _)| is_platform_package(r)) {
        let candidates = find_with_replacers_and_providers(packages, require, None);
        if candidates.is_empty() {
            rows.push(Row { name: require.clone(), version: "n/a".into(), link: Some(links[0].clone()), status: "missing", provider: String::new() });
            code = code.max(2);
            continue;
        }
        let mut failures = vec![];
        for candidate in candidates {
            let own = candidate.name == *require;
            let (constraint, pretty) = if own {
                (Constraint::single(Op::Eq, candidate.version.clone()), candidate.pretty_version.clone())
            } else {
                let Some(l) = candidate.provides.iter().chain(&candidate.replaces).find(|l| l.target == *require) else { continue };
                (l.constraint.clone(), l.pretty_constraint.clone())
            };
            let name = if own { candidate.pretty_name.clone() } else { require.clone() };
            let provider = if own { String::new() } else { format!("<comment>provided by {}</comment>", candidate.pretty_name) };
            match links.iter().find(|l| !l.constraint.matches(&constraint)) {
                // the first failure of every candidate is reported if none matches
                Some(failed) => failures.push(Row { name, version: pretty, link: Some(failed.clone()), status: "failed", provider }),
                None => {
                    rows.push(Row { name, version: pretty, link: None, status: "success", provider });
                    continue 'requirement;
                }
            }
        }
        rows.extend(failures);
        code = code.max(1);
    }
    (rows, code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use komposer_package::loader::{LoadOptions, load_package};

    fn package(data: Value) -> PackageRef {
        Arc::new(load_package(data.as_object().unwrap(), LoadOptions::default()).unwrap())
    }

    #[test]
    fn statuses_and_exit_codes() {
        // CheckPlatformReqsCommandTest: providers, failed and missing requirements
        let root = package(json!({"name": "root/pkg", "version": "1.0.0", "require": {"ext-foobar": "^2.3", "php": "^8.1", "ext-mbstring": "*"}}));
        let polyfill = package(json!({"name": "symfony/polyfill-mbstring", "version": "1.0.0", "provide": {"ext-mbstring": "*"}}));
        let php = package(json!({"name": "php", "version": "8.0.1"}));
        let packages = vec![polyfill, root.clone(), php];
        let mut requires: BTreeMap<String, Vec<Link>> = BTreeMap::new();
        for l in &root.requires {
            requires.entry(l.target.clone()).or_default().push(l.clone());
        }
        let (rows, code) = check(&requires, &packages);
        let summary: Vec<(String, String, &str, String)> =
            rows.iter().map(|r| (r.name.clone(), r.version.clone(), r.status, Io::null().format(&r.provider))).collect();
        assert_eq!(
            summary,
            vec![
                ("ext-foobar".into(), "n/a".into(), "missing", String::new()),
                ("ext-mbstring".into(), "*".into(), "success", "provided by symfony/polyfill-mbstring".into()),
                ("php".into(), "8.0.1".into(), "failed", String::new()),
            ]
        );
        assert_eq!(rows[2].link.as_ref().unwrap().to_string(), "root/pkg requires php ([>= 8.1.0.0-dev < 9.0.0.0-dev])");
        assert_eq!(code, 2);
    }
}
