//! Port of Composer's `DependencyResolver/PoolBuilderTest.php`, running `Fixtures/poolbuilder/*.test`.

use anyhow::{Context, Result};
use komposer_package::io::Io;
use komposer_package::loader::{LoadOptions, load};
use komposer_package::repository::{ArrayRepository, LoadRequest, LoadResult, RepoKind, Repository};
use komposer_package::{PackageRef, RootAlias};
use komposer_semver::{Stability, normalize};
use komposer_solver::{DefaultPolicy, PoolOptions, PrettyConstraint, RepositorySet, Request, UpdateAllowTransitive, optimize_pool};
use regex::Regex;
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// `FilterRepository` with `"canonical": false`: never claims the names it has.
struct NonCanonical(ArrayRepository);

impl Repository for NonCanonical {
    fn repo_name(&self) -> String {
        self.0.repo_name()
    }
    fn load_packages(&self, req: &LoadRequest) -> Result<LoadResult> {
        Ok(LoadResult { names_found: vec![], packages: self.0.load_packages(req)?.packages })
    }
    fn packages(&self) -> Result<Vec<PackageRef>> {
        self.0.packages()
    }
}

fn sections(text: &str) -> HashMap<String, String> {
    let re = Regex::new(r"(?:^|\n*)--([A-Z-]+)--\n").unwrap();
    let mut out = HashMap::new();
    let mut last: Option<(String, usize)> = None;
    for c in re.captures_iter(text) {
        let m = c.get(0).unwrap();
        if let Some((name, start)) = last.take() {
            out.insert(name, text[start..m.start()].to_string());
        }
        last = Some((c[1].to_string(), m.end()));
    }
    if let Some((name, start)) = last {
        out.insert(name, text[start..].to_string());
    }
    out
}

fn json_section(sections: &HashMap<String, String>, name: &str) -> Result<Value> {
    match sections.get(name).map(|s| s.trim()).filter(|s| !s.is_empty()) {
        Some(s) => serde_json::from_str(s).with_context(|| format!("section {name}")),
        None => Ok(Value::Null),
    }
}

struct Loader {
    ids: HashMap<i64, PackageRef>,
}

impl Loader {
    /// `ArrayLoader::load()`: returns the package (or its branch alias, like Composer) to add to a repository.
    fn load(&mut self, data: &Value) -> Result<PackageRef> {
        let mut data = data.as_object().context("package")?.clone();
        let id = data.remove("id").and_then(|v| v.as_i64());
        let loaded = load(&data, LoadOptions { transport_options: true, ..LoadOptions::default() })?;
        let package = loaded[0].clone();
        if let Some(id) = id {
            self.ids.insert(id, package.clone());
        }
        Ok(package)
    }
}

fn path_repo(dir: &Path, config: &Map<String, Value>, loader: &mut Loader) -> Result<ArrayRepository> {
    let url = config["url"].as_str().unwrap();
    let json = std::fs::read_to_string(dir.join(url).join("composer.json"))?;
    let mut package: Map<String, Value> = serde_json::from_str(&json)?;
    package.insert("dist".into(), json!({"type": "path", "url": url, "reference": "path-reference"}));
    let options = config.get("options").and_then(Value::as_object).cloned().unwrap_or_default();
    let transport: Map<String, Value> = options.into_iter().filter(|(k, _)| k == "symlink" || k == "relative").collect();
    package.insert("transport-options".into(), Value::Object(transport));
    let mut repo = ArrayRepository::new(format!("path repo ({url})"), RepoKind::Remote);
    repo.add(loader.load(&Value::Object(package))?);
    Ok(repo)
}

fn run(file: &Path) -> Result<()> {
    let dir = file.parent().unwrap();
    let text = std::fs::read_to_string(file)?;
    let sections = sections(&text);
    let root = json_section(&sections, "ROOT")?;
    let request_data = json_section(&sections, "REQUEST")?;
    let repos = json_section(&sections, "PACKAGE-REPOS")?;
    let fixed = json_section(&sections, "FIXED")?;
    let expect = json_section(&sections, "EXPECT")?;

    let stability = root.get("minimum-stability").and_then(Value::as_str).and_then(Stability::parse).unwrap_or(Stability::Stable);
    let flags: HashMap<String, Stability> = root
        .get("stability-flags")
        .and_then(Value::as_object)
        .map(|m| m.iter().map(|(k, v)| (k.clone(), Stability::parse(v.as_str().unwrap()).unwrap())).collect())
        .unwrap_or_default();
    let aliases: Vec<RootAlias> = root
        .get("aliases")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .map(|a| RootAlias {
                    package: a["package"].as_str().unwrap().into(),
                    version: normalize(a["version"].as_str().unwrap()).unwrap(),
                    alias: a["alias"].as_str().unwrap().into(),
                    alias_normalized: normalize(a["alias"].as_str().unwrap()).unwrap(),
                })
                .collect()
        })
        .unwrap_or_default();
    let references: HashMap<String, String> = root
        .get("references")
        .and_then(Value::as_object)
        .map(|m| m.iter().map(|(k, v)| (k.clone(), v.as_str().unwrap().into())).collect())
        .unwrap_or_default();

    let mut set = RepositorySet::new(stability).with_stability_flags(flags).with_root_aliases(&aliases).with_root_references(references);
    let mut loader = Loader { ids: HashMap::new() };
    for repo in repos.as_array().unwrap() {
        if let Some(config) = repo.as_object().filter(|m| m.contains_key("type")) {
            set.add_repository(Arc::new(path_repo(dir, config, &mut loader)?));
            continue;
        }
        let (packages, canonical) = match repo {
            Value::Object(m) => (m["packages"].as_array().unwrap().clone(), m.get("canonical") != Some(&Value::Bool(false))),
            v => (v.as_array().unwrap().clone(), true),
        };
        let mut array = ArrayRepository::new("package repo", RepoKind::Remote);
        for p in &packages {
            array.add(loader.load(p)?);
        }
        if canonical {
            set.add_repository(Arc::new(array));
        } else {
            set.add_repository(Arc::new(NonCanonical(array)));
        }
    }

    let mut locked = ArrayRepository::new("lock file", RepoKind::Lock);
    for p in request_data.get("locked").and_then(Value::as_array).into_iter().flatten() {
        locked.add(loader.load(p)?);
    }
    let locked_packages = locked.all().to_vec();
    let locked: Arc<dyn Repository> = Arc::new(locked);
    set.add_repository(locked.clone());

    let mut request = Request::with_locked_repository(locked)?;
    for (name, constraint) in request_data["require"].as_object().unwrap() {
        request.require_name(name, Some(PrettyConstraint::parse(constraint.as_str().unwrap())?))?;
    }
    if let Some(list) = request_data.get("allowList").and_then(Value::as_array) {
        let flag = |k: &str| request_data.get(k) == Some(&Value::Bool(true));
        let mode = if flag("allowTransitiveDeps") {
            UpdateAllowTransitive::WithDeps
        } else if flag("allowTransitiveDepsNoRootRequire") {
            UpdateAllowTransitive::WithDepsNoRootRequire
        } else {
            UpdateAllowTransitive::OnlyListed
        };
        request.set_update_allow_list(list.iter().map(|v| v.as_str().unwrap().to_string()).collect(), mode);
    }
    for p in fixed.as_array().into_iter().flatten() {
        request.fix_package(loader.load(p)?);
    }

    let pool = set.create_pool(&mut request, &Io::null(), &PoolOptions::default())?;

    let id_of = |p: &PackageRef| loader.ids.iter().find(|(_, q)| Arc::ptr_eq(p, q)).map(|(id, _)| *id);
    let describe = |packages: &[PackageRef]| -> Vec<String> {
        let mut result: Vec<String> = packages
            .iter()
            .map(|p| {
                if let Some(id) = id_of(p) {
                    return id.to_string();
                }
                let mut suffix = p.source_reference().filter(|r| !r.is_empty()).map(|r| format!("#{r}")).unwrap_or_default();
                if locked_packages.iter().any(|l| Arc::ptr_eq(l, p)) {
                    suffix += " (locked)";
                }
                match &p.alias_of {
                    Some(of) => match id_of(of) {
                        Some(id) => format!("{}-{}{suffix} (alias of {id})", p.name, p.version),
                        None => format!("{}-{}{suffix} (alias of {})", p.name, p.version, of.version),
                    },
                    None => format!("{}-{}{suffix}", p.name, p.version),
                }
            })
            .collect();
        result.sort();
        result
    };
    let expected = |v: &Value| -> Vec<String> {
        let mut list: Vec<String> = v.as_array().unwrap().iter().map(|v| v.as_str().map(String::from).unwrap_or_else(|| v.to_string())).collect();
        list.sort();
        list
    };
    let result = describe(pool.packages());
    let unoptimized = expected(&expect);
    anyhow::ensure!(unoptimized == result, "{}\nexpected {unoptimized:#?}\n     got {result:#?}", sections["TEST"].trim());

    let expect_optimized = json_section(&sections, "EXPECT-OPTIMIZED")?;
    let optimized = expected(if expect_optimized.is_null() { &expect } else { &expect_optimized });
    let result = describe(optimize_pool(&DefaultPolicy::default(), &request, pool).packages());
    anyhow::ensure!(optimized == result, "{} (optimized)\nexpected {optimized:#?}\n     got {result:#?}", sections["TEST"].trim());
    Ok(())
}

#[test]
fn pool_builder_fixtures() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/poolbuilder");
    let mut files: Vec<PathBuf> =
        std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()).filter(|p| p.extension().is_some_and(|e| e == "test")).collect();
    files.sort();
    let failures: Vec<String> =
        files.iter().filter_map(|f| run(f).err().map(|e| format!("{}: {e:#}", f.file_name().unwrap().to_string_lossy()))).collect();
    assert_eq!(files.len(), 31);
    assert!(failures.is_empty(), "{} of {} fixtures failed:\n{}", failures.len(), files.len(), failures.join("\n\n"));
}
