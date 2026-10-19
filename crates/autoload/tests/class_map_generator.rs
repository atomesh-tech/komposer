//! Port of composer/class-map-generator 1.7.3's ClassMapGeneratorTest and PhpFileParserTest.

use komposer_autoload::{ClassMapGenerator, Scan, find_classes};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn crate_dir() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn fixtures(sub: &str) -> String {
    format!("{}/tests/fixtures/classmap/{sub}", crate_dir().display())
}

fn generator() -> ClassMapGenerator {
    ClassMapGenerator::new(&["php", "inc", "hh"], crate_dir()).unwrap()
}

/// `ClassMapGenerator::createMap()`
fn create_map(path: &str) -> anyhow::Result<BTreeMap<String, String>> {
    let mut g = ClassMapGenerator::new(&["php", "inc"], crate_dir())?;
    g.scan_paths(path, None, Scan::Classmap, &[])?;
    Ok(g.class_map().map().clone())
}

fn map(dir: &str, entries: &[(&str, &str)]) -> BTreeMap<String, String> {
    entries.iter().map(|(class, file)| (class.to_string(), format!("{}/{file}", fixtures(dir)))).collect()
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("komposer-autoload-cmg-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.canonicalize().unwrap()
}

#[test]
fn create_map_fixtures() {
    let cases = [
        ("Namespaced", map("Namespaced", &[("Namespaced\\Bar", "Bar.inc"), ("Namespaced\\Foo", "Foo.php"), ("Namespaced\\Baz", "Baz.php")])),
        (
            "beta/NamespaceCollision",
            map("beta/NamespaceCollision", &[("NamespaceCollision\\A\\B\\Bar", "A/B/Bar.php"), ("NamespaceCollision\\A\\B\\Foo", "A/B/Foo.php")]),
        ),
        ("Pearlike", map("Pearlike", &[("Pearlike_Foo", "Foo.php"), ("Pearlike_Bar", "Bar.php"), ("Pearlike_Baz", "Baz.php")])),
        (
            "classmap",
            map(
                "classmap",
                &[
                    ("Foo\\Bar\\A", "sameNsMultipleClasses.php"),
                    ("Foo\\Bar\\B", "sameNsMultipleClasses.php"),
                    ("Alpha\\A", "multipleNs.php"),
                    ("Alpha\\B", "multipleNs.php"),
                    ("A", "multipleNs.php"),
                    ("Be\\ta\\A", "multipleNs.php"),
                    ("Be\\ta\\B", "multipleNs.php"),
                    ("ClassMap\\SomeInterface", "SomeInterface.php"),
                    ("ClassMap\\SomeParent", "SomeParent.php"),
                    ("ClassMap\\SomeClass", "SomeClass.php"),
                    ("ClassMap\\LongString", "LongString.php"),
                    ("Foo\\LargeClass", "LargeClass.php"),
                    ("Foo\\LargeGap", "LargeGap.php"),
                    ("Foo\\MissingSpace", "MissingSpace.php"),
                    ("Foo\\StripNoise", "StripNoise.php"),
                    ("Foo\\First", "StripNoise.php"),
                    ("Foo\\Second", "StripNoise.php"),
                    ("Foo\\Third", "StripNoise.php"),
                    ("Foo\\SlashedA", "BackslashLineEndingString.php"),
                    ("Foo\\SlashedB", "BackslashLineEndingString.php"),
                    ("Unicode\\↑\\↑", "Unicode.php"),
                    ("ShortOpenTag", "ShortOpenTag.php"),
                    ("Smarty_Internal_Compile_Block", "InvalidUnicode.php"),
                    ("Smarty_Internal_Compile_Blockclose", "InvalidUnicode.php"),
                    ("ShortOpenTagDocblock", "ShortOpenTagDocblock.php"),
                ],
            ),
        ),
        ("template", BTreeMap::new()),
        (
            "php5.4",
            map(
                "php5.4",
                &[
                    ("TFoo", "traits.php"),
                    ("CFoo", "traits.php"),
                    ("Foo\\TBar", "traits.php"),
                    ("Foo\\IBar", "traits.php"),
                    ("Foo\\TFooBar", "traits.php"),
                    ("Foo\\CBar", "traits.php"),
                ],
            ),
        ),
        ("php7.0", map("php7.0", &[("Dummy\\Test\\AnonClassHolder", "anonclass.php")])),
        (
            "php8.1",
            map(
                "php8.1",
                &[
                    ("RolesBasicEnum", "enum_basic.php"),
                    ("RolesBackedEnum", "enum_backed.php"),
                    ("RolesClassLikeEnum", "enum_class_semantics.php"),
                    ("Foo\\Bar\\RolesClassLikeNamespacedEnum", "enum_namespaced.php"),
                ],
            ),
        ),
    ];
    for (dir, expected) in cases {
        assert_eq!(create_map(&fixtures(dir)).unwrap(), expected, "{dir}");
    }
}

#[test]
fn create_map_does_not_hit_regex_backtrace_limit() {
    let expected = map(
        "pcrebacktracelimit",
        &[
            ("Foo\\StripNoise", "StripNoise.php"),
            ("Foo\\VeryLongHeredoc", "VeryLongHeredoc.php"),
            ("Foo\\ClassAfterLongHereDoc", "VeryLongHeredoc.php"),
            ("Foo\\VeryLongPHP73Heredoc", "VeryLongPHP73Heredoc.php"),
            ("Foo\\VeryLongPHP73Nowdoc", "VeryLongPHP73Nowdoc.php"),
            ("Foo\\ClassAfterLongNowDoc", "VeryLongPHP73Nowdoc.php"),
            ("Foo\\VeryLongNowdoc", "VeryLongNowdoc.php"),
        ],
    );
    assert_eq!(create_map(&fixtures("pcrebacktracelimit")).unwrap(), expected);
}

#[test]
fn ambiguous_reference() {
    let dir = temp_dir("ambiguous");
    std::fs::create_dir(dir.join("other")).unwrap();
    std::fs::write(dir.join("A.php"), "<?php\nclass A {}").unwrap();
    std::fs::write(dir.join("other/A.php"), "<?php\nclass A {}").unwrap();

    let mut g = generator();
    g.scan_paths(dir.to_str().unwrap(), None, Scan::Classmap, &[]).unwrap();
    let ambiguous = g.class_map().ambiguous_classes(true);
    assert_eq!(ambiguous.len(), 1);
    assert_eq!(ambiguous["A"].len(), 1);
    let possible = [format!("{}/A.php", dir.display()), format!("{}/other/A.php", dir.display())];
    assert!(possible.iter().any(|p| p == ambiguous["A"][0]));
    std::fs::remove_dir_all(dir).unwrap();
}

/// A file declaring the same class twice is not ambiguous; test dirs only count when unfiltered.
#[test]
fn unambiguous_reference() {
    let dir = temp_dir("unambiguous");
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/A.php"), "<?php\nclass A {}").unwrap();
    std::fs::write(
        dir.join("src/B.php"),
        "<?php\n                if (true) {\n                    interface B {}\n                } else {\n                    interface B extends Iterator {}\n                }\n            ",
    )
    .unwrap();
    for keyword in ["test", "fixture", "example"] {
        std::fs::create_dir_all(dir.join("ambiguous").join(keyword)).unwrap();
        std::fs::write(dir.join("ambiguous").join(keyword).join("A.php"), "<?php\nclass A {}").unwrap();
    }
    let (src, ambiguous) = (format!("{}/src", dir.display()), format!("{}/ambiguous", dir.display()));

    // if we scan src first, then test ambiguous refs will be ignored correctly
    let mut g = generator();
    g.scan_paths(&src, None, Scan::Classmap, &[]).unwrap();
    g.scan_paths(&ambiguous, None, Scan::Classmap, &[]).unwrap();
    assert_eq!(g.class_map().ambiguous_classes(false).len(), 1);
    assert!(g.class_map().ambiguous_classes(true).is_empty());
    assert_eq!(g.class_map().ambiguous_classes(false)["A"].len(), 3);

    // if we scan tests first however, then we always get ambiguous refs as the test one is overriding src
    let mut g = generator();
    g.scan_paths(&ambiguous, None, Scan::Classmap, &[]).unwrap();
    g.scan_paths(&src, None, Scan::Classmap, &[]).unwrap();
    let filtered = g.class_map().ambiguous_classes(true);
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered["A"], [format!("{src}/A.php")]);
    assert_eq!(g.class_map().ambiguous_classes(false)["A"].len(), 3);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn create_map_throws_when_directory_does_not_exist() {
    let err = create_map(&fixtures("no-file.no-foler")).unwrap_err();
    assert!(err.to_string().contains("Could not scan for classes inside"));
}

#[test]
fn find_classes_throws_when_file_does_not_exist() {
    let err = find_classes(&fixtures("no-file")).unwrap_err();
    assert!(err.to_string().contains("does not exist"));
}

#[test]
fn psr4_violations() {
    let mut g = generator();
    g.scan_paths(&fixtures("psrViolations"), None, Scan::Psr4("ExpectedNamespace\\"), &[]).unwrap();
    let mut violations = g.class_map().psr_violations();
    violations.sort();
    let rule = "does not comply with psr-4 autoloading standard (rule: ExpectedNamespace\\ => ./tests/fixtures/classmap/psrViolations). Skipping.";
    assert_eq!(
        violations,
        [
            format!("Class ClassWithoutNameSpace located in ./tests/fixtures/classmap/psrViolations/ClassWithoutNameSpace.php {rule}"),
            format!(
                "Class ExpectedNamespace\\UnexpectedSubNamespace\\ClassWithIncorrectSubNamespace located in ./tests/fixtures/classmap/psrViolations/ClassWithIncorrectSubNamespace.php {rule}"
            ),
            format!(
                "Class UnexpectedNamespace\\ClassWithNameSpaceOutsideConfiguredScope located in ./tests/fixtures/classmap/psrViolations/ClassWithNameSpaceOutsideConfiguredScope.php {rule}"
            ),
        ]
    );
    assert_eq!(g.class_map().map().keys().collect::<Vec<_>>(), ["ExpectedNamespace\\ClassWithCorrectNameSpace"]);
}

#[test]
fn create_map_with_directory_excluded() {
    let mut g = generator();
    g.scan_paths(&fixtures("beta"), None, Scan::Classmap, &["NamespaceCollision"]).unwrap();
    let expected =
        map("beta", &[("PrefixCollision_A_B_Bar", "PrefixCollision/A/B/Bar.php"), ("PrefixCollision_A_B_Foo", "PrefixCollision/A/B/Foo.php")]);
    assert_eq!(g.class_map().map(), &expected);
}

#[test]
fn psr0_optimized_classmap_respects_namespace_prefix() {
    let mut g = generator();
    g.scan_paths(&fixtures("psr0NamespacePrefix"), None, Scan::Psr0("Acme_"), &[]).unwrap();
    let map = g.class_map().map();
    assert!(map.contains_key("Acme_Utils_Helper"));
    assert!(map.contains_key("Acme_Logger"));
    assert!(!map.contains_key("Other\\Controller\\Page"));
    assert!(!map.contains_key("Other\\Widget"));
    assert!(!map.contains_key("Other_Service"));
    let mut violations = g.class_map().psr_violations();
    violations.sort();
    let rule = "does not comply with psr-0 autoloading standard (rule: Acme_ => ./tests/fixtures/classmap/psr0NamespacePrefix). Skipping.";
    assert_eq!(
        violations,
        [
            format!("Class Other\\Controller\\Page located in ./tests/fixtures/classmap/psr0NamespacePrefix/Other/Controller/Page.php {rule}"),
            format!("Class Other\\Widget located in ./tests/fixtures/classmap/psr0NamespacePrefix/Other/Widget.php {rule}"),
            format!("Class Other_Service located in ./tests/fixtures/classmap/psr0NamespacePrefix/Other/Service.php {rule}"),
        ]
    );
}

/// Finder skips dot files and VCS dirs, follows symlinks, and `*` expands to directories.
#[test]
fn finder_rules() {
    let dir = temp_dir("finder");
    for sub in ["src/.hidden", "src/.git", "src/CVS", "lib/a", "lib/b"] {
        std::fs::create_dir_all(dir.join(sub)).unwrap();
    }
    for (file, class) in
        [("src/A.php", "A"), ("src/.B.php", "B"), ("src/.hidden/C.php", "C"), ("src/CVS/D.php", "D"), ("lib/a/E.php", "E"), ("lib/b/F.inc", "F")]
    {
        std::fs::write(dir.join(file), format!("<?php class {class} {{}}")).unwrap();
    }
    std::os::unix::fs::symlink(dir.join("lib"), dir.join("src/linked")).unwrap();

    let mut g = ClassMapGenerator::new(&["php", "inc"], &dir).unwrap();
    g.avoid_duplicate_scans();
    g.scan_paths("src", None, Scan::Classmap, &[]).unwrap();
    g.scan_paths("lib/*", None, Scan::Classmap, &[]).unwrap();
    let root = dir.display();
    let expected: BTreeMap<String, String> =
        [("A", format!("{root}/src/A.php")), ("E", format!("{root}/src/linked/a/E.php")), ("F", format!("{root}/src/linked/b/F.inc"))]
            .into_iter()
            .map(|(c, p)| (c.to_string(), p))
            .collect();
    assert_eq!(g.class_map().map(), &expected);
    // lib/* files were already scanned through the symlink
    assert!(g.class_map().ambiguous_classes(false).is_empty());
    std::fs::remove_dir_all(dir).unwrap();
}
