//! `PackageRepository`: packages defined inline in composer.json, with optional
//! `security-advisories` and `filter` lists.

use crate::composer::create_advisory;
use anyhow::{Result, anyhow};
use komposer_package::PackageRef;
use komposer_package::json::{self, Flags};
use komposer_package::loader::{self, LoadOptions};
use komposer_package::repository::{AdvisoryResult, ArrayRepository, FilterEntry, LoadRequest, LoadResult, RepoKind, Repository};
use komposer_semver::Constraint;
use serde_json::{Map, Value};

pub struct PackageRepository {
    repo: ArrayRepository,
    advisories: Map<String, Value>,
    filter: Map<String, Value>,
}

impl PackageRepository {
    pub fn new(config: &Value) -> Result<PackageRepository> {
        // a single package definition or a list of them
        let definitions = match config.get("package") {
            Some(Value::Array(list)) => list.clone(),
            Some(single) => vec![single.clone()],
            None => vec![],
        };
        let mut repo = ArrayRepository::new("", RepoKind::Remote);
        for definition in &definitions {
            let loaded = definition
                .as_object()
                .ok_or_else(|| anyhow!("package definition is not an object"))
                .and_then(|d| loader::load(d, LoadOptions { transport_options: true, skip_self_links: false }));
            let packages = loaded.map_err(|e| {
                anyhow!(
                    "A repository of type \"package\" contains an invalid package definition: {e}\n\nInvalid package definition:\n{}",
                    json::encode(definition, Flags::PLAIN)
                )
            })?;
            for p in packages {
                repo.add(p);
            }
        }
        let map = |k: &str| config.get(k).and_then(Value::as_object).cloned().unwrap_or_default();
        Ok(PackageRepository { repo, advisories: map("security-advisories"), filter: map("filter") })
    }

    /// `getFilterLists()`.
    pub fn filter_lists(&self) -> Vec<String> {
        self.filter.keys().cloned().collect()
    }
}

impl Repository for PackageRepository {
    fn repo_name(&self) -> String {
        let n = self.repo.len();
        format!("package repo (defining {n} package{})", if n > 1 { "s" } else { "" })
    }

    fn load_packages(&self, req: &LoadRequest) -> Result<LoadResult> {
        self.repo.load_packages(req)
    }

    fn packages(&self) -> Result<Vec<PackageRef>> {
        self.repo.packages()
    }

    fn has_security_advisories(&self) -> bool {
        !self.advisories.is_empty()
    }

    fn security_advisories(&self, constraints: &[(String, Constraint)], allow_partial: bool) -> Result<AdvisoryResult> {
        let mut result = AdvisoryResult::default();
        for (name, list) in &self.advisories {
            let Some((_, wanted)) = constraints.iter().find(|(n, _)| n == name) else { continue };
            result.names_found.push(name.clone());
            let mut found = vec![];
            for data in list.as_array().into_iter().flatten() {
                let advisory = create_advisory(name, data);
                if !allow_partial && advisory.is_partial() {
                    return Err(anyhow!(
                        "Advisory for {name} could not be loaded as a full advisory from {}\n{}",
                        self.repo_name(),
                        json::encode(data, Flags::PLAIN)
                    ));
                }
                if advisory.affected_versions.matches(wanted) {
                    found.push(advisory);
                }
            }
            if !found.is_empty() {
                result.advisories.insert(name.clone(), found);
            }
        }
        Ok(result)
    }

    fn filter_entries(&self, constraints: &[(String, Constraint)], lists: &[String]) -> Result<Vec<FilterEntry>> {
        let mut out = vec![];
        for (list, entries) in self.filter.iter().filter(|(l, _)| lists.contains(l)) {
            for data in entries.as_array().into_iter().flatten() {
                let s = |k: &str| data.get(k).and_then(Value::as_str).map(String::from);
                let Some(package) = s("package") else { continue };
                let constraint = komposer_semver::parse_constraints(&s("constraint").unwrap_or_default()).map_err(|e| anyhow!("{e}"))?;
                if constraints.iter().any(|(n, c)| *n == package && constraint.matches(c)) {
                    out.push(FilterEntry {
                        package_name: package,
                        list_name: list.clone(),
                        constraint,
                        url: s("url"),
                        reason: s("reason"),
                        id: s("id"),
                        source: s("source"),
                    });
                }
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn single_and_multiple_definitions() {
        let repo = PackageRepository::new(&json!({"type": "package", "package": {"name": "a/a", "version": "1.0.0"}})).unwrap();
        assert_eq!(repo.repo_name(), "package repo (defining 1 package)");
        let repo = PackageRepository::new(&json!({"type": "package", "package": [
            {"name": "a/a", "version": "1.0.0", "dist": {"type": "zip", "url": "https://example.org/a.zip"}},
            {"name": "a/a", "version": "dev-main", "extra": {"branch-alias": {"dev-main": "2.x-dev"}}},
        ]}))
        .unwrap();
        assert_eq!(repo.packages().unwrap().len(), 3);
        let err = PackageRepository::new(&json!({"package": {"name": "a/a"}})).err().unwrap();
        assert!(
            err.to_string()
                .starts_with("A repository of type \"package\" contains an invalid package definition: Package a/a has no version defined.")
        );
    }

    #[test]
    fn advisories_and_filters() {
        let repo = PackageRepository::new(&json!({
            "package": {"name": "a/a", "version": "1.0.0"},
            "security-advisories": {"a/a": [{"advisoryId": "PKSA-1", "affectedVersions": ">=1.0,<1.1", "title": "Bad", "sources": [], "reportedAt": "2024-01-01 00:00:00"}]},
            "filter": {"malware": [{"package": "a/a", "constraint": "*", "reason": "evil"}]},
        }))
        .unwrap();
        assert!(repo.has_security_advisories());
        let wanted = vec![("a/a".to_string(), Constraint::single(komposer_semver::Op::Eq, "1.0.0.0"))];
        let result = repo.security_advisories(&wanted, false).unwrap();
        assert_eq!(result.advisories["a/a"][0].advisory_id, "PKSA-1");
        assert_eq!(repo.filter_lists(), ["malware"]);
        assert_eq!(repo.filter_entries(&wanted, &["malware".into()]).unwrap()[0].reason.as_deref(), Some("evil"));
        assert!(repo.filter_entries(&wanted, &["other".into()]).unwrap().is_empty());
    }
}
