//! `PathRepository`: packages from local directories (glob patterns allowed), installed as
//! `path` dists (symlinked or mirrored by the installer).

use crate::git;
use crate::url::sanitize;
use crate::util::{php_serialize, sha1_hex};
use crate::version_guesser::guess_version;
use anyhow::{Context, Result, bail};
use komposer_package::PackageRef;
use komposer_package::io::Io;
use komposer_package::json;
use komposer_package::loader::{self, LoadOptions};
use komposer_package::repository::{ArrayRepository, LoadRequest, LoadResult, RepoKind, Repository};
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub struct PathRepository {
    io: Arc<Io>,
    /// `url` with `~`/`$VAR` expanded
    url: String,
    repo_config: Value,
    options: Map<String, Value>,
    /// relative URLs are resolved against this directory (the project)
    base_dir: PathBuf,
    loaded: Mutex<Option<Arc<ArrayRepository>>>,
}

impl PathRepository {
    /// `new PathRepository($repoConfig, ...)`; relative URLs are relative to `base_dir`.
    pub fn new(repo_config: &Value, io: Arc<Io>, base_dir: &Path) -> Result<PathRepository> {
        let Some(url) = repo_config.get("url").and_then(Value::as_str) else {
            bail!("You must specify the `url` configuration for the path repository");
        };
        let url = komposer_package::config::expand_path(url);
        let mut options = repo_config.get("options").and_then(Value::as_object).cloned().unwrap_or_default();
        if !options.contains_key("relative") {
            let absolute = url.starts_with('/') || url.get(1..2) == Some(":") || url.starts_with("\\\\");
            options.insert("relative".into(), (!absolute).into());
        }
        Ok(PathRepository { io, url, repo_config: repo_config.clone(), options, base_dir: base_dir.to_path_buf(), loaded: Mutex::new(None) })
    }

    pub fn repo_config(&self) -> &Value {
        &self.repo_config
    }

    fn repository(&self) -> Result<Arc<ArrayRepository>> {
        crate::util::lazy_repository(&self.loaded, || self.initialize())
    }

    /// `PathRepository::initialize()`.
    fn initialize(&self) -> Result<ArrayRepository> {
        let mut repo = ArrayRepository::new(self.repo_name(), RepoKind::Remote);
        let matches = self.url_matches();
        if matches.is_empty() {
            if self.url.contains(['*', '{', '}']) {
                // the parent directory before any wildcard exists: correctly configured but empty
                let mut dir = self.url.as_str();
                while dir.contains(['*', '{', '}']) {
                    dir = Path::new(dir).parent().and_then(Path::to_str).unwrap_or("");
                }
                if self.resolve(dir).is_dir() {
                    return Ok(repo);
                }
            }
            bail!("The `url` supplied for the path ({}) repository does not exist", sanitize(&self.url));
        }
        let reference = self.options.get("reference").and_then(Value::as_str).unwrap_or("auto");
        let transport: Map<String, Value> =
            self.options.iter().filter(|(k, _)| matches!(k.as_str(), "symlink" | "relative")).map(|(k, v)| (k.clone(), v.clone())).collect();
        for url in matches {
            let path = std::fs::canonicalize(self.resolve(&url)).unwrap_or_else(|_| self.resolve(&url));
            let composer_file = path.join("composer.json");
            if !composer_file.exists() {
                continue;
            }
            let contents = std::fs::read_to_string(&composer_file).with_context(|| format!("Could not read {}", composer_file.display()))?;
            let parsed = json::parse(&contents).with_context(|| format!("\"{}\" does not contain valid JSON", composer_file.display()))?;
            let Value::Object(mut package) = parsed else { bail!("\"{}\" does not contain a package definition", composer_file.display()) };
            let mut dist = json!({"type": "path", "url": url});
            match reference {
                "none" => dist["reference"] = Value::Null,
                "config" | "auto" => {
                    dist["reference"] = sha1_hex((contents.clone() + &php_serialize(&Value::Object(self.options.clone()))).as_bytes()).into()
                }
                _ => {}
            }
            package.insert("transport-options".into(), Value::Object(transport.clone()));

            let name = package.get("name").and_then(Value::as_str).map(String::from);
            if let Some(version) = name.as_ref().and_then(|n| self.options.get("versions")?.get(n.as_str()).cloned()) {
                package.insert("version".into(), version);
            }
            // carry over the root version when this path is in the same git repository as the root package
            if !package.contains_key("version")
                && let Some(root_version) = std::env::var("COMPOSER_ROOT_VERSION").ok().filter(|v| !v.is_empty())
            {
                let (here, root) = (git::run(&["rev-parse", "HEAD"], Some(&path)), git::run(&["rev-parse", "HEAD"], None));
                if here.ok() && root.ok() && here.stdout == root.stdout {
                    package.insert("version".into(), loader::root_version_from_env(&root_version).into());
                }
            }
            if reference == "auto" && path.join(".git").is_dir() {
                let mut args = git::rev_list_command(&["-n1", "--format=%H", "HEAD"]);
                args.extend(git::no_show_signature_flags());
                let out = git::run(&args, Some(&path));
                if out.ok() {
                    dist["reference"] = git::parse_rev_list_output(&out.stdout).trim().into();
                }
            }
            package.insert("dist".into(), dist);
            if !package.contains_key("version") {
                match guess_version(&package, &path, &self.io).filter(|v| !v.pretty_version.is_empty()) {
                    Some(v) => {
                        // a feature branch also gets a package with the feature branch version
                        if let Some(feature) = v.feature_pretty_version {
                            package.insert("version".into(), feature.into());
                            for p in self.load(&package, &composer_file)? {
                                repo.add(p);
                            }
                        }
                        package.insert("version".into(), v.pretty_version.into());
                    }
                    None => {
                        package.insert("version".into(), "dev-main".into());
                    }
                }
            }
            for p in self.load(&package, &composer_file)? {
                repo.add(p);
            }
        }
        Ok(repo)
    }

    fn load(&self, package: &Map<String, Value>, composer_file: &Path) -> Result<Vec<PackageRef>> {
        loader::load(package, LoadOptions { transport_options: true, skip_self_links: false })
            .with_context(|| format!("Failed loading the package in {}", composer_file.display()))
    }

    fn resolve(&self, url: &str) -> PathBuf {
        if Path::new(url).is_absolute() { PathBuf::from(url) } else { self.base_dir.join(url) }
    }

    /// `getUrlMatches()`: `glob($url, GLOB_MARK | GLOB_ONLYDIR | GLOB_BRACE)`, keeping relative URLs relative.
    fn url_matches(&self) -> Vec<String> {
        let relative = !Path::new(&self.url).is_absolute();
        let base = format!("{}/", self.base_dir.display());
        let mut out = vec![];
        for pattern in expand_braces(&self.url) {
            let full = if relative { format!("{base}{pattern}") } else { pattern };
            let Ok(paths) = glob::glob(&full) else { continue };
            for path in paths.flatten().filter(|p| p.is_dir()) {
                let s = path.to_string_lossy().replace('\\', "/");
                let s = if relative { s.strip_prefix(&base).unwrap_or(&s).to_string() } else { s };
                out.push(s.trim_end_matches('/').to_string());
            }
        }
        out
    }
}

/// `GLOB_BRACE`: `a/{b,c}` => `a/b`, `a/c`.
fn expand_braces(pattern: &str) -> Vec<String> {
    let Some(open) = pattern.find('{') else { return vec![pattern.to_string()] };
    let mut depth = 0;
    let mut close = None;
    let mut splits = vec![];
    for (i, c) in pattern[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(open + i);
                    break;
                }
            }
            ',' if depth == 1 => splits.push(open + i),
            _ => {}
        }
    }
    let Some(close) = close else { return vec![pattern.to_string()] };
    let (prefix, suffix) = (&pattern[..open], &pattern[close + 1..]);
    let mut bounds = vec![open];
    bounds.extend(&splits);
    bounds.push(close);
    bounds.windows(2).flat_map(|w| expand_braces(&format!("{prefix}{}{suffix}", &pattern[w[0] + 1..w[1]]))).collect()
}

impl Repository for PathRepository {
    fn repo_name(&self) -> String {
        format!("path repo ({})", sanitize(self.repo_config.get("url").and_then(Value::as_str).unwrap_or("")))
    }

    fn load_packages(&self, req: &LoadRequest) -> Result<LoadResult> {
        self.repository()?.load_packages(req)
    }

    fn packages(&self) -> Result<Vec<PackageRef>> {
        self.repository()?.packages()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixtures() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/path")
    }

    fn repo(config: Value) -> PathRepository {
        PathRepository::new(&config, Arc::new(Io::null()), &fixtures()).unwrap()
    }

    fn names(packages: &[PackageRef]) -> Vec<String> {
        let mut n: Vec<String> = packages.iter().map(|p| p.name.clone()).collect();
        n.sort();
        n.dedup();
        n
    }

    #[test]
    fn incorrect_path() {
        let err = repo(json!({"url": fixtures().join("missing")})).packages().unwrap_err();
        assert!(err.to_string().contains("repository does not exist"));
        // an existing parent before the wildcard is just empty
        assert!(repo(json!({"url": fixtures().join("missing-*")})).packages().unwrap().is_empty());
    }

    #[test]
    fn with_version() {
        let packages = repo(json!({"url": fixtures().join("with-version")})).packages().unwrap();
        assert_eq!(packages.len(), 1);
        assert_eq!((packages[0].name.as_str(), packages[0].version.as_str()), ("test/path-versioned", "0.0.2.0"));
        let dist = packages[0].dist.as_ref().unwrap();
        assert_eq!(dist.kind, "path");
        assert_eq!(packages[0].transport_options, json!({"relative": false}));
        let packages = repo(json!({"url": fixtures().join("with-published-time")})).packages().unwrap();
        assert_eq!(packages[0].name, "test/path-published-time");
    }

    #[test]
    fn without_version_and_wildcards() {
        let packages = repo(json!({"url": fixtures().join("without-version")})).packages().unwrap();
        assert_eq!(packages[0].name, "test/path-unversioned");
        assert!(!packages[0].version.is_empty());
        let packages = repo(json!({"url": fixtures().join("*")})).packages().unwrap();
        assert_eq!(names(&packages), ["test/path-published-time", "test/path-unversioned", "test/path-versioned"]);
        let packages = repo(json!({"url": fixtures().join("{with-version,without-version}")})).packages().unwrap();
        assert_eq!(names(&packages), ["test/path-unversioned", "test/path-versioned"]);
    }

    #[test]
    fn explicit_versions() {
        let options =
            json!({"versions": {"test/path-unversioned": "4.3.2.1", "test/path-versioned": "3.2.1.0", "test/path-published-time": "2.1.0.0"}});
        let packages = repo(json!({"url": fixtures().join("*"), "options": options})).packages().unwrap();
        assert_eq!(packages.len(), 3);
        let mut versions: Vec<(String, String)> = packages.iter().map(|p| (p.name.clone(), p.version.clone())).collect();
        versions.sort();
        assert_eq!(versions[1], ("test/path-unversioned".into(), "4.3.2.1".into()));
        assert_eq!(versions[2], ("test/path-versioned".into(), "3.2.1.0".into()));
    }

    #[test]
    fn url_remains_relative() {
        let packages = repo(json!({"url": "with-version"})).packages().unwrap();
        assert_eq!(packages[0].dist.as_ref().unwrap().url, "with-version");
        assert_eq!(packages[0].transport_options, json!({"relative": true}));
    }

    #[test]
    fn references() {
        let packages = repo(json!({"url": fixtures().join("*"), "options": {"reference": "none"}})).packages().unwrap();
        assert!(packages.iter().all(|p| p.dist_reference().is_none()));
        let options = json!({"reference": "config", "relative": true});
        let packages = repo(json!({"url": fixtures().join("*"), "options": options})).packages().unwrap();
        for p in packages {
            let dist = p.dist.as_ref().unwrap();
            let contents = std::fs::read_to_string(Path::new(&dist.url).join("composer.json")).unwrap();
            let expected = sha1_hex(format!("{contents}a:2:{{s:9:\"reference\";s:6:\"config\";s:8:\"relative\";b:1;}}").as_bytes());
            assert_eq!(dist.reference.as_deref(), Some(expected.as_str()));
        }
    }

    #[test]
    fn braces() {
        assert_eq!(expand_braces("a/{b,c}/d"), ["a/b/d", "a/c/d"]);
        assert_eq!(expand_braces("{x,{y,z}}"), ["x", "y", "z"]);
        assert_eq!(expand_braces("plain"), ["plain"]);
    }
}
