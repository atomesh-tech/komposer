//! Port of Composer's AutoloadGeneratorTest: the same package setups in temp dirs, the
//! generated files compared byte for byte with its Fixtures.

use komposer_autoload::{AutoloadOptions, ClassMap, dump};
use komposer_package::config::Config;
use komposer_package::io::Io;
use komposer_package::{Link, LinkType, Package, PackageRef, RootPackage};
use komposer_semver::{Constraint, Stability, parse_constraints};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/autoload");

struct Env {
    working_dir: PathBuf,
    vendor_dir: PathBuf,
    config: Value,
}

impl Env {
    fn new() -> Env {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!("komposer-autoload-gen-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let working_dir = dir.canonicalize().unwrap();
        let vendor_dir = working_dir.join("composer-test-autoload");
        std::fs::create_dir_all(&vendor_dir).unwrap();
        Env { working_dir, vendor_dir, config: json!({"platform-check": true, "use-include-path": false}) }
    }

    fn write(&self, path: impl AsRef<Path>, content: &str) {
        let path = self.working_dir.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    fn mkdir(&self, path: impl AsRef<Path>) {
        std::fs::create_dir_all(self.working_dir.join(path)).unwrap();
    }

    fn install_path(&self, package: &Package) -> Option<PathBuf> {
        (package.package_type != "metapackage")
            .then(|| self.vendor_dir.join(&package.name).join(package.target_dir.as_deref().unwrap_or("")))
            .map(|p| PathBuf::from(p.to_string_lossy().trim_end_matches('/')))
    }

    fn dump_in(&self, project_dir: &Path, root: &RootPackage, packages: &[PackageRef], opts: &AutoloadOptions) -> ClassMap {
        let mut config = Config::new(false, Some(project_dir));
        let mut values = self.config.clone();
        values["vendor-dir"] = json!(self.vendor_dir.to_string_lossy());
        config.merge_config(&values);
        let packages: Vec<(PackageRef, Option<PathBuf>)> = packages.iter().map(|p| (p.clone(), self.install_path(p))).collect();
        dump(&config, root, &packages, project_dir, opts, &Io::null()).unwrap()
    }

    fn dump_with(&self, root: &RootPackage, packages: &[PackageRef], opts: AutoloadOptions) -> ClassMap {
        self.dump_in(&self.working_dir, root, packages, &opts)
    }

    fn dump(&self, root: &RootPackage, packages: &[PackageRef], suffix: &str, optimize: bool) -> ClassMap {
        self.dump_with(root, packages, AutoloadOptions { suffix: Some(suffix.into()), optimize, dev_mode: Some(false), ..Default::default() })
    }

    fn read(&self, file: &str) -> String {
        std::fs::read_to_string(self.vendor_dir.join(file)).unwrap_or_else(|e| panic!("{file}: {e}"))
    }

    fn exists(&self, file: &str) -> bool {
        self.vendor_dir.join(file).exists()
    }

    #[track_caller]
    fn assert_fixture(&self, fixture: &str, file: &str) {
        let expected = std::fs::read_to_string(format!("{FIXTURES}/{fixture}")).unwrap().replace('\r', "");
        assert_eq!(self.read(file), expected, "{fixture} vs {file}");
    }

    /// `include` of autoload_classmap.php
    fn classmap(&self) -> BTreeMap<String, String> {
        let base = self.working_dir.to_string_lossy().into_owned();
        let vendor = self.vendor_dir.canonicalize().unwrap().to_string_lossy().into_owned();
        php_array(&self.read("composer/autoload_classmap.php"), &vendor, &base)
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.working_dir);
    }
}

/// Evaluates the `'key' => $vendorDir . '/path',` lines of a generated map.
fn php_array(file: &str, vendor: &str, base: &str) -> BTreeMap<String, String> {
    let unquote = |s: &str| s.trim_matches('\'').replace("\\'", "'").replace("\\\\", "\\");
    file.lines()
        .filter_map(|line| line.strip_prefix("    ")?.strip_suffix(',')?.split_once(" => "))
        .map(|(key, value)| {
            let value: String = value
                .split(" . ")
                .map(|part| match part {
                    "$vendorDir" => vendor.to_string(),
                    "$baseDir" => base.to_string(),
                    quoted => unquote(quoted),
                })
                .collect();
            (unquote(key), value)
        })
        .collect()
}

fn link(source: &str, target: &str, kind: LinkType, constraint: &str) -> Link {
    let parsed = if constraint == "*" { Constraint::Any } else { parse_constraints(constraint).unwrap() };
    Link { source: source.into(), target: target.into(), constraint: parsed, kind, pretty_constraint: constraint.into() }
}

fn package(name: &str, autoload: Value) -> Package {
    let mut p = Package::new(name, "1.0.0.0", "1.0");
    p.autoload = autoload;
    p
}

fn requiring(mut p: Package, targets: &[&str]) -> Package {
    p.requires = targets.iter().map(|t| link(&p.name, t, LinkType::Require, "*")).collect();
    p
}

fn root(autoload: Value) -> RootPackage {
    root_package(package("root/a", autoload))
}

fn root_package(package: Package) -> RootPackage {
    RootPackage {
        package,
        minimum_stability: Stability::Stable,
        prefer_stable: false,
        stability_flags: vec![],
        references: vec![],
        aliases: vec![],
        config: Map::new(),
        repositories: vec![],
        branch_alias: None,
        raw: Map::new(),
    }
}

fn refs(packages: Vec<Package>) -> Vec<PackageRef> {
    packages.into_iter().map(Arc::new).collect()
}

fn map(entries: &[(&str, String)]) -> BTreeMap<String, String> {
    entries.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
}

#[test]
fn root_package_autoloading() {
    let t = Env::new();
    let root = root(json!({
        "psr-0": {"Main": "src/", "Lala": ["src/", "lib/"]},
        "psr-4": {"Acme\\Fruit\\": "src-fruit/", "Acme\\Cake\\": ["src-cake/", "lib-cake/"]},
        "classmap": ["composersrc/"],
    }));
    t.mkdir("composer");
    t.mkdir("lib");
    t.write("src/Lala/ClassMapMain.php", "<?php namespace Lala; class ClassMapMain {}");
    t.write("src/Lala/Test/ClassMapMainTest.php", "<?php namespace Lala\\Test; class ClassMapMainTest {}");
    t.mkdir("src-fruit");
    t.mkdir("lib-cake");
    t.write("src-cake/ClassMapBar.php", "<?php namespace Acme\\Cake; class ClassMapBar {}");
    t.write("composersrc/foo.php", "<?php class ClassMapFoo {}");

    t.dump(&root, &[], "_1", true);
    t.assert_fixture("autoload_main.php", "composer/autoload_namespaces.php");
    t.assert_fixture("autoload_psr4.php", "composer/autoload_psr4.php");
    t.assert_fixture("autoload_classmap.php", "composer/autoload_classmap.php");
}

#[test]
fn root_package_dev_autoloading() {
    let t = Env::new();
    let mut root = root(json!({"psr-0": {"Main": "src/"}}));
    root.package.autoload_dev = json!({"files": ["devfiles/foo.php"], "psr-0": {"Main": "tests/"}});
    t.write("src/Main/ClassMain.php", "<?php namespace Main; class ClassMain {}");
    t.write("devfiles/foo.php", "<?php function foo() { echo \"foo\"; }");

    t.dump_with(&root, &[], AutoloadOptions { dev_mode: Some(true), suffix: Some("_1".into()), optimize: true, ..Default::default() });
    t.assert_fixture("autoload_main5.php", "composer/autoload_namespaces.php");
    t.assert_fixture("autoload_classmap7.php", "composer/autoload_classmap.php");
    t.assert_fixture("autoload_files2.php", "composer/autoload_files.php");
}

#[test]
fn root_package_dev_autoloading_disabled_by_default() {
    let t = Env::new();
    let mut root = root(json!({"psr-0": {"Main": "src/"}}));
    root.package.autoload_dev = json!({"files": ["devfiles/foo.php"]});
    t.write("src/Main/ClassMain.php", "<?php namespace Main; class ClassMain {}");
    t.write("devfiles/foo.php", "<?php function foo() { echo \"foo\"; }");

    // no installed.json: no-dev
    t.dump_with(&root, &[], AutoloadOptions { suffix: Some("_1".into()), optimize: true, ..Default::default() });
    t.assert_fixture("autoload_main4.php", "composer/autoload_namespaces.php");
    t.assert_fixture("autoload_classmap7.php", "composer/autoload_classmap.php");
    assert!(!t.exists("composer/autoload_files.php"));
}

#[test]
fn vendor_dir_same_as_working_dir() {
    let mut t = Env::new();
    t.vendor_dir = t.working_dir.clone();
    let root = root(json!({
        "psr-0": {"Main": "src/", "Lala": "src/"},
        "psr-4": {"Acme\\Fruit\\": "src-fruit/", "Acme\\Cake\\": ["src-cake/", "lib-cake/"]},
        "classmap": ["composersrc/"],
    }));
    t.write("src/Main/Foo.php", "<?php namespace Main; class Foo {}");
    t.write("composersrc/foo.php", "<?php class ClassMapFoo {}");

    t.dump(&root, &[], "_2", true);
    t.assert_fixture("autoload_main3.php", "composer/autoload_namespaces.php");
    t.assert_fixture("autoload_psr4_3.php", "composer/autoload_psr4.php");
    t.assert_fixture("autoload_classmap3.php", "composer/autoload_classmap.php");
}

#[test]
fn root_package_autoloading_alternative_vendor_dir() {
    let mut t = Env::new();
    t.vendor_dir = t.vendor_dir.join("subdir");
    let root = root(json!({
        "psr-0": {"Main": "src/", "Lala": "src/"},
        "psr-4": {"Acme\\Fruit\\": "src-fruit/", "Acme\\Cake\\": ["src-cake/", "lib-cake/"]},
        "classmap": ["composersrc/"],
    }));
    t.mkdir("src");
    t.write("composersrc/foo.php", "<?php class ClassMapFoo {}");

    t.dump(&root, &[], "_3", false);
    t.assert_fixture("autoload_main2.php", "composer/autoload_namespaces.php");
    t.assert_fixture("autoload_psr4_2.php", "composer/autoload_psr4.php");
    t.assert_fixture("autoload_classmap2.php", "composer/autoload_classmap.php");
}

#[test]
fn root_package_autoloading_with_target_dir() {
    let t = Env::new();
    let mut root = root(json!({
        "psr-0": {"Main\\Foo": "", "Main\\Bar": ""},
        "classmap": ["Main/Foo/src", "lib"],
        "files": ["foo.php", "Main/Foo/bar.php"],
    }));
    root.package.target_dir = Some("Main/Foo/".into());
    t.write("src/rootfoo.php", "<?php class ClassMapFoo {}");
    t.write("lib/rootbar.php", "<?php class ClassMapBar {}");
    t.write("foo.php", "<?php class FilesFoo {}");
    t.write("bar.php", "<?php class FilesBar {}");

    t.dump(&root, &[], "TargetDir", false);
    t.assert_fixture("autoload_target_dir.php", "autoload.php");
    t.assert_fixture("autoload_real_target_dir.php", "composer/autoload_real.php");
    t.assert_fixture("autoload_static_target_dir.php", "composer/autoload_static.php");
    t.assert_fixture("autoload_files_target_dir.php", "composer/autoload_files.php");
    t.assert_fixture("autoload_classmap6.php", "composer/autoload_classmap.php");
}

#[test]
fn duplicate_files() {
    let t = Env::new();
    let root = root(json!({"files": ["foo.php", "bar.php", "./foo.php", "././foo.php"]}));
    t.write("foo.php", "<?php class FilesFoo {}");
    t.write("bar.php", "<?php class FilesBar {}");

    t.dump(&root, &[], "FilesWarning", false);
    t.assert_fixture("autoload_files_duplicates.php", "composer/autoload_files.php");
}

#[test]
fn vendors_autoloading() {
    let t = Env::new();
    let root_pkg = root_package(requiring(package("root/a", json!([])), &["a/a", "b/b"]));
    let a = package("a/a", json!({"psr-0": {"A": "src/", "A\\B": "lib/"}}));
    let b = Arc::new(package("b/b", json!({"psr-0": {"B\\Sub\\Name": "src/"}})));
    let alias = Arc::new(Package::alias(&b, "1.2.0.0", "1.2"));
    t.mkdir("composer-test-autoload/a/a/src");
    t.mkdir("composer-test-autoload/a/a/lib");
    t.mkdir("composer-test-autoload/b/b/src");

    t.dump(&root_pkg, &[Arc::new(a), b, alias], "_5", false);
    t.assert_fixture("autoload_vendors.php", "composer/autoload_namespaces.php");
    assert!(t.exists("composer/autoload_classmap.php"), "ClassMap file needs to be generated, even if empty.");
}

#[test]
fn vendors_autoloading_with_metapackages() {
    let t = Env::new();
    let root_pkg = root_package(requiring(package("root/a", json!([])), &["a/a"]));
    let mut a = requiring(package("a/a", json!({"psr-0": {"A": "src/", "A\\B": "lib/"}})), &["b/b"]);
    a.package_type = "metapackage".into();
    let b = Arc::new(package("b/b", json!({"psr-0": {"B\\Sub\\Name": "src/"}})));
    let alias = Arc::new(Package::alias(&b, "1.2.0.0", "1.2"));
    t.mkdir("composer-test-autoload/b/b/src");
    // would be found if the metapackage was not excluded
    t.mkdir("composer-test-autoload/a/a/src");
    t.mkdir("composer-test-autoload/a/a/lib");

    t.dump(&root_pkg, &[Arc::new(a), b, alias], "_5", false);
    t.assert_fixture("autoload_vendors_meta.php", "composer/autoload_namespaces.php");
    assert!(t.exists("composer/autoload_classmap.php"));
}

#[test]
fn non_dev_autoload_exclusion_with_recursion() {
    let t = Env::new();
    let root_pkg = root_package(requiring(package("root/a", json!([])), &["a/a"]));
    let a = requiring(package("a/a", json!({"psr-0": {"A": "src/", "A\\B": "lib/"}})), &["b/b"]);
    let b = requiring(package("b/b", json!({"psr-0": {"B\\Sub\\Name": "src/"}})), &["a/a"]);
    t.mkdir("composer-test-autoload/a/a/src");
    t.mkdir("composer-test-autoload/a/a/lib");
    t.mkdir("composer-test-autoload/b/b/src");

    t.dump(&root_pkg, &refs(vec![a, b]), "_5", false);
    t.assert_fixture("autoload_vendors.php", "composer/autoload_namespaces.php");
}

#[test]
fn non_dev_autoload_should_include_replaced_packages() {
    let t = Env::new();
    let root_pkg = root_package(requiring(package("root/a", json!([])), &["a/a"]));
    let a = requiring(package("a/a", json!([])), &["b/c"]);
    let mut b = package("b/b", json!({"psr-4": {"B\\": "src/"}}));
    b.replaces = vec![link("b/b", "b/c", LinkType::Replace, "==1.0")];
    t.write("composer-test-autoload/b/b/src/C/C.php", "<?php namespace B\\C; class C {}");

    t.dump(&root_pkg, &refs(vec![a, b]), "_5", true);
    let vendor = t.vendor_dir.display();
    assert_eq!(
        t.classmap(),
        map(&[("B\\C\\C", format!("{vendor}/b/b/src/C/C.php")), ("Composer\\InstalledVersions", format!("{vendor}/composer/InstalledVersions.php"))])
    );
}

#[test]
fn non_dev_autoload_exclusion_with_recursion_replace() {
    let t = Env::new();
    let root_pkg = root_package(requiring(package("root/a", json!([])), &["a/a"]));
    let a = requiring(package("a/a", json!({"psr-0": {"A": "src/", "A\\B": "lib/"}})), &["c/c"]);
    let mut b = package("b/b", json!({"psr-0": {"B\\Sub\\Name": "src/"}}));
    b.replaces = vec![link("b/b", "c/c", LinkType::Replace, "*")];
    t.mkdir("composer-test-autoload/a/a/src");
    t.mkdir("composer-test-autoload/a/a/lib");
    t.mkdir("composer-test-autoload/b/b/src");

    t.dump(&root_pkg, &refs(vec![a, b]), "_5", false);
    t.assert_fixture("autoload_vendors.php", "composer/autoload_namespaces.php");
}

#[test]
fn non_dev_autoload_replaces_nested_requirements() {
    let t = Env::new();
    let root_pkg = root_package(requiring(package("root/a", json!([])), &["a/a"]));
    let a = requiring(package("a/a", json!({"classmap": ["src/A.php"]})), &["b/b"]);
    let b = requiring(package("b/b", json!({"classmap": ["src/B.php"]})), &["e/e"]);
    let mut c = requiring(package("c/c", json!({"classmap": ["src/C.php"]})), &["d/d"]);
    c.replaces = vec![link("c/c", "b/b", LinkType::Replace, "*")];
    let d = package("d/d", json!({"classmap": ["src/D.php"]}));
    let e = package("e/e", json!({"classmap": ["src/E.php"]}));
    for name in ["A", "B", "C", "D", "E"] {
        let lower = name.to_lowercase();
        t.write(format!("composer-test-autoload/{lower}/{lower}/src/{name}.php"), &format!("<?php class {name} {{}}"));
    }

    t.dump(&root_pkg, &refs(vec![a, b, c, d, e]), "_5", false);
    t.assert_fixture("autoload_classmap9.php", "composer/autoload_classmap.php");
}

#[test]
fn phar_autoload() {
    let t = Env::new();
    let root_pkg = root_package(requiring(
        package(
            "root/a",
            json!({
                "psr-0": {"Foo": "foo.phar", "Bar": "dir/bar.phar/src"},
                "psr-4": {"Baz\\": "baz.phar", "Qux\\": "dir/qux.phar/src"},
            }),
        ),
        &["a/a"],
    ));
    let vendor_package = package(
        "a/a",
        json!({
            "psr-0": {"Lorem": "lorem.phar", "Ipsum": "dir/ipsum.phar/src"},
            "psr-4": {"Dolor\\": "dolor.phar", "Sit\\": "dir/sit.phar/src"},
        }),
    );

    t.dump(&root_pkg, &refs(vec![vendor_package]), "Phar", true);
    t.assert_fixture("autoload_phar.php", "composer/autoload_namespaces.php");
    t.assert_fixture("autoload_phar_psr4.php", "composer/autoload_psr4.php");
    t.assert_fixture("autoload_phar_static.php", "composer/autoload_static.php");
}

#[test]
fn psr_to_classmap_ignores_non_existing_dir() {
    let t = Env::new();
    let root = root(json!({"psr-0": {"Prefix": "foo/bar/non/existing/"}, "psr-4": {"Prefix\\": "foo/bar/non/existing2/"}}));
    t.dump(&root, &[], "_8", true);
    assert_eq!(t.classmap(), map(&[("Composer\\InstalledVersions", format!("{}/composer/InstalledVersions.php", t.vendor_dir.display()))]));
}

#[test]
fn psr_to_classmap_ignores_non_psr_classes() {
    let t = Env::new();
    let root = root(json!({"psr-0": {"psr0_": "psr0/"}, "psr-4": {"psr4\\": "psr4/"}}));
    t.write("psr0/psr0/match.php", "<?php class psr0_match {}");
    t.write("psr0/psr0/badfile.php", "<?php class psr0_badclass {}");
    t.write("psr4/match.php", "<?php namespace psr4; class match {}");
    t.write("psr4/badfile.php", "<?php namespace psr4; class badclass {}");

    let class_map = t.dump(&root, &[], "_1", true);
    let expected = r#"<?php

// autoload_classmap.php @generated by Composer

$vendorDir = dirname(__DIR__);
$baseDir = dirname($vendorDir);

return array(
    'Composer\\InstalledVersions' => $vendorDir . '/composer/InstalledVersions.php',
    'psr0_match' => $baseDir . '/psr0/psr0/match.php',
    'psr4\\match' => $baseDir . '/psr4/match.php',
);
"#;
    assert_eq!(t.read("composer/autoload_classmap.php"), expected);
    let mut violations = class_map.psr_violations();
    violations.sort();
    assert_eq!(
        violations,
        [
            "Class psr0_badclass located in ./psr0/psr0/badfile.php does not comply with psr-0 autoloading standard (rule: psr0_ => ./psr0). Skipping.",
            "Class psr4\\badclass located in ./psr4/badfile.php does not comply with psr-4 autoloading standard (rule: psr4\\ => ./psr4). Skipping.",
        ]
    );
}

#[test]
fn vendors_class_map_autoloading() {
    let t = Env::new();
    let root_pkg = root_package(requiring(package("root/a", json!([])), &["a/a", "b/b"]));
    let a = package("a/a", json!({"classmap": ["src/"]}));
    let b = package("b/b", json!({"classmap": ["src/", "lib/"]}));
    t.write("composer-test-autoload/a/a/src/a.php", "<?php class ClassMapFoo {}");
    t.write("composer-test-autoload/b/b/src/b.php", "<?php class ClassMapBar {}");
    t.write("composer-test-autoload/b/b/lib/c.php", "<?php class ClassMapBaz {}");

    t.dump(&root_pkg, &refs(vec![a, b]), "_6", false);
    let v = t.vendor_dir.display();
    assert_eq!(
        t.classmap(),
        map(&[
            ("ClassMapBar", format!("{v}/b/b/src/b.php")),
            ("ClassMapBaz", format!("{v}/b/b/lib/c.php")),
            ("ClassMapFoo", format!("{v}/a/a/src/a.php")),
            ("Composer\\InstalledVersions", format!("{v}/composer/InstalledVersions.php")),
        ])
    );
    t.assert_fixture("autoload_classmap4.php", "composer/autoload_classmap.php");
}

/// Composer 2.10.3 does not exclude the vendor dir from root classmap scans (master does).
#[test]
fn root_class_map_scans_vendor_dir() {
    let t = Env::new();
    let root_pkg = root_package(requiring(package("root/a", json!({"classmap": ["."]})), &["b/b"]));
    t.write("src/RootClass.php", "<?php class RootClass {}");
    t.write("composer-test-autoload/b/b/src/VendorClass.php", "<?php class VendorClass {}");

    t.dump(&root_pkg, &refs(vec![package("b/b", json!([]))]), "_12867", true);
    let (v, w) = (t.vendor_dir.display(), t.working_dir.display());
    assert_eq!(
        t.classmap(),
        map(&[
            ("Composer\\InstalledVersions", format!("{v}/composer/InstalledVersions.php")),
            ("RootClass", format!("{w}/src/RootClass.php")),
            ("VendorClass", format!("{v}/b/b/src/VendorClass.php")),
        ])
    );
}

#[test]
fn vendors_class_map_autoloading_with_target_dir() {
    let t = Env::new();
    let root_pkg = root_package(requiring(package("root/a", json!([])), &["a/a", "b/b"]));
    let mut a = package("a/a", json!({"classmap": ["target/src/", "lib/"]}));
    a.target_dir = Some("target".into());
    let b = package("b/b", json!({"classmap": ["src/"]}));
    t.write("composer-test-autoload/a/a/target/src/a.php", "<?php class ClassMapFoo {}");
    t.write("composer-test-autoload/a/a/target/lib/b.php", "<?php class ClassMapBar {}");
    t.write("composer-test-autoload/b/b/src/c.php", "<?php class ClassMapBaz {}");

    t.dump(&root_pkg, &refs(vec![a, b]), "_6", false);
    let v = t.vendor_dir.display();
    assert_eq!(
        t.classmap(),
        map(&[
            ("ClassMapBar", format!("{v}/a/a/target/lib/b.php")),
            ("ClassMapBaz", format!("{v}/b/b/src/c.php")),
            ("ClassMapFoo", format!("{v}/a/a/target/src/a.php")),
            ("Composer\\InstalledVersions", format!("{v}/composer/InstalledVersions.php")),
        ])
    );
}

fn three_packages(t: &Env, autoloads: [Value; 3], files: [(&str, &str); 3]) -> (RootPackage, Vec<PackageRef>) {
    let root_pkg = root_package(requiring(package("root/a", json!([])), &["a/a", "b/b", "c/c"]));
    let [a, b, c] = autoloads;
    for (path, class) in files {
        t.write(format!("composer-test-autoload/{path}"), &format!("<?php class {class} {{}}"));
    }
    (root_pkg, refs(vec![package("a/a", a), package("b/b", b), package("c/c", c)]))
}

#[test]
fn class_map_autoloading_empty_dir_and_exact_file() {
    let t = Env::new();
    let (root_pkg, packages) = three_packages(
        &t,
        [json!({"classmap": [""]}), json!({"classmap": ["test.php"]}), json!({"classmap": ["./"]})],
        [("a/a/src/a.php", "ClassMapFoo"), ("b/b/test.php", "ClassMapBar"), ("c/c/foo/test.php", "ClassMapBaz")],
    );
    t.dump(&root_pkg, &packages, "_7", false);
    let v = t.vendor_dir.display();
    assert_eq!(
        t.classmap(),
        map(&[
            ("ClassMapBar", format!("{v}/b/b/test.php")),
            ("ClassMapBaz", format!("{v}/c/c/foo/test.php")),
            ("ClassMapFoo", format!("{v}/a/a/src/a.php")),
            ("Composer\\InstalledVersions", format!("{v}/composer/InstalledVersions.php")),
        ])
    );
    t.assert_fixture("autoload_classmap5.php", "composer/autoload_classmap.php");
    let real = t.read("composer/autoload_real.php");
    assert!(!real.contains("$loader->setClassMapAuthoritative(true);"));
    assert!(!real.contains("$loader->setApcuPrefix("));
}

#[test]
fn class_map_autoloading_authoritative_and_apcu() {
    for prefix in [None, Some("custom'Prefix")] {
        let t = Env::new();
        let (root_pkg, packages) = three_packages(
            &t,
            [json!({"psr-4": {"": "src/"}}), json!({"psr-4": {"": "./"}}), json!({"psr-4": {"": "foo/"}})],
            [("a/a/src/ClassMapFoo.php", "ClassMapFoo"), ("b/b/ClassMapBar.php", "ClassMapBar"), ("c/c/foo/ClassMapBaz.php", "ClassMapBaz")],
        );
        let opts = AutoloadOptions {
            classmap_authoritative: true,
            apcu: true,
            apcu_prefix: prefix.map(String::from),
            suffix: Some("_7".into()),
            dev_mode: Some(false),
            ..Default::default()
        };
        t.dump_with(&root_pkg, &packages, opts);
        let v = t.vendor_dir.display();
        assert_eq!(
            t.classmap(),
            map(&[
                ("ClassMapBar", format!("{v}/b/b/ClassMapBar.php")),
                ("ClassMapBaz", format!("{v}/c/c/foo/ClassMapBaz.php")),
                ("ClassMapFoo", format!("{v}/a/a/src/ClassMapFoo.php")),
                ("Composer\\InstalledVersions", format!("{v}/composer/InstalledVersions.php")),
            ])
        );
        t.assert_fixture("autoload_classmap8.php", "composer/autoload_classmap.php");
        let real = t.read("composer/autoload_real.php");
        assert!(real.contains("$loader->setClassMapAuthoritative(true);"));
        assert!(real.contains(if prefix.is_some() { "$loader->setApcuPrefix('custom\\'Prefix');" } else { "$loader->setApcuPrefix(" }));
    }
}

fn files_packages(t: &Env) -> Vec<Package> {
    let a = package("a/a", json!({"files": ["test.php"]}));
    let b = package("b/b", json!({"files": ["test2.php"]}));
    let mut c = package("c/c", json!({"files": ["test3.php", "foo/bar/test4.php"]}));
    c.target_dir = Some("foo/bar".into());
    t.write("composer-test-autoload/a/a/test.php", "<?php function testFilesAutoloadGeneration1() {}");
    t.write("composer-test-autoload/b/b/test2.php", "<?php function testFilesAutoloadGeneration2() {}");
    t.write("composer-test-autoload/c/c/foo/bar/test3.php", "<?php function testFilesAutoloadGeneration3() {}");
    t.write("composer-test-autoload/c/c/foo/bar/test4.php", "<?php function testFilesAutoloadGeneration4() {}");
    t.write("root.php", "<?php function testFilesAutoloadGenerationRoot() {}");
    vec![a, b, c]
}

#[test]
fn files_autoload_generation() {
    let t = Env::new();
    let root_pkg = root_package(requiring(package("root/a", json!({"files": ["root.php"]})), &["a/a", "b/b", "c/c"]));
    let packages = refs(files_packages(&t));

    t.dump(&root_pkg, &packages, "FilesAutoload", false);
    t.assert_fixture("autoload_functions.php", "autoload.php");
    t.assert_fixture("autoload_real_functions.php", "composer/autoload_real.php");
    t.assert_fixture("autoload_static_functions.php", "composer/autoload_static.php");
    t.assert_fixture("autoload_files_functions.php", "composer/autoload_files.php");
}

#[test]
fn files_autoload_generation_remove_extra_entities_from_autoload_files() {
    let t = Env::new();
    let mut autoload_root = requiring(package("root/a", json!({"files": ["root.php"]})), &["a/a", "b/b", "c/c"]);
    autoload_root.include_path = json!(["/lib", "/src"]);
    let autoload_root = root_package(autoload_root);
    let not_autoload_root = root_package(requiring(package("root/a", json!([])), &["a/a", "b/b", "c/c"]));

    let mut packages = files_packages(&t);
    for (p, paths) in packages.iter_mut().zip([json!(["lib1", "src1"]), json!(["lib2"]), json!(["lib3"])]) {
        p.include_path = paths;
    }
    let not_autoload_packages = refs(vec![package("a/a", json!([])), package("b/b", json!([])), package("c/c", json!([]))]);

    t.dump(&autoload_root, &refs(packages), "FilesAutoload", false);
    t.assert_fixture("autoload_functions.php", "autoload.php");
    t.assert_fixture("autoload_real_functions_with_include_paths.php", "composer/autoload_real.php");
    t.assert_fixture("autoload_static_functions_with_include_paths.php", "composer/autoload_static.php");
    t.assert_fixture("autoload_files_functions.php", "composer/autoload_files.php");
    t.assert_fixture("include_paths_functions.php", "composer/include_paths.php");

    t.dump(&autoload_root, &not_autoload_packages, "FilesAutoload", false);
    t.assert_fixture("autoload_functions.php", "autoload.php");
    t.assert_fixture("autoload_real_functions_with_include_paths.php", "composer/autoload_real.php");
    t.assert_fixture("autoload_files_functions_with_removed_extra.php", "composer/autoload_files.php");
    t.assert_fixture("include_paths_functions_with_removed_extra.php", "composer/include_paths.php");

    t.dump(&not_autoload_root, &not_autoload_packages, "FilesAutoload", false);
    t.assert_fixture("autoload_functions.php", "autoload.php");
    t.assert_fixture("autoload_real_functions_with_removed_include_paths_and_autolad_files.php", "composer/autoload_real.php");
    t.assert_fixture("autoload_static_functions_with_removed_include_paths_and_autolad_files.php", "composer/autoload_static.php");
    assert!(!t.exists("composer/autoload_files.php"));
    assert!(!t.exists("composer/include_paths.php"));
}

#[test]
fn files_autoload_order_by_dependencies() {
    let t = Env::new();
    let root_pkg = root_package(requiring(package("root/a", json!({"files": ["root2.php"]})), &["z/foo", "b/bar", "d/d", "e/e"]));
    // c requires nothing, d requires c, b requires c & d, e and z require c (b, e, z ordered alphabetically)
    let z = requiring(package("z/foo", json!({"files": ["testA.php"]})), &["c/lorem"]);
    let b = requiring(package("b/bar", json!({"files": ["testB.php"]})), &["c/lorem", "d/d"]);
    let d = requiring(package("d/d", json!({"files": ["testD.php"]})), &["c/lorem"]);
    let c = package("c/lorem", json!({"files": ["testC.php"]}));
    let e = requiring(package("e/e", json!({"files": ["testE.php"]})), &["c/lorem"]);
    for (path, n) in [("z/foo/testA.php", 1), ("b/bar/testB.php", 2), ("c/lorem/testC.php", 3), ("d/d/testD.php", 4), ("e/e/testE.php", 5)] {
        t.write(format!("composer-test-autoload/{path}"), &format!("<?php function testFilesAutoloadOrderByDependency{n}() {{}}"));
    }
    t.write("root2.php", "<?php function testFilesAutoloadOrderByDependencyRoot() {}");

    t.dump(&root_pkg, &refs(vec![z, b, d, c, e]), "FilesAutoloadOrder", false);
    t.assert_fixture("autoload_functions_by_dependency.php", "autoload.php");
    t.assert_fixture("autoload_real_files_by_dependency.php", "composer/autoload_real.php");
    t.assert_fixture("autoload_static_files_by_dependency.php", "composer/autoload_static.php");
}

/// The root package has priority over other packages, longer namespaces over shorter ones.
#[test]
fn override_vendors_autoloading() {
    let t = Env::new();
    let w = t.working_dir.display().to_string();
    let root_pkg =
        root_package(requiring(package("root/z", json!({"psr-0": {"A\\B": format!("{w}/lib")}, "classmap": [format!("{w}/src")]})), &["a/a", "b/b"]));
    let a = package("a/a", json!({"psr-0": {"A": "src/", "A\\B": "lib/"}, "classmap": ["classmap"]}));
    let b = package("b/b", json!({"psr-0": {"B\\Sub\\Name": "src/"}}));
    for dir in ["src", "composer-test-autoload/a/a/src", "composer-test-autoload/b/b/src"] {
        t.mkdir(dir);
    }
    t.write("lib/A/B/C.php", "<?php namespace A\\B; class C {}");
    t.write("src/classes.php", "<?php namespace Foo; class Bar {}");
    t.write("composer-test-autoload/a/a/lib/A/B/C.php", "<?php namespace A\\B; class C {}");
    t.write("composer-test-autoload/a/a/classmap/classes.php", "<?php namespace Foo; class Bar {}");

    t.dump(&root_pkg, &refs(vec![a, b]), "_9", true);
    let head = "<?php\n\n// %s @generated by Composer\n\n$vendorDir = dirname(__DIR__);\n$baseDir = dirname($vendorDir);\n\nreturn array(\n";
    assert_eq!(
        t.read("composer/autoload_namespaces.php"),
        head.replace("%s", "autoload_namespaces.php")
            + "    'B\\\\Sub\\\\Name' => array($vendorDir . '/b/b/src'),\n    'A\\\\B' => array($baseDir . '/lib', $vendorDir . '/a/a/lib'),\n    'A' => array($vendorDir . '/a/a/src'),\n);\n"
    );
    assert_eq!(t.read("composer/autoload_psr4.php"), head.replace("%s", "autoload_psr4.php") + ");\n");
    assert_eq!(
        t.read("composer/autoload_classmap.php"),
        head.replace("%s", "autoload_classmap.php")
            + "    'A\\\\B\\\\C' => $baseDir . '/lib/A/B/C.php',\n    'Composer\\\\InstalledVersions' => $vendorDir . '/composer/InstalledVersions.php',\n    'Foo\\\\Bar' => $baseDir . '/src/classes.php',\n);\n"
    );
}

#[test]
fn include_path_file_generation() {
    let t = Env::new();
    let mut a = package("a/a", json!([]));
    a.include_path = json!(["lib/"]);
    let mut b = package("b/b", json!([]));
    b.include_path = json!(["library"]);
    let mut c = package("c", json!([]));
    c.include_path = json!(["library"]);

    t.dump(&root(json!([])), &refs(vec![a, b, c]), "_10", false);
    t.assert_fixture("include_paths.php", "composer/include_paths.php");
}

#[test]
fn include_paths_in_root_package() {
    let t = Env::new();
    let mut root_pkg = package("root/a", json!([]));
    root_pkg.include_path = json!(["/lib", "/src"]);
    let mut a = package("a/a", json!([]));
    a.include_path = json!(["lib/"]);

    t.dump(&root_package(root_pkg), &refs(vec![a]), "_12", false);
    let include_paths = t.read("composer/include_paths.php");
    assert!(include_paths.ends_with("return array(\n    $baseDir . '/lib',\n    $baseDir . '/src',\n    $vendorDir . '/a/a/lib',\n);\n"));
    assert!(t.read("composer/autoload_real.php").contains("$includePaths = require __DIR__ . '/include_paths.php';"));
}

#[test]
fn include_path_file_without_paths_is_skipped() {
    let t = Env::new();
    t.dump(&root(json!([])), &refs(vec![package("a/a", json!([]))]), "_12", false);
    assert!(!t.exists("composer/include_paths.php"));
}

#[test]
fn use_global_include_path() {
    let mut t = Env::new();
    t.config["use-include-path"] = json!(true);
    let mut root = root(json!({"psr-0": {"Main\\Foo": "", "Main\\Bar": ""}}));
    root.package.target_dir = Some("Main/Foo/".into());

    t.dump(&root, &[], "IncludePath", false);
    t.assert_fixture("autoload_real_include_path.php", "composer/autoload_real.php");
    t.assert_fixture("autoload_static_include_path.php", "composer/autoload_static.php");
}

#[test]
fn vendor_dir_excluded_from_working_dir() {
    let mut t = Env::new();
    let working_dir = t.vendor_dir.join("working-dir");
    t.vendor_dir = working_dir.join("../vendor");
    std::fs::create_dir_all(&working_dir).unwrap();

    let root_pkg = root_package(requiring(
        package("root/a", json!({"psr-0": {"Foo": "src"}, "psr-4": {"Acme\\Foo\\": "src-psr4"}, "classmap": ["classmap"], "files": ["test.php"]})),
        &["b/b"],
    ));
    let vendor_package =
        package("b/b", json!({"psr-0": {"Bar": "lib"}, "psr-4": {"Acme\\Bar\\": "lib-psr4"}, "classmap": ["classmaps"], "files": ["bootstrap.php"]}));
    t.write(working_dir.join("src/Foo/Bar.php"), "<?php namespace Foo; class Bar {}");
    t.write(working_dir.join("classmap/classes.php"), "<?php namespace Foo; class Foo {}");
    t.write(working_dir.join("test.php"), "<?php class Foo {}");
    t.write(t.vendor_dir.join("b/b/lib/Bar/Foo.php"), "<?php namespace Bar; class Foo {}");
    t.write(t.vendor_dir.join("b/b/classmaps/classes.php"), "<?php namespace Bar; class Bar {}");
    t.write(t.vendor_dir.join("b/b/bootstrap.php"), "<?php class Bar {}");

    t.dump_in(
        &working_dir,
        &root_pkg,
        &refs(vec![vendor_package]),
        &AutoloadOptions { suffix: Some("_13".into()), optimize: true, dev_mode: Some(false), ..Default::default() },
    );
    let head =
        "<?php\n\n// %s @generated by Composer\n\n$vendorDir = dirname(__DIR__);\n$baseDir = dirname($vendorDir).'/working-dir';\n\nreturn array(\n";
    assert_eq!(
        t.read("composer/autoload_namespaces.php"),
        head.replace("%s", "autoload_namespaces.php") + "    'Foo' => array($baseDir . '/src'),\n    'Bar' => array($vendorDir . '/b/b/lib'),\n);\n"
    );
    assert_eq!(
        t.read("composer/autoload_psr4.php"),
        head.replace("%s", "autoload_psr4.php")
            + "    'Acme\\\\Foo\\\\' => array($baseDir . '/src-psr4'),\n    'Acme\\\\Bar\\\\' => array($vendorDir . '/b/b/lib-psr4'),\n);\n"
    );
    assert_eq!(
        t.read("composer/autoload_classmap.php"),
        head.replace("%s", "autoload_classmap.php")
            + "    'Bar\\\\Bar' => $vendorDir . '/b/b/classmaps/classes.php',\n    'Bar\\\\Foo' => $vendorDir . '/b/b/lib/Bar/Foo.php',\n    'Composer\\\\InstalledVersions' => $vendorDir . '/composer/InstalledVersions.php',\n    'Foo\\\\Bar' => $baseDir . '/src/Foo/Bar.php',\n    'Foo\\\\Foo' => $baseDir . '/classmap/classes.php',\n);\n"
    );
    let files = t.read("composer/autoload_files.php");
    assert!(files.contains("$vendorDir . '/b/b/bootstrap.php',\n"));
    assert!(files.contains("$baseDir . '/test.php',\n"));
}

#[test]
fn up_level_relative_paths() {
    let t = Env::new();
    let working_dir = t.working_dir.join("working-dir");
    std::fs::create_dir_all(&working_dir).unwrap();
    let root = root(json!({
        "psr-0": {"Foo": "../path/../src"},
        "psr-4": {"Acme\\Foo\\": "../path/../src-psr4"},
        "classmap": ["../classmap", "../classmap2/subdir", "classmap3", "classmap4"],
        "files": ["../test.php"],
        "exclude-from-classmap": ["./../classmap/excluded", "../classmap2", "classmap3/classes.php", "classmap4/*/classes.php"],
    }));
    t.write("src/Foo/Bar.php", "<?php namespace Foo; class Bar {}");
    t.write("classmap/classes.php", "<?php namespace Foo; class Foo {}");
    t.write("classmap/excluded/classes.php", "<?php namespace Foo; class Boo {}");
    t.write("classmap2/subdir/classes.php", "<?php namespace Foo; class Boo2 {}");
    t.write("working-dir/classmap3/classes.php", "<?php namespace Foo; class Boo3 {}");
    t.write("working-dir/classmap4/foo/classes.php", "<?php namespace Foo; class Boo4 {}");
    t.write("test.php", "<?php class Foo {}");

    t.dump_in(&working_dir, &root, &[], &AutoloadOptions { suffix: Some("_14".into()), optimize: true, dev_mode: Some(false), ..Default::default() });
    let head =
        "<?php\n\n// %s @generated by Composer\n\n$vendorDir = dirname(__DIR__);\n$baseDir = dirname($vendorDir).'/working-dir';\n\nreturn array(\n";
    assert_eq!(
        t.read("composer/autoload_namespaces.php"),
        head.replace("%s", "autoload_namespaces.php") + "    'Foo' => array($baseDir . '/../src'),\n);\n"
    );
    assert_eq!(
        t.read("composer/autoload_psr4.php"),
        head.replace("%s", "autoload_psr4.php") + "    'Acme\\\\Foo\\\\' => array($baseDir . '/../src-psr4'),\n);\n"
    );
    assert_eq!(
        t.read("composer/autoload_classmap.php"),
        head.replace("%s", "autoload_classmap.php")
            + "    'Composer\\\\InstalledVersions' => $vendorDir . '/composer/InstalledVersions.php',\n    'Foo\\\\Bar' => $baseDir . '/../src/Foo/Bar.php',\n    'Foo\\\\Foo' => $baseDir . '/../classmap/classes.php',\n);\n"
    );
    assert!(t.read("composer/autoload_files.php").contains("$baseDir . '/../test.php',\n"));
}

#[test]
fn autoload_rules_in_package_that_does_not_exist_on_disk() {
    let t = Env::new();
    let root_pkg = root_package(requiring(package("root/a", json!([])), &["dep/a"]));
    let dep = |autoload: Value| refs(vec![package("dep/a", autoload)]);
    let head = "<?php\n\n// %s @generated by Composer\n\n$vendorDir = dirname(__DIR__);\n$baseDir = dirname($vendorDir);\n\nreturn array(\n";

    t.dump(&root_pkg, &dep(json!({"psr-0": {"Foo": "./src"}})), "_19", true);
    assert_eq!(
        t.read("composer/autoload_namespaces.php"),
        head.replace("%s", "autoload_namespaces.php") + "    'Foo' => array($vendorDir . '/dep/a/src'),\n);\n"
    );

    t.dump(&root_pkg, &dep(json!({"psr-4": {"Acme\\Foo\\": "./src-psr4"}})), "_19", true);
    assert_eq!(
        t.read("composer/autoload_psr4.php"),
        head.replace("%s", "autoload_psr4.php") + "    'Acme\\\\Foo\\\\' => array($vendorDir . '/dep/a/src-psr4'),\n);\n"
    );

    let mut config = Config::new(false, Some(&t.working_dir));
    config.merge_config(&json!({"vendor-dir": t.vendor_dir.to_string_lossy()}));
    let packages = vec![(dep(json!({"classmap": ["classmap"]})).remove(0), t.install_path(&package("dep/a", json!([]))))];
    let err = dump(&config, &root_pkg, &packages, &t.working_dir, &AutoloadOptions::default(), &Io::null()).unwrap_err();
    assert_eq!(
        err.to_string(),
        format!("Could not scan for classes inside \"{}/dep/a/classmap\" which does not appear to be a file nor a folder", t.vendor_dir.display())
    );

    t.dump(&root_pkg, &dep(json!({"files": ["./test.php"]})), "_19", true);
    assert!(t.read("composer/autoload_files.php").contains("$vendorDir . '/dep/a/test.php',\n"));
}

#[test]
fn empty_paths() {
    let t = Env::new();
    let root = root(json!({"psr-0": {"Foo": ""}, "psr-4": {"Acme\\Foo\\": ""}, "classmap": [""]}));
    t.write("Foo/Bar.php", "<?php namespace Foo; class Bar {}");
    t.write("class.php", "<?php namespace Classmap; class Foo {}");

    t.dump(&root, &[], "_15", true);
    let head = "<?php\n\n// %s @generated by Composer\n\n$vendorDir = dirname(__DIR__);\n$baseDir = dirname($vendorDir);\n\nreturn array(\n";
    assert_eq!(
        t.read("composer/autoload_namespaces.php"),
        head.replace("%s", "autoload_namespaces.php") + "    'Foo' => array($baseDir . '/'),\n);\n"
    );
    assert_eq!(
        t.read("composer/autoload_psr4.php"),
        head.replace("%s", "autoload_psr4.php") + "    'Acme\\\\Foo\\\\' => array($baseDir . '/'),\n);\n"
    );
    assert_eq!(
        t.read("composer/autoload_classmap.php"),
        head.replace("%s", "autoload_classmap.php")
            + "    'Classmap\\\\Foo' => $baseDir . '/class.php',\n    'Composer\\\\InstalledVersions' => $vendorDir . '/composer/InstalledVersions.php',\n    'Foo\\\\Bar' => $baseDir . '/Foo/Bar.php',\n);\n"
    );
}

#[test]
fn vendor_substring_path() {
    let t = Env::new();
    let root = root(json!({"psr-0": {"Foo": "composer-test-autoload-src/src"}, "psr-4": {"Acme\\Foo\\": "composer-test-autoload-src/src-psr4"}}));
    t.dump(&root, &[], "VendorSubstring", false);
    let head = "<?php\n\n// %s @generated by Composer\n\n$vendorDir = dirname(__DIR__);\n$baseDir = dirname($vendorDir);\n\nreturn array(\n";
    assert_eq!(
        t.read("composer/autoload_namespaces.php"),
        head.replace("%s", "autoload_namespaces.php") + "    'Foo' => array($baseDir . '/composer-test-autoload-src/src'),\n);\n"
    );
    assert_eq!(
        t.read("composer/autoload_psr4.php"),
        head.replace("%s", "autoload_psr4.php") + "    'Acme\\\\Foo\\\\' => array($baseDir . '/composer-test-autoload-src/src-psr4'),\n);\n"
    );
}

#[test]
fn exclude_from_classmap() {
    let t = Env::new();
    let root = root(json!({
        "psr-0": {"Main": "src/", "Lala": ["src/", "lib/"]},
        "psr-4": {"Acme\\Fruit\\": "src-fruit/", "Acme\\Cake\\": ["src-cake/", "lib-cake/"]},
        "classmap": ["composersrc/"],
        "exclude-from-classmap": [
            "/composersrc/foo/bar/",
            "/composersrc/excludedTests/",
            "/composersrc/ClassToExclude.php",
            "/composersrc/*/excluded/excsubpath",
            "**/excsubpath",
            // should not exclude /composersrc/** (equivalent to /composers/**) or /src-cake/**
            "composers",
            "/src-ca/",
        ],
    }));
    t.mkdir("composer");
    t.mkdir("lib");
    t.mkdir("src-fruit");
    t.mkdir("lib-cake");
    t.mkdir("composersrc/tests");
    t.write("src/Lala/ClassMapMain.php", "<?php namespace Lala; class ClassMapMain {}");
    t.write("src/Lala/Test/ClassMapMainTest.php", "<?php namespace Lala\\Test; class ClassMapMainTest {}");
    t.write("src-cake/ClassMapBar.php", "<?php namespace Acme\\Cake; class ClassMapBar {}");
    t.write("composersrc/foo.php", "<?php class ClassMapFoo {}");
    // these classes should not be found in the classmap
    t.write("composersrc/excludedTests/bar.php", "<?php class ClassExcludeMapFoo {}");
    t.write("composersrc/ClassToExclude.php", "<?php class ClassClassToExclude {}");
    t.write("composersrc/long/excluded/excsubpath/foo.php", "<?php class ClassExcludeMapFoo2 {}");
    t.write("composersrc/long/excluded/excsubpath/bar.php", "<?php class ClassExcludeMapBar {}");
    // symlinked directory in the project directory in classmap
    t.write("forks/bar/src/exclude/FooExclClass.php", "<?php class FooExclClass {};");
    t.mkdir("composersrc/foo");
    std::os::unix::fs::symlink(t.working_dir.join("forks/bar/"), t.working_dir.join("composersrc/foo/bar")).unwrap();

    t.dump(&root, &[], "_1", true);
    t.assert_fixture("autoload_classmap.php", "composer/autoload_classmap.php");
}

#[test]
fn generates_platform_check() {
    type Case = (
        &'static [(&'static str, &'static str)],
        Option<&'static str>,
        &'static [(&'static str, &'static str)],
        &'static [(&'static str, &'static str)],
        Option<&'static [&'static str]>,
    );
    let cases: [(&str, Case); 12] = [
        ("Typical project requirements", (&[("php", "^7.2"), ("ext-xml", "*"), ("ext-json", "*")], Some("typical"), &[], &[], None)),
        ("No PHP lower bound", (&[("php", "< 8")], None, &[], &[], None)),
        ("No PHP upper bound", (&[("php", ">= 7.2")], Some("no_php_upper_bound"), &[], &[], None)),
        ("Specific PHP release version", (&[("php", "^7.2.8")], Some("specific_php_release"), &[], &[], None)),
        ("Specific 64-bit PHP version", (&[("php-64bit", "^7.2.8")], Some("specific_php_64bit_required"), &[], &[], None)),
        ("64-bit PHP required", (&[("php-64bit", "*")], Some("php_64bit_required"), &[], &[], None)),
        ("No PHP required", (&[("ext-xml", "*"), ("ext-json", "*")], Some("no_php_required"), &[], &[], None)),
        (
            "Ignoring all platform requirements skips check completely",
            (&[("php", "^7.2"), ("ext-xml", "*"), ("ext-json", "*")], None, &[], &[], Some(&[])),
        ),
        (
            "Ignored platform requirements are not checked for",
            (
                &[("php", "^7.2.8"), ("ext-xml", "*"), ("ext-json", "*"), ("ext-pdo", "*")],
                Some("no_php_required"),
                &[],
                &[],
                Some(&["php", "ext-pdo"]),
            ),
        ),
        (
            "Via wildcard ignored platform requirements are not checked for",
            (
                &[("php", "^7.2.8"), ("ext-xml", "*"), ("ext-json", "*"), ("ext-fileinfo", "*"), ("ext-filesystem", "*"), ("ext-filter", "*")],
                Some("no_php_required"),
                &[],
                &[],
                Some(&["php", "ext-fil*"]),
            ),
        ),
        ("No extensions required", (&[("php", "^7.2")], Some("no_extensions_required"), &[], &[], None)),
        (
            "Replaced/provided extensions are not checked for + checking case insensitivity",
            // ext-pdo 7.1.* does not satisfy ^7.2 so it is still checked; bcmath and xml are covered
            (
                &[("ext-xml", "^7.2"), ("ext-Pdo", "^7.2"), ("ext-bcMath", "^7.2")],
                Some("replaced_provided_exts"),
                &[("ext-PDO", "7.1.*"), ("ext-BCMath", "^7.1")],
                &[("ext-XML", "*")],
                None,
            ),
        ),
    ];
    for (name, (requires, fixture, provides, replaces, ignore)) in cases {
        let t = Env::new();
        let links = |kind, list: &[(&str, &str)]| list.iter().map(|(target, c)| link("a", &target.to_lowercase(), kind, c)).collect::<Vec<_>>();
        let mut root_pkg = package("root/a", json!([]));
        root_pkg.requires = links(LinkType::Require, requires);
        root_pkg.provides = links(LinkType::Provide, provides);
        root_pkg.replaces = links(LinkType::Replace, replaces);
        let ignored: Vec<String> = ignore.unwrap_or_default().iter().map(|s| s.to_string()).collect();
        let opts = AutoloadOptions {
            suffix: Some("_1".into()),
            optimize: true,
            dev_mode: Some(false),
            // `Some(&[])` stands for `--ignore-platform-reqs`: everything
            is_platform_req_ignored: Some(Box::new(move |req| {
                ignore.is_some_and(|i| i.is_empty()) || ignored.iter().any(|p| p.strip_suffix('*').map_or(p == req, |prefix| req.starts_with(prefix)))
            })),
            ..Default::default()
        };
        t.dump_with(&root_package(root_pkg), &[], opts);
        let real = t.read("composer/autoload_real.php");
        match fixture {
            None => {
                assert!(!t.exists("composer/platform_check.php"), "{name}");
                assert!(!real.contains("require __DIR__ . '/platform_check.php';"), "{name}");
            }
            Some(fixture) => {
                t.assert_fixture(&format!("platform/{fixture}.php"), "composer/platform_check.php");
                assert!(real.contains("require __DIR__ . '/platform_check.php';"), "{name}");
            }
        }
    }
}

#[test]
fn absolute_symlink_with_psr4_does_not_generate_warnings() {
    let t = Env::new();
    t.write(
        "tools-real/vendor/phpunit/phpunit/src/Framework/Exception/Exception.php",
        "<?php namespace PHPUnit\\Framework; class Exception extends \\Exception {}",
    );
    std::os::unix::fs::symlink(t.working_dir.join("tools-real"), t.working_dir.join("tools")).unwrap();
    let root_pkg = root_package(package("test/package", json!({"psr-4": {"MyTools\\": "tools/"}, "exclude-from-classmap": ["**/vendor/"]})));

    let class_map = t.dump(&root_pkg, &[], "_9", true);
    assert!(class_map.psr_violations().iter().all(|v| !v.contains("does not comply with psr-4 autoloading standard")));
}

#[test]
fn absolute_symlink_with_classmap_exclude_from_classmap() {
    let t = Env::new();
    t.write(
        "tools-real/vendor/phpunit/phpunit/src/Framework/Exception.php",
        "<?php namespace PHPUnit\\Framework; class Exception extends \\Exception {}",
    );
    t.write("tools-real/MyClass.php", "<?php class MyClass {}");
    std::os::unix::fs::symlink(t.working_dir.join("tools-real"), t.working_dir.join("tools")).unwrap();

    let class_map =
        t.dump(&root_package(package("test/package", json!({"classmap": ["tools/"], "exclude-from-classmap": ["**/vendor/"]}))), &[], "_9", false);
    assert!(class_map.map().contains_key("MyClass"));
    assert!(!class_map.map().contains_key("PHPUnit\\Framework\\Exception"));
}

/// The suffix carries over from an existing autoload.php, else comes from the lock hash.
#[test]
fn suffix_resolution() {
    let t = Env::new();
    let opts = |hash: Option<&str>| AutoloadOptions { lock_content_hash: hash.map(String::from), dev_mode: Some(false), ..Default::default() };
    t.dump_with(&root(json!([])), &[], opts(Some("not a hash")));
    let random = t.read("autoload.php");
    let suffix = random.split("ComposerAutoloaderInit").nth(1).unwrap().split("::").next().unwrap().to_string();
    assert_eq!(suffix.len(), 32);
    t.dump_with(&root(json!([])), &[], opts(Some("abc123")));
    assert!(t.read("composer/autoload_real.php").contains(&format!("class ComposerAutoloaderInit{suffix}\n")));
    std::fs::remove_file(t.vendor_dir.join("autoload.php")).unwrap();
    t.dump_with(&root(json!([])), &[], opts(Some("abc123")));
    assert!(t.read("autoload.php").contains("ComposerAutoloaderInitabc123::getLoader()"));
    assert_eq!(t.read("composer/ClassLoader.php"), include_str!("../res/ClassLoader.php"));
}
