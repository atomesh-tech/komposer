//! `Advisory\Auditor`: security advisories, abandoned packages and filter lists (malware), plus
//! the Symfony `Table` the console output uses.

use crate::commands::install::package_name_regex;
use anyhow::{Result, bail};
use komposer_package::config::Config;
use komposer_package::io::Io;
use komposer_package::json::{self, Flags};
use komposer_package::repository::{FilterEntry, SecurityAdvisory};
use komposer_package::{Package, PackageRef};
use komposer_semver::{Constraint, Op};
use komposer_solver::RepositorySet;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;

/// `Auditor::FORMATS`.
pub const FORMATS: [&str; 4] = ["table", "plain", "json", "summary"];

/// `ListPolicyConfig::AUDIT_*`: what an audit does with a list's matches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuditMode {
    Ignore,
    Report,
    Fail,
}

impl AuditMode {
    pub fn parse(s: &str) -> Option<AuditMode> {
        match s {
            "ignore" => Some(AuditMode::Ignore),
            "report" => Some(AuditMode::Report),
            "fail" => Some(AuditMode::Fail),
            _ => None,
        }
    }
}

/// The audit side of `Policy\PolicyConfig`.
#[derive(Clone, Debug)]
pub struct AuditOptions {
    /// advisory ids, CVEs, remote ids and package names => ignore reason
    pub ignore: Vec<(String, Option<String>)>,
    /// severities => ignore reason
    pub ignore_severity: Vec<(String, Option<String>)>,
    pub abandoned: AuditMode,
    /// package name patterns
    pub abandoned_ignore: Vec<String>,
    pub malware: AuditMode,
    pub malware_ignore: Vec<(String, Constraint)>,
    pub ignore_unreachable: bool,
}

impl Default for AuditOptions {
    fn default() -> AuditOptions {
        AuditOptions {
            ignore: vec![],
            ignore_severity: vec![],
            abandoned: AuditMode::Fail,
            abandoned_ignore: vec![],
            malware: AuditMode::Fail,
            malware_ignore: vec![],
            ignore_unreachable: false,
        }
    }
}

impl AuditOptions {
    /// `PolicyConfig::fromConfig()` for the `audit` operation: `policy.*`, falling back to `audit.*`.
    pub fn from_config(config: &Config) -> Result<AuditOptions> {
        let policy = match config.get("policy") {
            Some(Value::Bool(false)) => {
                return Ok(AuditOptions {
                    abandoned: AuditMode::Ignore,
                    malware: AuditMode::Ignore,
                    ignore_unreachable: true,
                    ..AuditOptions::default()
                });
            }
            Some(Value::Object(p)) => p,
            _ => Map::new(),
        };
        let audit = config.get("audit").and_then(|a| a.as_object().cloned()).unwrap_or_default();
        let set = |m: &Map<String, Value>, k: &str| m.get(k).filter(|v| !v.is_null()).cloned();
        let mode = |v: Option<Value>| -> Result<AuditMode> {
            match v {
                None => Ok(AuditMode::Fail),
                Some(Value::String(s)) => AuditMode::parse(&s).ok_or_else(|| anyhow::anyhow!("Invalid audit mode {s}")),
                Some(v) => bail!("Invalid audit mode {v}"),
            }
        };
        let mut o = AuditOptions::default();

        match set(&policy, "advisories") {
            None if !audit.is_empty() => {
                for (id, reason, on_audit) in legacy_ignores(audit.get("ignore"))? {
                    if on_audit {
                        o.ignore.push((id, reason));
                    }
                }
                o.ignore_severity = legacy_ignores(audit.get("ignore-severity"))?.into_iter().filter(|e| e.2).map(|(s, r, _)| (s, r)).collect();
            }
            Some(Value::Bool(false)) => {}
            advisories => {
                let advisories = advisories.and_then(|a| a.as_object().cloned()).unwrap_or_default();
                o.ignore = rule_map(advisories.get("ignore-id"))?;
                for (name, reason) in package_rules(advisories.get("ignore"))?.into_iter().map(|(n, _, r)| (n, r)) {
                    merge_reason(&mut o.ignore, name, reason);
                }
                o.ignore_severity = rule_map(advisories.get("ignore-severity"))?;
            }
        }

        match set(&policy, "abandoned") {
            None if !audit.is_empty() => {
                o.abandoned = mode(set(&audit, "abandoned"))?;
                o.abandoned_ignore = legacy_ignores(audit.get("ignore-abandoned"))?.into_iter().filter(|e| e.2).map(|e| e.0).collect();
            }
            Some(Value::Bool(false)) => o.abandoned = AuditMode::Ignore,
            abandoned => {
                let abandoned = abandoned.and_then(|a| a.as_object().cloned()).unwrap_or_default();
                o.abandoned = mode(set(&abandoned, "audit"))?;
                o.abandoned_ignore = package_rules(abandoned.get("ignore"))?.into_iter().map(|r| r.0).collect();
            }
        }
        if let Ok(env) = std::env::var("COMPOSER_AUDIT_ABANDONED") {
            o.abandoned = AuditMode::parse(&env)
                .ok_or_else(|| anyhow::anyhow!("Invalid value for COMPOSER_AUDIT_ABANDONED: {env}. Expected one of ignore, report, fail."))?;
        }

        match set(&policy, "malware") {
            Some(Value::Bool(false)) => o.malware = AuditMode::Ignore,
            malware => {
                let malware = malware.and_then(|m| m.as_object().cloned()).unwrap_or_default();
                o.malware = mode(set(&malware, "audit"))?;
                o.malware_ignore = package_rules(malware.get("ignore"))?.into_iter().map(|(n, c, _)| (n, c)).collect();
            }
        }

        o.ignore_unreachable = match (set(&policy, "ignore-unreachable"), set(&audit, "ignore-unreachable")) {
            (Some(Value::Array(scopes)), _) => scopes.iter().any(|s| s == "audit"),
            (Some(v), _) => komposer_package::loader::php_truthy(&v),
            (None, Some(v)) => komposer_package::loader::php_truthy(&v),
            (None, None) => false,
        };
        Ok(o)
    }
}

/// `ListPolicyConfig::mergeReason()`.
fn merge_reason(map: &mut Vec<(String, Option<String>)>, key: String, reason: Option<String>) {
    let Some((_, existing)) = map.iter_mut().find(|(k, _)| *k == key) else {
        map.push((key, reason));
        return;
    };
    match (existing.as_mut(), reason) {
        (_, None) => {}
        (None, r) => *existing = r,
        (Some(e), Some(r)) => {
            if !e.split(';').any(|p| p.trim() == r) {
                *e = format!("{e}; {r}");
            }
        }
    }
}

/// `parseLegacyAuditIgnore()` / `parseLegacyIgnoreWithApply()`: (id, reason, applies to audits).
fn legacy_ignores(config: Option<&Value>) -> Result<Vec<(String, Option<String>, bool)>> {
    let mut out = vec![];
    for (key, value) in entries(config) {
        let (id, reason, on_audit) = match (key, value) {
            (None, v) => (v.as_str().map(String::from).unwrap_or_else(|| v.to_string()), None, true),
            (Some(k), Value::String(r)) => (k, Some(r.clone()), true),
            (Some(k), Value::Object(o)) => {
                let apply = o.get("apply").and_then(Value::as_str).unwrap_or("all");
                if !["audit", "block", "all"].contains(&apply) {
                    bail!("Invalid 'apply' value for '{k}': {apply}. Expected 'audit', 'block', or 'all'.");
                }
                (k, o.get("reason").and_then(Value::as_str).map(String::from), apply != "block")
            }
            (Some(k), _) => (k, None, true),
        };
        out.push((id, reason, on_audit));
    }
    Ok(out)
}

/// `IgnoreIdRule::parseIgnoreIdMap()` / `IgnoreSeverityRule::parseIgnoreSeverityMap()`, audit rules only.
fn rule_map(config: Option<&Value>) -> Result<Vec<(String, Option<String>)>> {
    let mut out = vec![];
    for (key, value) in entries(config) {
        match (key, value) {
            (None, Value::String(id)) => out.push((id.clone(), None)),
            (None, v) => bail!("Invalid ignore entry: expected a string, got {v}."),
            (Some(k), Value::Null) => out.push((k, None)),
            (Some(k), Value::String(r)) => out.push((k, Some(r.clone()))),
            (Some(k), Value::Object(o)) => {
                if o.get("on-audit").is_none_or(|v| v != &Value::Bool(false)) {
                    out.push((k, o.get("reason").and_then(Value::as_str).map(String::from)));
                }
            }
            (Some(k), v) => bail!("Invalid ignore entry for \"{k}\": value {v} is not a supported shape."),
        }
    }
    Ok(out)
}

/// `IgnorePackageRule::parseIgnoreMap()`, audit rules only: (name pattern, constraint, reason).
fn package_rules(config: Option<&Value>) -> Result<Vec<(String, Constraint, Option<String>)>> {
    let mut out = vec![];
    let rule = |name: &str, o: &Map<String, Value>| -> Result<Option<(String, Constraint, Option<String>)>> {
        if o.get("on-audit") == Some(&Value::Bool(false)) {
            return Ok(None);
        }
        let constraint = match o.get("constraint").and_then(Value::as_str) {
            Some(c) => komposer_semver::parse_constraints(c)?,
            None => Constraint::Any,
        };
        Ok(Some((name.to_string(), constraint, o.get("reason").and_then(Value::as_str).map(String::from))))
    };
    for (key, value) in entries(config) {
        match (key, value) {
            (Some(k), Value::Null) => out.push((k, Constraint::Any, None)),
            (Some(k), Value::String(r)) => out.push((k, Constraint::Any, Some(r.clone()))),
            (None, Value::String(name)) => out.push((name.clone(), Constraint::Any, None)),
            (Some(k), Value::Array(rules)) => {
                for r in rules {
                    let Some(r) = r.as_object() else { bail!("Invalid ignore rule for \"{k}\": expected an object.") };
                    out.extend(rule(&k, r)?);
                }
            }
            (Some(k), Value::Object(r)) => out.extend(rule(&k, r)?),
            (k, v) => bail!("Invalid ignore entry at key \"{}\": value {v} is not a supported shape.", k.unwrap_or_default()),
        }
    }
    Ok(out)
}

/// Entries of a PHP array: `None` keys for list items.
fn entries(config: Option<&Value>) -> Vec<(Option<String>, &Value)> {
    match config {
        Some(Value::Array(list)) => list.iter().map(|v| (None, v)).collect(),
        Some(Value::Object(map)) => map.iter().map(|(k, v)| (Some(k.clone()), v)).collect(),
        _ => vec![],
    }
}

/// Advisories by package; ignored ones carry their (optional) ignore reason.
type Found = BTreeMap<String, Vec<(SecurityAdvisory, Option<Option<String>>)>>;

/// `Auditor::audit()`: `STATUS_OK` (0) or `STATUS_FAILED` (1).
pub fn audit_with(io: &Io, set: &RepositorySet, o: &AuditOptions, packages: &[PackageRef], format: &str, warning_only: bool) -> Result<i32> {
    let summary = format == "summary";
    let mut result = set.matching_security_advisories(packages, summary, o.ignore_unreachable)?;
    let mut unreachable = std::mem::take(&mut result.unreachable_repos);
    // ignores can name CVEs and remote ids, which partial advisories lack
    let partial = result.advisories.values().flatten().any(|a| a.is_partial());
    if summary && partial && o.ignore.iter().any(|(id, _)| !id.starts_with("PKSA-")) {
        result = set.matching_security_advisories(packages, false, o.ignore_unreachable)?;
        unreachable.append(&mut result.unreachable_repos);
    }
    let (advisories, ignored) = process_advisories(result.advisories, o);

    let abandoned: Vec<&PackageRef> = if o.abandoned == AuditMode::Ignore {
        vec![]
    } else {
        let ignore = (!o.abandoned_ignore.is_empty()).then(|| o.abandoned_ignore.iter().map(|p| package_name_regex(p)).collect::<Vec<_>>());
        packages.iter().filter(|p| p.is_abandoned() && ignore.as_ref().is_none_or(|i| !i.iter().any(|r| r.is_match(&p.name)))).collect()
    };
    let abandoned_count = if o.abandoned == AuditMode::Fail { abandoned.len() } else { 0 };

    let mut filtered: BTreeMap<String, Vec<FilterEntry>> = BTreeMap::new();
    let mut filtered_count = 0;
    if o.malware != AuditMode::Ignore {
        let (entries, mut errors) = filter_entries(set, packages, o.ignore_unreachable)?;
        unreachable.append(&mut errors);
        for p in packages {
            for entry in matching_filter_entries(p, &entries, &o.malware_ignore) {
                filtered_count += usize::from(o.malware == AuditMode::Fail);
                filtered.entry(p.name.clone()).or_default().push(entry);
            }
        }
    }
    let status = i32::from(!advisories.is_empty() || abandoned_count > 0 || filtered_count > 0);

    if format == "json" {
        let mut out = Map::new();
        out.insert("advisories".into(), advisories_json(&advisories));
        if !ignored.is_empty() {
            out.insert("ignored-advisories".into(), advisories_json(&ignored));
        }
        if !unreachable.is_empty() {
            out.insert("unreachable-repositories".into(), json!(unreachable));
        }
        let abandoned_map: Map<String, Value> = abandoned.iter().map(|p| (p.pretty_name.clone(), p.replacement_package().into())).collect();
        out.insert("abandoned".into(), Value::Object(abandoned_map));
        let filter: Map<String, Value> = filtered
            .iter()
            .map(|(name, entries)| {
                let list = entries.iter().map(|e| {
                    json!({"packageName": e.package_name, "listName": e.list_name, "constraint": e.constraint.to_string(), "url": e.url, "reason": e.reason, "id": e.id, "source": null})
                });
                (name.clone(), Value::Array(list.collect()))
            })
            .collect();
        out.insert("filter".into(), Value::Object(filter));
        // empty maps are PHP arrays, encoded as []
        io.write(&json::encode(&json::php_normalize(Value::Object(out)), Flags::FILE));
        return Ok(status);
    }

    let style = if warning_only { "warning" } else { "error" };
    if !advisories.is_empty() || !ignored.is_empty() {
        for (list, kind, open) in [(&ignored, "ignored security vulnerability", "info"), (&advisories, "security vulnerability", style)] {
            let pkgs = list.len();
            let total: usize = list.values().map(Vec::len).sum();
            if pkgs == 0 {
                continue;
            }
            io.write(&format!(
                "<{open}>Found {total} {kind} advisor{} affecting {pkgs} package{}{}</{open}>",
                if total == 1 { "y" } else { "ies" },
                if pkgs == 1 { "" } else { "s" },
                if summary { "." } else { ":" }
            ));
            match format {
                "table" => output_table(io, list),
                "plain" => output_plain(io, list),
                _ => {}
            }
        }
        if summary {
            io.write("Run \"composer audit\" for a full list of advisories.");
        }
    } else {
        io.write("<info>No security vulnerability advisories found.</info>");
    }
    if !unreachable.is_empty() {
        io.write_error("<warning>The following repositories were unreachable:</warning>");
        for repo in &unreachable {
            io.write_error(&format!("  - {repo}"));
        }
    }
    if !abandoned.is_empty() && !summary {
        output_abandoned(io, &abandoned, format);
    }
    if !filtered.is_empty() {
        let style = if filtered_count > 0 { "error" } else { "warning" };
        io.write(&format!(
            "<{style}>Found {} package{} matching filters{}</{style}>",
            filtered.len(),
            if filtered.len() == 1 { "" } else { "s" },
            if summary { "." } else { ":" }
        ));
        if !summary {
            output_filtered(io, &filtered, format);
        }
    }
    Ok(status)
}

/// `processAdvisories()`: splits off the ignored advisories, with the reason they are ignored for.
fn process_advisories(all: BTreeMap<String, Vec<SecurityAdvisory>>, o: &AuditOptions) -> (Found, Found) {
    let (mut active, mut ignored) = (Found::new(), Found::new());
    let lookup = |list: &[(String, Option<String>)], key: &str| list.iter().find(|(k, _)| k == key).map(|(_, r)| r.clone());
    for (package, list) in all {
        for a in list {
            let mut reason: Option<Option<String>> = lookup(&o.ignore, &package);
            if let Some(r) = lookup(&o.ignore, &a.advisory_id) {
                reason = Some(r);
            }
            if !a.is_partial() {
                if let Some(severity) = &a.severity
                    && let Some(r) = lookup(&o.ignore_severity, severity)
                {
                    reason = Some(r.or_else(|| Some(format!("{severity} severity is ignored"))));
                }
                if let Some(r) = a.cve.as_ref().and_then(|cve| lookup(&o.ignore, cve)) {
                    reason = Some(r);
                }
                let remote = a.sources.iter().filter_map(|s| s.get("remoteId")?.as_str()).find_map(|id| lookup(&o.ignore, id));
                if let Some(r) = remote {
                    reason = Some(r);
                }
            }
            match reason {
                None => active.entry(package.clone()).or_default().push((a, None)),
                Some(r) => ignored.entry(package.clone()).or_default().push((a, Some(r))),
            }
        }
    }
    (active, ignored)
}

/// `FilterListProviderSet::getMatchingFilterLists()` for the `malware` list.
fn filter_entries(set: &RepositorySet, packages: &[PackageRef], ignore_unreachable: bool) -> Result<(Vec<FilterEntry>, Vec<String>)> {
    let mut by_name: Vec<(String, Vec<Constraint>)> = vec![];
    // root alias versions are not actual package versions
    for p in packages.iter().filter(|p| !(p.is_alias() && p.root_package_alias)) {
        let c = Constraint::single(Op::Eq, p.version.clone());
        match by_name.iter_mut().find(|(n, _)| *n == p.name) {
            Some((_, list)) if !list.contains(&c) => list.push(c),
            Some(_) => {}
            None => by_name.push((p.name.clone(), vec![c])),
        }
    }
    let constraints: Vec<(String, Constraint)> = by_name.into_iter().map(|(n, c)| (n, Constraint::multi(c, false))).collect();
    let (mut entries, mut unreachable) = (vec![], vec![]);
    for repo in set.repositories() {
        match repo.filter_entries(&constraints, &["malware".to_string()]) {
            Ok(found) => entries.extend(found),
            Err(e) if ignore_unreachable => unreachable.push(e.to_string()),
            Err(e) => return Err(e),
        }
    }
    Ok((entries, unreachable))
}

/// `FilterListAuditor::getMatchingAuditEntries()`.
fn matching_filter_entries(p: &Package, entries: &[FilterEntry], ignore: &[(String, Constraint)]) -> Vec<FilterEntry> {
    let version = Constraint::single(Op::Eq, p.version.clone());
    let mut out = vec![];
    for name in p.names(false) {
        if ignore.iter().any(|(pattern, c)| package_name_regex(pattern).is_match(&name) && c.matches(&version)) {
            continue;
        }
        out.extend(entries.iter().filter(|e| e.package_name == name && e.constraint.matches(&version)).cloned());
    }
    out
}

/// `PackageInfo::getViewSourceOrHomepageUrl()` linked to the pretty name.
fn name_with_link(p: &Package) -> String {
    let support = p.meta.get("support").and_then(|s| s.get("source")).and_then(Value::as_str).filter(|s| !s.is_empty());
    let source = support.or(p.source.as_ref().map(|s| s.url.as_str()));
    let url = source.or_else(|| p.meta.get("homepage").and_then(Value::as_str)).filter(|u| !u.is_empty());
    match url {
        Some(url) => format!("<href={}>{}</>", escape(url), p.pretty_name),
        None => p.pretty_name.clone(),
    }
}

fn severity(a: &SecurityAdvisory) -> String {
    a.severity.clone().unwrap_or_default()
}

fn advisory_id(a: &SecurityAdvisory) -> String {
    match a.advisory_id.starts_with("PKSA-") {
        true => format!("<href=https://packagist.org/security-advisories/{0}>{0}</>", a.advisory_id),
        false => a.advisory_id.clone(),
    }
}

fn cve(a: &SecurityAdvisory) -> String {
    match &a.cve {
        Some(cve) => format!("<href=https://www.cve.org/CVERecord?id={cve}>{cve}</>"),
        None => "NO CVE".into(),
    }
}

fn url(a: &SecurityAdvisory) -> String {
    a.link.as_ref().map(|l| format!("<href={0}>{0}</>", escape(l))).unwrap_or_default()
}

/// `OutputFormatter::escape()`.
fn escape(s: &str) -> String {
    s.replace('<', "\\<")
}

/// `ConsoleIO::sanitize()`: drops terminal escape sequences and control characters.
fn sanitize(s: &str) -> String {
    static RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"\x1B\[[\x30-\x3F]*[\x20-\x2F]*[\x40-\x7E]|\x1B\].*?(?:\x1B\\|\x07)|\x1B.|[\x01-\x09\x0B\x0C\x0E-\x1A]|\r[^\n]").unwrap()
    });
    RE.replace_all(s, "").into_owned()
}

fn output_table(io: &Io, list: &Found) {
    for (a, reason) in list.values().flatten() {
        let mut headers = vec!["Package", "Severity", "Advisory ID", "CVE", "Title", "URL", "Affected versions", "Reported at"];
        let mut row = vec![
            a.package_name.clone(),
            severity(a),
            advisory_id(a),
            cve(a),
            a.title.clone().unwrap_or_default(),
            url(a),
            a.affected_versions_pretty.clone(),
            a.reported_at.clone().unwrap_or_default(),
        ];
        if let Some(reason) = reason {
            headers.push("Ignore reason");
            row.push(reason.clone().unwrap_or_else(|| "None specified".into()));
        }
        let mut t = Table::new(headers.into_iter().map(String::from).collect());
        t.rows.push(row.iter().map(|c| sanitize(c)).collect());
        t.horizontal = true;
        t.width(1, 80);
        t.render(io);
    }
}

fn output_plain(io: &Io, list: &Found) {
    let mut lines = vec![];
    for (i, (a, reason)) in list.values().flatten().enumerate() {
        if i > 0 {
            lines.push("--------".to_string());
        }
        lines.push(format!("Package: {}", a.package_name));
        lines.push(format!("Severity: {}", severity(a)));
        lines.push(format!("Advisory ID: {}", advisory_id(a)));
        lines.push(format!("CVE: {}", cve(a)));
        lines.push(format!("Title: {}", escape(a.title.as_deref().unwrap_or(""))));
        lines.push(format!("URL: {}", url(a)));
        lines.push(format!("Affected versions: {}", escape(&a.affected_versions_pretty)));
        lines.push(format!("Reported at: {}", a.reported_at.as_deref().unwrap_or("")));
        if let Some(reason) = reason {
            lines.push(format!("Ignore reason: {}", reason.as_deref().unwrap_or("None specified")));
        }
    }
    io.write(&lines.join("\n"));
}

fn output_abandoned(io: &Io, packages: &[&PackageRef], format: &str) {
    io.write(&format!("<error>Found {} abandoned package{}:</error>", packages.len(), if packages.len() > 1 { "s" } else { "" }));
    if format == "plain" {
        for p in packages {
            let replacement = p.replacement_package().map_or("No replacement was suggested".into(), |r| format!("Use {r} instead"));
            io.write(&format!("{} is abandoned. {replacement}.", name_with_link(p)));
        }
        return;
    }
    let mut t = Table::new(vec!["Abandoned Package".into(), "Suggested Replacement".into()]);
    t.width(1, 80);
    for p in packages {
        t.rows.push(vec![sanitize(&name_with_link(p)), sanitize(p.replacement_package().unwrap_or("none"))]);
    }
    t.render(io);
}

fn output_filtered(io: &Io, filtered: &BTreeMap<String, Vec<FilterEntry>>, format: &str) {
    if format == "plain" {
        for e in filtered.values().flatten() {
            let mut parts = vec![format!("{} matched dependency policy \"{}\"", e.package_name, e.list_name)];
            parts.extend(e.reason.as_ref().map(|r| format!("Reason: {r}")));
            parts.extend(e.url.as_ref().map(|u| format!("URL: {u}")));
            io.write(&format!("{}.", parts.join(". ")));
        }
        return;
    }
    let mut t = Table::new(["Package", "Versions", "List", "URL", "Reason", "ID", "Source"].map(String::from).to_vec());
    t.max_widths.push((4, 40));
    for e in filtered.values().flatten() {
        let row = [
            &e.package_name,
            &e.constraint.to_string(),
            &e.list_name,
            e.url.as_deref().unwrap_or(""),
            e.reason.as_deref().unwrap_or(""),
            e.id.as_deref().unwrap_or(""),
            "",
        ];
        t.rows.push(row.iter().map(|c| sanitize(c)).collect());
    }
    t.render(io);
}

fn advisories_json(list: &Found) -> Value {
    let map: Map<String, Value> = list
        .iter()
        .map(|(name, advisories)| {
            let items = advisories.iter().map(|(a, reason)| {
                let mut v = json!({
                    "advisoryId": a.advisory_id,
                    "packageName": name,
                    "affectedVersions": a.affected_versions_pretty,
                    "title": a.title,
                    "cve": a.cve,
                    "link": a.link,
                    "reportedAt": a.reported_at,
                    "sources": a.sources,
                    "severity": a.severity,
                });
                if let Some(Some(r)) = reason {
                    v["ignoreReason"] = r.clone().into();
                }
                v
            });
            (name.clone(), Value::Array(items.collect()))
        })
        .collect();
    Value::Object(map)
}

/// Symfony Console's `Table` with the `default` and `compact` styles: column widths from the
/// visible (tag-stripped) text, `setColumnWidth`/`setColumnMaxWidth` with hard wrapping, and
/// `setHorizontal()`. Rows go to stdout.
#[derive(Default)]
pub struct Table {
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub horizontal: bool,
    pub compact: bool,
    pub widths: Vec<(usize, usize)>,
    pub max_widths: Vec<(usize, usize)>,
}

impl Table {
    pub fn new(headers: Vec<String>) -> Table {
        Table { headers, ..Table::default() }
    }

    /// `setColumnWidth()` and `setColumnMaxWidth()` with the same value.
    pub fn width(&mut self, column: usize, width: usize) {
        self.widths.push((column, width));
        self.max_widths.push((column, width));
    }

    pub fn render(&self, io: &Io) {
        for line in self.lines() {
            io.write(&line);
        }
    }

    /// The rendered lines, still holding the cells' style tags.
    fn lines(&self) -> Vec<String> {
        let (header_rows, body): (Vec<Vec<String>>, Vec<Vec<String>>) = if self.horizontal {
            let rows = self
                .headers
                .iter()
                .enumerate()
                .map(|(i, h)| std::iter::once(h.clone()).chain(self.rows.iter().map(|r| r.get(i).cloned().unwrap_or_default())).collect());
            (vec![], rows.collect())
        } else {
            ((!self.headers.is_empty()).then(|| self.headers.clone()).into_iter().collect(), self.rows.clone())
        };
        let header_rows = self.split_lines(header_rows);
        let body = self.split_lines(body);
        let columns = header_rows.iter().chain(&body).map(Vec::len).max().unwrap_or(0);
        let lookup = |list: &[(usize, usize)], c: usize| list.iter().find(|(col, _)| *col == c).map(|(_, w)| *w);
        let widths: Vec<usize> = (0..columns)
            .map(|c| {
                let longest = header_rows.iter().chain(&body).map(|r| r.get(c).map_or(0, |cell| visible_width(cell))).max().unwrap_or(0);
                let w = longest.max(lookup(&self.widths, c).unwrap_or(0));
                lookup(&self.max_widths, c).map_or(w, |m| w.min(m))
            })
            .collect();

        let row = |cells: &[String], header: bool| -> String {
            let mut out = String::from(if self.compact { "" } else { "|" });
            for (c, width) in widths.iter().enumerate() {
                let cell = cells.get(c).map_or("", String::as_str);
                let pad = " ".repeat(width.saturating_sub(visible_width(cell)));
                let styled = |s: String| if header || (self.horizontal && c == 0) { format!("<info>{s}</info>") } else { s };
                if self.compact {
                    out.push_str(&styled(format!("{cell} {pad}")));
                } else {
                    out.push_str(&styled(format!(" {cell}{pad} ")));
                    out.push('|');
                }
            }
            out
        };
        let separator = || -> Option<String> {
            (!self.compact && columns > 0)
                .then(|| std::iter::once("+".to_string()).chain(widths.iter().map(|w| format!("{}+", "-".repeat(w + 2)))).collect())
        };

        let mut lines = vec![];
        lines.extend(separator());
        for cells in &header_rows {
            lines.push(row(cells, true));
        }
        if !header_rows.is_empty() && !body.is_empty() {
            lines.extend(separator());
        }
        for cells in &body {
            lines.push(row(cells, false));
        }
        lines.extend(separator().filter(|_| !header_rows.is_empty() || !body.is_empty()));
        lines
    }

    /// `buildTableRows()`: wraps cells over their column's max width and spreads multi-line cells
    /// over extra rows.
    fn split_lines(&self, rows: Vec<Vec<String>>) -> Vec<Vec<String>> {
        let mut out = vec![];
        for row in rows {
            let cells: Vec<Vec<String>> = row
                .iter()
                .enumerate()
                .map(|(c, cell)| {
                    let max = self.max_widths.iter().find(|(col, _)| *col == c).map(|(_, w)| *w);
                    let cell = match max {
                        Some(max) if visible_width(cell) > max => wrap(&strip_tags(cell), max),
                        _ => cell.clone(),
                    };
                    cell.split('\n').map(String::from).collect()
                })
                .collect();
            let height = cells.iter().map(Vec::len).max().unwrap_or(1);
            for line in 0..height {
                out.push(cells.iter().map(|lines| lines.get(line).cloned().unwrap_or_default()).collect());
            }
        }
        out
    }
}

/// The text a cell shows once its style tags are rendered.
fn strip_tags(s: &str) -> String {
    Io::null().format(s)
}

fn visible_width(s: &str) -> usize {
    strip_tags(s).chars().count()
}

/// `OutputFormatter::formatAndWrap()` on plain text: PHP's `wordwrap($text, $width, "\n", true)`
/// over characters, as symfony/string does it.
fn wrap(text: &str, width: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let (mut last_start, mut last_space) = (0, 0);
    let push = |out: &mut String, from: usize, to: usize| out.extend(&chars[from..to]);
    let mut current = 0;
    while current < chars.len() {
        if chars[current] == '\n' && current + 1 < chars.len() {
            push(&mut out, last_start, current + 1);
            last_start = current + 1;
            last_space = current + 1;
        } else if chars[current] == ' ' {
            if current - last_start >= width {
                push(&mut out, last_start, current);
                out.push('\n');
                last_start = current + 1;
            }
            last_space = current;
        } else if current - last_start >= width && last_start >= last_space {
            push(&mut out, last_start, current);
            out.push('\n');
            last_start = current;
            last_space = current;
        } else if current - last_start >= width && last_start < last_space {
            push(&mut out, last_start, last_space);
            out.push('\n');
            last_start = last_space + 1;
            last_space = last_start;
        }
        current += 1;
    }
    push(&mut out, last_start.min(chars.len()), chars.len());
    let trailing = if text.ends_with('\n') { "\n" } else { "" };
    format!("{}{trailing}", out.trim_end_matches('\n'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_table_pads_like_symfony() {
        let rows = vec![
            vec!["php".into(), "8.5.1".into(), "".into(), "<info>success</info>".into()],
            vec!["ext-json".into(), "*".into(), "".into(), "failed".into()],
        ];
        let t = Table { compact: true, rows, ..Table::default() };
        let lines: Vec<String> = t.lines().iter().map(|l| strip_tags(l)).collect();
        assert_eq!(lines, vec!["php      8.5.1  success ", "ext-json *      failed  "]);
    }

    #[test]
    fn horizontal_table_wraps_long_cells() {
        let mut t = Table::new(vec!["Package".into(), "Title".into()]);
        t.rows.push(vec!["a/b".into(), format!("{} tail", "x".repeat(80))]);
        t.horizontal = true;
        t.width(1, 80);
        let lines: Vec<String> = t.lines().iter().map(|l| strip_tags(l)).collect();
        let border = format!("+---------+{}+", "-".repeat(82));
        assert_eq!(
            lines,
            vec![
                border.clone(),
                format!("| Package | a/b{} |", " ".repeat(77)),
                format!("| Title   | {} |", "x".repeat(80)),
                format!("|         | tail{} |", " ".repeat(76)),
                border,
            ]
        );
    }

    #[test]
    fn audit_config_with_legacy_fallback() {
        let mut config = Config::new(false, None);
        config.merge_config(
            &json!({"audit": {"ignore": {"CVE-1": "not used", "PKSA-2": {"apply": "block"}}, "abandoned": "report", "ignore-severity": ["low"]}}),
        );
        let o = AuditOptions::from_config(&config).unwrap();
        assert_eq!(o.ignore, vec![("CVE-1".to_string(), Some("not used".to_string()))]);
        assert_eq!(o.ignore_severity, vec![("low".to_string(), None)]);
        assert_eq!(o.abandoned, AuditMode::Report);

        config
            .merge_config(&json!({"policy": {"advisories": {"ignore-id": ["PKSA-3"], "ignore": {"a/b": "why"}}, "abandoned": {"audit": "ignore"}}}));
        let o = AuditOptions::from_config(&config).unwrap();
        assert_eq!(o.ignore, vec![("PKSA-3".to_string(), None), ("a/b".to_string(), Some("why".to_string()))]);
        assert_eq!(o.abandoned, AuditMode::Ignore);
    }
}
