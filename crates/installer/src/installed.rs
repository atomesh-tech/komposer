//! `Repository\InstalledFilesystemRepository` (`FilesystemRepository::write()` and its
//! `installed.php` dump) and `InstalledArrayRepository`.

use crate::fs::{self, find_shortest_path, normalize_path, var_export};
use anyhow::Result;
use komposer_package::repository::{ArrayRepository, RepoKind, read_installed_json};
use komposer_package::{Package, PackageRef, RootPackage, dumper, is_platform_package, json};
use serde_json::{Map, Value, json};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

/// Composer's runtime class, copied next to installed.php.
const INSTALLED_VERSIONS: &str = include_str!("InstalledVersions.php");

/// The local repository of installed packages.
pub struct InstalledRepo {
    /// `vendor/composer/installed.json`; `None` for an in-memory repository (create-project).
    pub file: Option<String>,
    pub packages: ArrayRepository,
    /// `setDevPackageNames()`: the lock file's dev package names.
    pub dev_package_names: Vec<String>,
    /// `getDevMode()`: the `dev` flag of the installed.json that was read.
    pub dev_mode: Option<bool>,
    /// With a root package, `write()` also dumps installed.php and InstalledVersions.php.
    pub root: Option<RootPackage>,
}

impl InstalledRepo {
    /// `InstalledArrayRepository`.
    pub fn in_memory() -> InstalledRepo {
        InstalledRepo {
            file: None,
            packages: ArrayRepository::new("installed repo", RepoKind::Installed),
            dev_package_names: vec![],
            dev_mode: None,
            root: None,
        }
    }

    /// `new InstalledFilesystemRepository($file, true, $root)` plus `initialize()`.
    pub fn load(file: &str, root: Option<RootPackage>) -> Result<InstalledRepo> {
        let installed = read_installed_json(Path::new(file))?;
        Ok(InstalledRepo {
            file: Some(file.to_string()),
            packages: installed.packages,
            dev_package_names: installed.dev_package_names,
            dev_mode: installed.dev_mode,
            root,
        })
    }

    /// `hasPackage()`: by unique name (`name-version`).
    pub fn has(&self, package: &Package) -> bool {
        self.find(package).is_some()
    }

    pub fn find(&self, package: &Package) -> Option<PackageRef> {
        let id = package.unique_name();
        self.packages.all().iter().find(|p| p.unique_name() == id).cloned()
    }

    /// `removePackage()`: the first package with the same unique name.
    pub fn remove(&mut self, package: &Package) {
        if let Some(p) = self.find(package) {
            self.packages.remove(&p);
        }
    }

    /// `FilesystemRepository::write()`. `install_path` is `InstallationManager::getInstallPath()`.
    pub fn write(&self, dev_mode: bool, install_path: impl Fn(&Package) -> Option<String>) -> Result<()> {
        let Some(file) = &self.file else { return Ok(()) };
        // realpath, as it normalizes more than normalizePath and findShortestPath needs that
        let repo_dir = fs::dirname(file);
        fs::ensure_directory_exists(&repo_dir)?;
        let repo_dir = normalize_path(&fs::realpath(&repo_dir).unwrap_or(repo_dir));
        let cwd = fs::cwd()?;

        let mut packages = vec![];
        let mut dev_names = vec![];
        let mut install_paths = HashMap::new();
        for p in self.packages.canonical() {
            let mut data = dumper::dump(&p);
            let path = match install_path(&p).filter(|p| !p.is_empty()) {
                Some(path) => {
                    let abs = if fs::is_absolute_path(&path) { path } else { format!("{cwd}/{path}") };
                    Some(find_shortest_path(&repo_dir, &normalize_path(&abs), true, false)?)
                }
                None => None,
            };
            data.insert("install-path".into(), path.clone().map_or(Value::Null, Value::String));
            install_paths.insert(p.name.clone(), path);
            packages.push(data);
            // only names which are really installed, the lock lists them all upfront
            if self.dev_package_names.contains(&p.name) {
                dev_names.push(p.name.clone());
            }
        }
        dev_names.sort();
        packages.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
        json::write(Path::new(file), &json!({"packages": packages, "dev": dev_mode, "dev-package-names": dev_names}))?;

        if let Some(root) = &self.root {
            let versions = self.installed_versions(root, &install_paths, dev_mode, &repo_dir, &cwd)?;
            let mut code = String::from("<?php return ");
            dump_php(&versions, 0, &mut code);
            code.push_str(";\n");
            fs::file_put_contents_if_modified(&format!("{repo_dir}/installed.php"), &code)?;
            fs::file_put_contents_if_modified(&format!("{repo_dir}/InstalledVersions.php"), INSTALLED_VERSIONS)?;
        }
        Ok(())
    }

    /// `generateInstalledVersions()`.
    fn installed_versions(
        &self,
        root: &RootPackage,
        install_paths: &HashMap<String, Option<String>>,
        dev_mode: bool,
        repo_dir: &str,
        cwd: &str,
    ) -> Result<Value> {
        let root_pkg = Arc::new(root.package.clone());
        let mut packages = self.packages.all().to_vec();
        if let Some((version, pretty)) = &root.branch_alias {
            let mut alias = Package::alias(&root_pkg, version, pretty);
            alias.root_package_alias = true;
            packages.push(Arc::new(alias));
        }
        packages.push(root_pkg.clone());
        let is_root = |p: &PackageRef| Arc::ptr_eq(p, &root_pkg) || p.root_package_alias;
        let is_dev = |p: &Package| self.dev_package_names.contains(&p.name);

        let root_path = find_shortest_path(repo_dir, &normalize_path(&fs::realpath(cwd).unwrap_or(cwd.to_string())), true, false)?;
        let installed = |p: &Package, install_path: Option<String>| {
            json!({
                "pretty_version": p.pretty_version,
                "version": p.version,
                "reference": installed_reference(p),
                "type": p.package_type,
                "install_path": install_path,
                "aliases": [],
                "dev_requirement": is_dev(p),
            })
        };
        let root_data = installed(&root_pkg, Some(root_path.clone()));
        let mut root_entry = json!({
            "name": root_pkg.name,
            "pretty_version": root_data["pretty_version"],
            "version": root_data["version"],
            "reference": root_data["reference"],
            "type": root_data["type"],
            "install_path": root_path,
            "aliases": [],
            "dev": dev_mode,
        });

        let mut versions = Map::new();
        for p in packages.iter().filter(|p| !p.is_alias()) {
            let data = if Arc::ptr_eq(p, &root_pkg) { root_data.clone() } else { installed(p, install_paths.get(&p.name).cloned().flatten()) };
            versions.insert(p.name.clone(), data);
        }
        for p in &packages {
            let dev = is_dev(p);
            for (key, links) in [("replaced", &p.replaces), ("provided", &p.provides)] {
                // platform packages can not be checked for presence when really there
                for link in links.iter().filter(|l| !is_platform_package(&l.target)) {
                    let Value::Object(entry) = versions.entry(link.target.clone()).or_insert_with(|| json!({})) else { unreachable!() };
                    if !entry.contains_key("dev_requirement") || !dev {
                        entry.insert("dev_requirement".into(), Value::Bool(dev));
                    }
                    let version = if link.pretty_constraint == "self.version" { &p.pretty_version } else { &link.pretty_constraint };
                    push_unique(entry, key, version);
                }
            }
        }
        for p in packages.iter().filter(|p| p.is_alias()) {
            let Value::Object(entry) = versions.entry(p.name.clone()).or_insert_with(|| json!({})) else { unreachable!() };
            push(entry, "aliases", &p.pretty_version);
            if is_root(p) {
                push(root_entry.as_object_mut().unwrap(), "aliases", &p.pretty_version);
            }
        }

        versions.sort_keys();
        for entry in versions.values_mut().filter_map(Value::as_object_mut) {
            for key in ["aliases", "replaced", "provided"] {
                if let Some(Value::Array(list)) = entry.get_mut(key) {
                    list.sort_by(|a, b| strnatcmp(a.as_str().unwrap_or(""), b.as_str().unwrap_or("")));
                }
            }
        }
        Ok(json!({"root": root_entry, "versions": versions}))
    }
}

/// `dumpInstalledPackage()`'s reference: the installed source's, else source or dist (PHP truthiness).
fn installed_reference(p: &Package) -> Option<String> {
    let truthy = |s: &&str| !s.is_empty() && *s != "0";
    let by_source =
        p.installation_source.as_deref().filter(truthy).map(|src| if src == "source" { p.source_reference() } else { p.dist_reference() });
    match by_source {
        Some(reference @ Some(_)) => reference,
        _ => p.source_reference().filter(truthy).or(p.dist_reference()).filter(truthy),
    }
    .map(String::from)
}

fn push(entry: &mut Map<String, Value>, key: &str, value: &str) {
    if let Value::Array(list) = entry.entry(key).or_insert_with(|| json!([])) {
        list.push(value.into());
    }
}

fn push_unique(entry: &mut Map<String, Value>, key: &str, value: &str) {
    if !entry.get(key).and_then(Value::as_array).is_some_and(|l| l.iter().any(|v| v == value)) {
        push(entry, key, value);
    }
}

/// `dumpToPhpCode()`: `var_export`-style arrays, `install_path`s relative to `__DIR__`.
fn dump_php(value: &Value, level: usize, out: &mut String) {
    out.push_str("array(\n");
    let entries: Vec<(Option<&str>, String, &Value)> = match value {
        Value::Object(m) => m.iter().map(|(k, v)| (Some(k.as_str()), var_export(k), v)).collect(),
        Value::Array(a) => a.iter().enumerate().map(|(i, v)| (None, i.to_string(), v)).collect(),
        _ => vec![],
    };
    for (key, exported, v) in entries {
        out.push_str(&"    ".repeat(level + 1));
        out.push_str(&exported);
        out.push_str(" => ");
        match v {
            Value::Array(a) if a.is_empty() => out.push_str("array(),\n"),
            Value::Object(m) if m.is_empty() => out.push_str("array(),\n"),
            Value::Array(_) | Value::Object(_) => dump_php(v, level + 1, out),
            Value::String(s) if key == Some("install_path") && !fs::is_absolute_path(s) => {
                out.push_str(&format!("__DIR__ . {},\n", var_export(&format!("/{s}"))));
            }
            Value::String(s) => out.push_str(&format!("{},\n", var_export(s))),
            Value::Bool(b) => out.push_str(if *b { "true,\n" } else { "false,\n" }),
            Value::Null | Value::Number(_) => out.push_str("null,\n"),
        }
    }
    out.push_str(&"    ".repeat(level));
    out.push(')');
    if level > 0 {
        out.push_str(",\n");
    }
}

/// PHP `strnatcmp()` (`sort($a, SORT_NATURAL)`).
pub(crate) fn strnatcmp(a: &str, b: &str) -> Ordering {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.is_empty() || b.is_empty() {
        return a.len().cmp(&b.len());
    }
    let at = |s: &[u8], i: usize| s.get(i).copied().unwrap_or(0);
    let (mut ai, mut bi) = (0, 0);
    let mut leading = true;
    loop {
        let (mut ca, mut cb) = (at(a, ai), at(b, bi));
        // skip over leading zeros
        while leading && ca == b'0' && at(a, ai + 1).is_ascii_digit() {
            ai += 1;
            ca = at(a, ai);
        }
        while leading && cb == b'0' && at(b, bi + 1).is_ascii_digit() {
            bi += 1;
            cb = at(b, bi);
        }
        leading = false;
        while ca.is_ascii_whitespace() || ca == 0x0b {
            ai += 1;
            ca = at(a, ai);
        }
        while cb.is_ascii_whitespace() || cb == 0x0b {
            bi += 1;
            cb = at(b, bi);
        }
        if ca.is_ascii_digit() && cb.is_ascii_digit() {
            let fractional = ca == b'0' || cb == b'0';
            let result = compare_digits(a, &mut ai, b, &mut bi, fractional);
            if result != Ordering::Equal {
                return result;
            }
            match (ai >= a.len(), bi >= b.len()) {
                (true, true) => return Ordering::Equal,
                (true, false) => return Ordering::Less,
                (false, true) => return Ordering::Greater,
                _ => {}
            }
            ca = at(a, ai);
            cb = at(b, bi);
        }
        if ca != cb {
            return ca.cmp(&cb);
        }
        ai += 1;
        bi += 1;
        match (ai >= a.len(), bi >= b.len()) {
            (true, true) => return Ordering::Equal,
            (true, false) => return Ordering::Less,
            (false, true) => return Ordering::Greater,
            _ => {}
        }
    }
}

/// strnatcmp's `compare_left` (fractional) / `compare_right` (integer) digit runs.
fn compare_digits(a: &[u8], ai: &mut usize, b: &[u8], bi: &mut usize, fractional: bool) -> Ordering {
    let mut bias = Ordering::Equal;
    loop {
        let da = a.get(*ai).is_some_and(u8::is_ascii_digit);
        let db = b.get(*bi).is_some_and(u8::is_ascii_digit);
        match (da, db) {
            (false, false) => return bias,
            (false, true) => return Ordering::Less,
            (true, false) => return Ordering::Greater,
            _ => {}
        }
        let ord = a[*ai].cmp(&b[*bi]);
        if ord != Ordering::Equal {
            if fractional {
                return ord;
            }
            if bias == Ordering::Equal {
                bias = ord;
            }
        }
        *ai += 1;
        *bi += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use komposer_package::loader;
    use komposer_package::{LinkType, Location};

    fn package(name: &str, version: &str, pretty: &str) -> Package {
        Package::new(name, version, pretty)
    }

    fn link(p: &mut Package, kind: LinkType, target: &str, constraint: &str) {
        let l = loader::create_link(&p.name, &p.pretty_version, kind, target, constraint).unwrap();
        p.links_mut(kind).push(l);
    }

    fn with_refs(mut p: Package, source: Option<&str>, dist: Option<&str>) -> Package {
        p.source = source.map(|r| Location { reference: Some(r.into()), ..Default::default() });
        p.dist = dist.map(|r| Location { reference: Some(r.into()), ..Default::default() });
        p
    }

    #[test]
    fn natural_sort() {
        let mut v = vec!["^1.1", "2.0", "1.10", "1.4", "1.2"];
        v.sort_by(|a, b| strnatcmp(a, b));
        assert_eq!(v, ["1.2", "1.4", "1.10", "2.0", "^1.1"]);
        assert_eq!(strnatcmp("1.02", "1.1"), Ordering::Less);
        assert_eq!(strnatcmp("x2", "x10"), Ordering::Less);
    }

    #[test]
    fn writes_installed_json_sorted() {
        let dir = crate::tests::tmp_dir("installed-json");
        let mut repo = InstalledRepo { file: Some(format!("{dir}/vendor/composer/installed.json")), ..InstalledRepo::in_memory() };
        repo.dev_package_names = vec!["mypkg2".into(), "notinstalled".into()];
        repo.packages.add_package(package("mypkg2", "1.2.3.0", "1.2.3"));
        repo.packages.add_package(package("mypkg", "0.1.10.0", "0.1.10"));
        repo.write(true, |_| Some(format!("{dir}/woop/woop"))).unwrap();
        let written = std::fs::read_to_string(format!("{dir}/vendor/composer/installed.json")).unwrap();
        assert_eq!(
            written,
            r#"{
    "packages": [
        {
            "name": "mypkg",
            "version": "0.1.10",
            "version_normalized": "0.1.10.0",
            "type": "library",
            "install-path": "../../woop/woop"
        },
        {
            "name": "mypkg2",
            "version": "1.2.3",
            "version_normalized": "1.2.3.0",
            "type": "library",
            "install-path": "../../woop/woop"
        }
    ],
    "dev": true,
    "dev-package-names": [
        "mypkg2"
    ]
}
"#
        );
        assert!(!Path::new(&format!("{dir}/vendor/composer/installed.php")).exists(), "no root, no installed.php");

        let reloaded = InstalledRepo::load(repo.file.as_deref().unwrap(), None).unwrap();
        assert_eq!(reloaded.packages.len(), 2);
        assert_eq!(reloaded.dev_mode, Some(true));
        assert_eq!(reloaded.dev_package_names, ["mypkg2"]);
    }

    /// `FilesystemRepositoryTest::testRepositoryWritesInstalledPhp`.
    #[test]
    fn writes_installed_php_like_composer() {
        let _cwd = crate::tests::CWD.lock().unwrap_or_else(|e| e.into_inner());
        let dir = crate::tests::tmp_dir("installed-php");
        let previous = std::env::current_dir().unwrap();
        std::env::set_current_dir(&*dir).unwrap();
        let dir = fs::realpath(&dir).unwrap();

        let raw = json!({
            "version": "dev-master",
            "provide": {"foo/impl": "2.0"},
            "source": {"type": "", "url": "", "reference": "sourceref-by-default"},
            "dist": {"type": "", "url": "", "reference": "distref"},
            "extra": {"branch-alias": {"dev-master": "1.10.x-dev"}},
        });
        let root = loader::load_root(raw.as_object().unwrap(), || None).unwrap();
        let mut repo = InstalledRepo { file: Some(format!("{dir}/installed.json")), root: Some(root), ..InstalledRepo::in_memory() };
        repo.dev_package_names = vec!["c/c".into()];

        let mut p = with_refs(package("a/provider", "1.1.0.0", "1.1"), None, Some("distref-as-no-source"));
        link(&mut p, LinkType::Provide, "foo/impl", "^1.1");
        link(&mut p, LinkType::Provide, "foo/impl2", "2.0");
        repo.packages.add_package(p);

        let mut p = with_refs(package("a/provider2", "1.2.0.0", "1.2"), Some("sourceref"), Some("distref-as-installed-from-dist"));
        link(&mut p, LinkType::Provide, "foo/impl", "self.version");
        link(&mut p, LinkType::Provide, "foo/impl2", "2.0");
        p.installation_source = Some("dist".into());
        let provider2 = repo.packages.add_package(p);
        repo.packages.add(Arc::new(Package::alias(&provider2, "1.4.0.0", "1.4")));

        let mut p = package("b/replacer", "2.2.0.0", "2.2");
        link(&mut p, LinkType::Replace, "foo/impl2", "self.version");
        link(&mut p, LinkType::Replace, "foo/replaced", "^3.0");
        repo.packages.add_package(p);

        repo.packages.add_package(with_refs(
            package("c/c", "3.0.0.0", "3.0"),
            None,
            Some("{${passthru('bash -i')}} Foo\\Bar\n\ttab\x0bverticaltab\0"),
        ));

        let mut p = package("meta/package", "3.0.0.0", "3.0");
        p.package_type = "metapackage".into();
        repo.packages.add_package(p);

        let result = repo.write(true, |p| match p.name.as_str() {
            "meta/package" => Some(String::new()),
            "c/c" => Some("/foo/bar/ven\\do{}r/c/c${}".into()),
            "a/provider" => Some("vendor/{${passthru('bash -i')}}".into()),
            name => Some(format!("vendor/{name}")),
        });
        std::env::set_current_dir(previous).unwrap();
        result.unwrap();

        let expected = include_str!("../tests/fixtures/installed.php");
        assert_eq!(std::fs::read_to_string(format!("{dir}/installed.php")).unwrap(), expected);
        assert_eq!(std::fs::read_to_string(format!("{dir}/InstalledVersions.php")).unwrap(), INSTALLED_VERSIONS);
    }
}
