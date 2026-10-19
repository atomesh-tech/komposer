//! `RepositoryFactory` / `RepositoryManager::createRepository()`: repository definitions to
//! repositories, plus `FilterRepository` for `only`/`exclude`/`canonical`.

use crate::artifact::ArtifactRepository;
use crate::composer::ComposerRepository;
use crate::http::HttpClient;
use crate::package_repository::PackageRepository;
use crate::path::PathRepository;
use crate::url::sanitize;
use crate::util::{package_names_regex, php_type};
use crate::vcs::VcsRepository;
use anyhow::{Result, anyhow, bail};
use komposer_package::PackageRef;
use komposer_package::json::{self, Flags};
use komposer_package::repository::{AdvisoryResult, FilterEntry, LoadRequest, LoadResult, RepoKind, Repository, SearchMode};
use komposer_semver::Constraint;
use regex::Regex;
use serde_json::{Value, json};
use std::sync::{Arc, LazyLock};

/// The types `RepositoryFactory::manager()` registers.
pub const REPOSITORY_TYPES: [&str; 15] = [
    "composer",
    "vcs",
    "package",
    "pear",
    "git",
    "bitbucket",
    "git-bitbucket",
    "github",
    "gitlab",
    "svn",
    "fossil",
    "perforce",
    "hg",
    "artifact",
    "path",
];

/// `RepositoryManager::createRepository($type, $config, $name)`.
pub fn create_repository(http: &HttpClient, config: &Value, name: &str) -> Result<Arc<dyn Repository>> {
    let kind = config.get("type").and_then(Value::as_str).unwrap_or("");
    if !REPOSITORY_TYPES.contains(&kind) {
        bail!("Repository type is not registered: {kind}");
    }
    if config.get("packagist") == Some(&Value::Bool(false)) {
        http.io().warning(&format!(
            "Repository \"{name}\" ({}) has a packagist key which should be in its own repository definition",
            json::encode(config, Flags::PLAIN)
        ));
    }
    let mut inner = config.clone();
    let filter = ["only", "exclude", "canonical"].iter().any(|k| config.get(*k).is_some_and(|v| !v.is_null()));
    if let Some(m) = inner.as_object_mut() {
        for k in ["only", "exclude", "canonical"] {
            m.shift_remove(k);
        }
    }
    let base_dir = http.config().base_dir().map(std::path::Path::to_path_buf).unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    let repo: Arc<dyn Repository> = match kind {
        "composer" => Arc::new(ComposerRepository::new(&inner, http.clone())?),
        "package" => Arc::new(PackageRepository::new(&inner)?),
        "artifact" => Arc::new(ArtifactRepository::new(&inner, http.io().clone())),
        "path" => Arc::new(PathRepository::new(&inner, http.io().clone(), &base_dir)?),
        "pear" => bail!("The PEAR repository has been removed from Composer 2.x"),
        _ => Arc::new(VcsRepository::new(&inner, http.clone())?),
    };
    if filter {
        return Ok(Arc::new(FilterRepository::new(repo, config)?));
    }
    Ok(repo)
}

/// `RepositoryFactory::createRepos()` over a list of definitions (e.g. `Config::repositories()`).
pub fn create_repositories(http: &HttpClient, repos: &[Value]) -> Result<Vec<Arc<dyn Repository>>> {
    let mut names: Vec<String> = vec![];
    let mut out = vec![];
    for (index, repo) in repos.iter().enumerate() {
        if repo.is_string() {
            bail!("\"repositories\" should be an array of repository definitions, only a single repository was given");
        }
        if !repo.is_object() {
            bail!("Repository \"{index}\" ({}) should be an array, {} given", json::encode(repo, Flags::PLAIN), php_type(repo));
        }
        if repo.get("type").is_none_or(Value::is_null) {
            bail!("Repository \"{index}\" ({}) must have a type defined", json::encode(repo, Flags::PLAIN));
        }
        let name = generate_repository_name(&index.to_string(), true, repo, &names);
        out.push(create_repository(http, repo, &index.to_string())?);
        names.push(name);
    }
    Ok(out)
}

/// `RepositoryFactory::generateRepositoryName()`: list entries are named after their URL.
pub fn generate_repository_name(index: &str, is_int: bool, repo: &Value, existing: &[String]) -> String {
    static SCHEME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^https?://").unwrap());
    let mut name = match repo.get("url").and_then(Value::as_str) {
        Some(url) if is_int => SCHEME.replace(url, "").into_owned(),
        _ => index.to_string(),
    };
    while existing.contains(&name) {
        name.push('2');
    }
    name
}

/// `RepositoryFactory::configFromString()`: a `--repository` argument to a definition.
pub fn config_from_string(repository: &str) -> Result<Value> {
    if repository.to_ascii_lowercase().starts_with("http") {
        return Ok(json!({"type": "composer", "url": repository}));
    }
    if repository.ends_with(".json") {
        let data = json::read(std::path::Path::new(repository))?;
        let filled = |k: &str| data.get(k).is_some_and(|v| !komposer_package::loader::php_empty(v));
        if filled("packages") || filled("includes") || filled("provider-includes") {
            let real = std::fs::canonicalize(repository)?;
            return Ok(json!({"type": "composer", "url": format!("file://{}", real.display().to_string().replace('\\', "/"))}));
        }
        bail!("Invalid repository URL ({repository}) given. This file does not contain a valid composer repository.");
    }
    if repository.starts_with('{') {
        return json::parse(repository);
    }
    bail!("Invalid repository url ({}) given. Has to be a .json file, an http url or a JSON object.", sanitize(repository))
}

/// `FilterRepository`: limits a repository to (or excludes) package name patterns, and with
/// `canonical: false` lets lower priority repositories provide the same names.
pub struct FilterRepository {
    inner: Arc<dyn Repository>,
    only: Option<Regex>,
    exclude: Option<Regex>,
    canonical: bool,
}

impl FilterRepository {
    pub fn new(inner: Arc<dyn Repository>, options: &Value) -> Result<FilterRepository> {
        let patterns = |key: &str| -> Result<Option<Regex>> {
            match options.get(key) {
                None | Some(Value::Null) => Ok(None),
                Some(Value::Array(a)) => Ok(Some(package_names_regex(&a.iter().filter_map(|v| v.as_str().map(String::from)).collect::<Vec<_>>()))),
                Some(_) => Err(anyhow!("\"{key}\" key for repository {} should be an array", inner.repo_name())),
            }
        };
        let (only, exclude) = (patterns("only")?, patterns("exclude")?);
        if only.is_some() && exclude.is_some() {
            bail!("Only one of \"only\" and \"exclude\" can be specified for repository {}", inner.repo_name());
        }
        let canonical = match options.get("canonical") {
            None | Some(Value::Null) => true,
            Some(Value::Bool(b)) => *b,
            Some(_) => bail!("\"canonical\" key for repository {} should be a boolean", inner.repo_name()),
        };
        Ok(FilterRepository { inner, only, exclude, canonical })
    }

    pub fn inner(&self) -> &Arc<dyn Repository> {
        &self.inner
    }

    fn is_allowed(&self, name: &str) -> bool {
        match (&self.only, &self.exclude) {
            (Some(only), _) => only.is_match(name),
            (None, Some(exclude)) => !exclude.is_match(name),
            _ => true,
        }
    }

    fn allowed(&self, constraints: &[(String, Constraint)]) -> Vec<(String, Constraint)> {
        constraints.iter().filter(|(n, _)| self.is_allowed(n)).cloned().collect()
    }
}

impl Repository for FilterRepository {
    fn repo_name(&self) -> String {
        self.inner.repo_name()
    }

    fn kind(&self) -> RepoKind {
        self.inner.kind()
    }

    fn load_packages(&self, req: &LoadRequest) -> Result<LoadResult> {
        let names: Vec<(String, Option<Constraint>)> = req.names.iter().filter(|(n, _)| self.is_allowed(n)).cloned().collect();
        if names.is_empty() {
            return Ok(LoadResult::default());
        }
        let mut result = self.inner.load_packages(&LoadRequest { names: &names, ..*req })?;
        if !self.canonical {
            result.names_found.clear();
        }
        Ok(result)
    }

    fn packages(&self) -> Result<Vec<PackageRef>> {
        Ok(self.inner.packages()?.into_iter().filter(|p| self.is_allowed(&p.name)).collect())
    }

    fn find_packages(&self, name: &str, constraint: Option<&Constraint>) -> Result<Vec<PackageRef>> {
        if !self.is_allowed(name) {
            return Ok(vec![]);
        }
        self.inner.find_packages(name, constraint)
    }

    fn has_security_advisories(&self) -> bool {
        self.inner.has_security_advisories()
    }

    fn security_advisories(&self, constraints: &[(String, Constraint)], allow_partial: bool) -> Result<AdvisoryResult> {
        self.inner.security_advisories(&self.allowed(constraints), allow_partial)
    }

    fn filter_entries(&self, constraints: &[(String, Constraint)], lists: &[String]) -> Result<Vec<FilterEntry>> {
        self.inner.filter_entries(&self.allowed(constraints), lists)
    }

    fn providers(&self, name: &str) -> Result<Vec<String>> {
        Ok(self.inner.providers(name)?.into_iter().filter(|p| self.is_allowed(p)).collect())
    }

    fn provider_descriptions(&self, name: &str) -> Result<Vec<(String, Option<String>)>> {
        Ok(self.inner.provider_descriptions(name)?.into_iter().filter(|(p, _)| self.is_allowed(p)).collect())
    }

    fn loaded_files(&self) -> Vec<String> {
        self.inner.loaded_files()
    }

    fn prefetch(&self, names: &[String], dev_names: &[String]) {
        let allowed = |list: &[String]| list.iter().filter(|n| self.is_allowed(n)).cloned().collect::<Vec<_>>();
        self.inner.prefetch(&allowed(names), &allowed(dev_names));
    }

    fn search(&self, query: &str, mode: SearchMode, kind: Option<&str>) -> Result<Vec<Value>> {
        let results = self.inner.search(query, mode, kind)?;
        Ok(results
            .into_iter()
            .filter(|r| mode == SearchMode::Vendor || r.get("name").and_then(Value::as_str).is_some_and(|n| self.is_allowed(n)))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::test_config;
    use komposer_package::io::Io;
    use komposer_semver::Stability;
    use std::collections::{HashMap, HashSet};

    fn http() -> HttpClient {
        HttpClient::new(Arc::new(Io::null()), &test_config()).unwrap()
    }

    #[test]
    fn repository_names() {
        let existing = vec!["0".to_string(), "example.org".to_string()];
        assert_eq!(generate_repository_name("0", true, &json!({}), &[]), "0");
        assert_eq!(generate_repository_name("0", true, &json!({}), &existing), "02");
        assert_eq!(generate_repository_name("0", true, &json!({"url": "https://example.org"}), &[]), "example.org");
        assert_eq!(generate_repository_name("0", true, &json!({"url": "https://example.org"}), &existing), "example.org2");
        assert_eq!(generate_repository_name("example.org", false, &json!({"url": "https://example.org/repository"}), &[]), "example.org");
        assert_eq!(generate_repository_name("example.org", false, &json!({"url": "https://example.org/repository"}), &existing), "example.org2");
    }

    #[test]
    fn creates_all_supported_types() {
        let http = http();
        for (config, name) in [
            (json!({"type": "composer", "url": "https://repo.packagist.org"}), "composer repo (https://repo.packagist.org)"),
            (json!({"type": "package", "package": {"name": "a/a", "version": "1.0"}}), "package repo (defining 1 package)"),
            (json!({"type": "vcs", "url": "https://github.com/acme/lib"}), "vcs repo (github https://github.com/acme/lib)"),
            (json!({"type": "git", "url": "https://example.org/lib.git"}), "vcs repo (git https://example.org/lib.git)"),
            (json!({"type": "artifact", "url": "fixtures/artifacts"}), "artifact repo (fixtures/artifacts)"),
            (json!({"type": "path", "url": "../x"}), "path repo (../x)"),
        ] {
            assert_eq!(create_repository(&http, &config, "0").unwrap().repo_name(), name);
        }
        for (config, error) in [
            (json!({"type": "pear", "url": "https://pear.php.net"}), "The PEAR repository has been removed from Composer 2.x"),
            (
                json!({"type": "hg", "url": "https://example.org/hg"}),
                "Repositories of type \"hg\" are not supported by komposer (https://example.org/hg)",
            ),
            (json!({"type": "nope"}), "Repository type is not registered: nope"),
        ] {
            assert_eq!(create_repository(&http, &config, "0").err().unwrap().to_string(), error);
        }
        assert!(create_repositories(&http, &[json!({"url": "x"})]).err().unwrap().to_string().contains("must have a type defined"));
        assert_eq!(create_repositories(&http, &http.config().repositories()).unwrap().len(), 1);
    }

    #[test]
    fn filter_repository() {
        let config = json!({"type": "package", "package": [{"name": "a/a", "version": "1.0"}, {"name": "b/b", "version": "1.0"}], "only": ["a/*"], "canonical": false});
        let repo = create_repository(&http(), &config, "0").unwrap();
        assert_eq!(repo.packages().unwrap().len(), 1);
        let names = vec![("a/a".to_string(), None), ("b/b".to_string(), None)];
        let acceptable: HashSet<Stability> = [Stability::Stable].into();
        let req =
            LoadRequest { names: &names, acceptable_stabilities: &acceptable, stability_flags: &HashMap::new(), already_loaded: &HashMap::new() };
        let result = repo.load_packages(&req).unwrap();
        assert_eq!(result.packages.len(), 1);
        assert!(result.names_found.is_empty());
        let both = json!({"type": "package", "package": [], "only": ["a"], "exclude": ["b"]});
        assert!(create_repository(&http(), &both, "0").err().unwrap().to_string().starts_with("Only one of \"only\" and \"exclude\""));
    }

    #[test]
    fn repository_strings() {
        assert_eq!(config_from_string("https://example.org").unwrap(), json!({"type": "composer", "url": "https://example.org"}));
        assert_eq!(config_from_string(r#"{"type": "vcs", "url": "x"}"#).unwrap()["type"], "vcs");
        assert!(config_from_string("nope").is_err());
    }
}
