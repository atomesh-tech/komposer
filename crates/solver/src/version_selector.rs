//! `Package\Version\VersionSelector`: the version `require` and `create-project` pick, and the
//! constraint they write to composer.json.

use crate::filter::PlatformRequirementFilter;
use crate::repository_set::RepositorySet;
use anyhow::Result;
use komposer_package::io::{Io, Verbosity};
use komposer_package::repository::Repository;
use komposer_package::{Package, PackageRef, dumper, is_platform_package, loader};
use komposer_semver::{Constraint, DEFAULT_BRANCH_ALIAS, Op, Stability, parse_constraints, version_compare};
use regex::Regex;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

/// Warnings with the verbosity they are shown at.
type Warnings = Vec<(String, Verbosity)>;

/// `VersionSelector`.
pub struct VersionSelector<'a> {
    set: &'a RepositorySet,
    /// platform package name => `== version` constraints
    platform_constraints: HashMap<String, Vec<Constraint>>,
    /// `PHP_VERSION` as major.minor.release, taken from the platform's `php` package
    php_version: Option<String>,
}

impl<'a> VersionSelector<'a> {
    /// With a platform repository, candidates are checked against its packages.
    pub fn new(set: &'a RepositorySet, platform: Option<&dyn Repository>) -> Result<VersionSelector<'a>> {
        let mut platform_constraints: HashMap<String, Vec<Constraint>> = HashMap::new();
        let mut php_version = None;
        for p in platform.map(|r| r.packages()).transpose()?.unwrap_or_default() {
            platform_constraints.entry(p.name.clone()).or_default().push(Constraint::single(Op::Eq, p.version.clone()));
            if p.name == "php" {
                php_version = Some(first_three_parts(&p.version));
            }
        }
        Ok(VersionSelector { set, platform_constraints, php_version })
    }

    /// `findBestCandidate()`: the best version of `name` matching `target_version`, or `None`.
    /// `show_warnings` decides for which packages platform mismatches are reported on `io`.
    #[allow(clippy::too_many_arguments)]
    pub fn find_best_candidate(
        &self,
        name: &str,
        target_version: Option<&str>,
        preferred_stability: Stability,
        filter: &PlatformRequirementFilter,
        repo_set_flags: u8,
        io: Option<&Io>,
        show_warnings: &dyn Fn(&Package) -> bool,
    ) -> Result<Option<PackageRef>> {
        let (best, warnings) = self.best_candidate(name, target_version, preferred_stability, filter, repo_set_flags, show_warnings)?;
        if let Some(io) = io {
            for (warning, verbosity) in warnings {
                io.write_error_at(&warning, verbosity);
            }
        }
        Ok(best)
    }

    fn best_candidate(
        &self,
        name: &str,
        target_version: Option<&str>,
        preferred_stability: Stability,
        filter: &PlatformRequirementFilter,
        repo_set_flags: u8,
        show_warnings: &dyn Fn(&Package) -> bool,
    ) -> Result<(Option<PackageRef>, Warnings)> {
        let constraint = target_version.filter(|v| !v.is_empty()).map(parse_constraints).transpose()?;
        let mut candidates = self.set.find_packages(&name.to_lowercase(), constraint.as_ref(), repo_set_flags)?;

        // acceptable stabilities first, highest version first, then the most stable of the unacceptable ones
        let min = preferred_stability.priority();
        candidates.sort_by(|a, b| {
            let (pa, pb) = (a.stability.priority(), b.stability.priority());
            if min < pa && pb < pa {
                Ordering::Greater
            } else if (min < pa && pa < pb) || (min >= pa && min < pb) {
                Ordering::Less
            } else {
                version_compare(&b.version, &a.version)
            }
        });

        let mut warnings = vec![];
        let package = if self.platform_constraints.is_empty() || matches!(filter, PlatformRequirementFilter::IgnoreAll) {
            candidates.into_iter().next()
        } else {
            let mut warned = HashSet::new();
            let mut seen = HashSet::new();
            let mut selected = None;
            for p in candidates {
                let mut skip = false;
                for link in p.requires.iter().filter(|l| is_platform_package(&l.target) && !filter.is_ignored(&l.target)) {
                    let reason = match self.platform_constraints.get(&link.target) {
                        Some(provided) => {
                            let satisfied = provided.iter().any(|c| {
                                link.constraint.matches(c)
                                    || (filter.is_upper_bound_ignored(&link.target)
                                        && filter.filter_constraint(&link.target, &link.constraint, true).matches(c))
                            });
                            if satisfied {
                                continue;
                            }
                            "is not satisfied by your platform"
                        }
                        // the platform cannot validate this constraint, so the package is not installable
                        None => "is missing from your platform",
                    };
                    let latest = seen.insert(p.name.clone());
                    if show_warnings(&p) {
                        let first = warned.insert(format!("{}/{}", p.name, link.target));
                        warnings.push((
                            format!(
                                "<warning>Cannot use {}{} {} as it {} {} {} which {reason}.</warning>",
                                p.pretty_name,
                                if latest { "'s latest version" } else { "" },
                                p.pretty_version,
                                link.kind.description(),
                                link.target,
                                link.pretty_constraint
                            ),
                            if first { Verbosity::Normal } else { Verbosity::Verbose },
                        ));
                    }
                    skip = true;
                }
                if !skip {
                    selected = Some(p);
                    break;
                }
            }
            selected
        };

        // a 9999999-dev alias stands for the branch it aliases
        let package = package.map(|p| match &p.alias_of {
            Some(of) if p.version == DEFAULT_BRANCH_ALIAS => of.clone(),
            _ => p,
        });
        Ok((package, warnings))
    }

    /// `findRecommendedRequireVersion()`: e.g. `^1.2` for 1.2.1, `^2.0@beta` for 2.0-beta.1, `^2.1@dev`
    /// for a dev branch aliased to 2.1.x-dev.
    pub fn find_recommended_require_version(&self, package: &Package) -> String {
        // extensions versioned in sync with PHP are rather required as "*"
        if package.name.starts_with("ext-") && self.php_version.as_deref() == Some(&first_three_parts(&package.version)) {
            return "*".into();
        }
        if !package.is_dev() {
            return transform_version(&package.version, &package.pretty_version, package.stability);
        }

        static NUMERIC_ALIAS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(\d+\.\d+\.\d+)(\.9999999)-dev$").unwrap());
        if let Ok(Some(alias)) = loader::branch_alias(&dumper::dump(package))
            && alias != DEFAULT_BRANCH_ALIAS
            && let Some(c) = NUMERIC_ALIAS.captures(&alias)
        {
            let version = format!("{}.0", &c[1]).replace(".9999999", ".0");
            return transform_version(&version, &version, Stability::Dev);
        }
        package.pretty_version.clone()
    }
}

fn first_three_parts(version: &str) -> String {
    version.split('.').take(3).collect::<Vec<_>>().join(".")
}

/// `transformVersion()`: 2.1.1 => ^2.1, 0.1.3 => ^0.1.3, plus the stability flag.
fn transform_version(version: &str, pretty: &str, stability: Stability) -> String {
    static SEMVER_PART: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d+\D?").unwrap());
    let parts: Vec<&str> = version.split('.').collect();
    if parts.len() != 4 || !SEMVER_PART.is_match(parts[3]) {
        return pretty.to_string();
    }
    let keep = if parts[0] == "0" { 3 } else { 2 };
    let mut version = parts[..keep].join(".");
    if stability != Stability::Stable {
        version += &format!("@{stability}");
    }
    format!("^{version}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use komposer_package::repository::{ArrayRepository, RepoKind};
    use komposer_package::{Link, LinkType};
    use komposer_semver::normalize;
    use serde_json::json;
    use std::sync::Arc;

    fn pkg(version: &str) -> Package {
        Package::new("foo/bar", &normalize(version).unwrap(), version)
    }

    fn requiring(version: &str, target: &str, constraint: &str) -> Package {
        let mut p = pkg(version);
        p.requires = vec![Link {
            source: "foo/bar".into(),
            target: target.into(),
            constraint: parse_constraints(constraint).unwrap(),
            kind: LinkType::Require,
            pretty_constraint: constraint.into(),
        }];
        p
    }

    fn set_of(packages: Vec<Package>) -> (RepositorySet, Vec<PackageRef>) {
        let mut repo = ArrayRepository::new("repo", RepoKind::Remote);
        let refs = packages.into_iter().map(|p| repo.add_package(p)).collect();
        let mut set = RepositorySet::new(Stability::Dev);
        set.add_repository(Arc::new(repo));
        (set, refs)
    }

    fn platform(packages: &[(&str, &str)]) -> ArrayRepository {
        let mut repo = ArrayRepository::new("platform repo", RepoKind::Platform);
        for (name, version) in packages {
            repo.add_package(Package::new(name, &normalize(version).unwrap(), version));
        }
        repo
    }

    fn best(selector: &VersionSelector, stability: Stability, filter: &PlatformRequirementFilter) -> (Option<PackageRef>, Warnings) {
        selector.best_candidate("foo/bar", None, stability, filter, 0, &|_| true).unwrap()
    }

    fn version(p: Option<PackageRef>) -> String {
        p.map(|p| p.pretty_version.clone()).unwrap_or_default()
    }

    #[test]
    fn latest_version_is_returned() {
        let (set, _) = set_of(vec![pkg("1.2.1"), pkg("1.2.2"), pkg("1.2.0")]);
        let selector = VersionSelector::new(&set, None).unwrap();
        assert_eq!(version(best(&selector, Stability::Stable, &PlatformRequirementFilter::IgnoreNothing).0), "1.2.2");
    }

    #[test]
    fn latest_version_matching_php_requirements() {
        let (set, _) = set_of(vec![
            requiring("0.9.0", "php", ">=5.6"),
            requiring("1.0.0", "php", ">=5.4"),
            requiring("2.0.0", "php", ">=5.6"),
            requiring("2.1.0", "php", ">=5.6"),
        ]);
        let platform = platform(&[("php", "5.5.0")]);
        let selector = VersionSelector::new(&set, Some(&platform)).unwrap();
        let (found, warnings) = best(&selector, Stability::Stable, &PlatformRequirementFilter::IgnoreNothing);
        assert_eq!(version(found), "1.0.0");
        assert_eq!(
            warnings,
            vec![
                (
                    "<warning>Cannot use foo/bar's latest version 2.1.0 as it requires php >=5.6 which is not satisfied by your platform.</warning>"
                        .to_string(),
                    Verbosity::Normal
                ),
                (
                    "<warning>Cannot use foo/bar 2.0.0 as it requires php >=5.6 which is not satisfied by your platform.</warning>".to_string(),
                    Verbosity::Verbose
                ),
            ]
        );
        assert_eq!(version(best(&selector, Stability::Stable, &PlatformRequirementFilter::IgnoreAll).0), "2.1.0");
    }

    #[test]
    fn latest_version_matching_ext_and_composer_requirements() {
        for (name, provided, low, high) in [("ext-zip", "5.3.0", "^5.2", "^5.4"), ("composer-runtime-api", "1.0.0", "^1.0", "^2.0")] {
            let (set, _) = set_of(vec![requiring("1.0.0", name, low), requiring("2.0.0", name, high)]);
            let platform = platform(&[(name, provided)]);
            let selector = VersionSelector::new(&set, Some(&platform)).unwrap();
            assert_eq!(version(best(&selector, Stability::Stable, &PlatformRequirementFilter::IgnoreNothing).0), "1.0.0");
            assert_eq!(version(best(&selector, Stability::Stable, &PlatformRequirementFilter::IgnoreAll).0), "2.0.0");
        }
    }

    #[test]
    fn missing_platform_package_skips_candidate() {
        let (set, _) = set_of(vec![pkg("1.0.0"), requiring("2.0.0", "ext-barfoo", "*")]);
        let platform = platform(&[("php", "8.3.0")]);
        let selector = VersionSelector::new(&set, Some(&platform)).unwrap();
        let (found, warnings) = best(&selector, Stability::Stable, &PlatformRequirementFilter::IgnoreNothing);
        assert_eq!(version(found), "1.0.0");
        assert!(warnings[0].0.ends_with("requires ext-barfoo * which is missing from your platform.</warning>"));
        assert_eq!(version(best(&selector, Stability::Stable, &PlatformRequirementFilter::IgnoreAll).0), "2.0.0");
    }

    #[test]
    fn upper_bound_ignored_platform_requirements() {
        let (set, _) = set_of(vec![requiring("1.0.0", "php", "^7.4")]);
        let platform = platform(&[("php", "8.3.0")]);
        let selector = VersionSelector::new(&set, Some(&platform)).unwrap();
        assert!(best(&selector, Stability::Stable, &PlatformRequirementFilter::IgnoreNothing).0.is_none());
        let filter = PlatformRequirementFilter::ignore_list(&["php+"]);
        assert_eq!(version(best(&selector, Stability::Stable, &filter).0), "1.0.0");
    }

    #[test]
    fn stability_preferences() {
        let none = PlatformRequirementFilter::IgnoreNothing;
        let pick = |versions: &[&str], stability| {
            let (set, _) = set_of(versions.iter().map(|v| pkg(v)).collect());
            version(best(&VersionSelector::new(&set, None).unwrap(), stability, &none).0)
        };
        assert_eq!(pick(&["1.0.0", "1.1.0-beta"], Stability::Stable), "1.0.0");
        assert_eq!(pick(&["2.x-dev", "2.0.0-beta3"], Stability::Stable), "2.0.0-beta3");
        assert_eq!(pick(&["2.0.0-beta3", "2.x-dev"], Stability::Stable), "2.0.0-beta3");
        assert_eq!(pick(&["1.0.0", "1.1.0-beta"], Stability::Dev), "1.1.0-beta");
        assert_eq!(pick(&["1.0.0", "1.1.0-beta", "1.2.0-alpha"], Stability::Beta), "1.1.0-beta");
        assert_eq!(pick(&["1.1.0-beta", "1.2.0-alpha"], Stability::Stable), "1.1.0-beta");
        assert_eq!(pick(&[], Stability::Stable), "");
    }

    #[test]
    fn default_branch_alias_is_never_returned() {
        let mut repo = ArrayRepository::new("repo", RepoKind::Remote);
        repo.add_package(pkg("1.1.0-beta"));
        let main = Arc::new(pkg("dev-main"));
        let main_alias = repo.add_package(Package::alias(&main, DEFAULT_BRANCH_ALIAS, DEFAULT_BRANCH_ALIAS));
        let mut set = RepositorySet::new(Stability::Dev);
        set.add_repository(Arc::new(repo));
        let selector = VersionSelector::new(&set, None).unwrap();
        let found = best(&selector, Stability::Dev, &PlatformRequirementFilter::IgnoreNothing).0.unwrap();
        assert!(Arc::ptr_eq(&found, main_alias.alias_of.as_ref().unwrap()));
    }

    #[test]
    fn recommended_require_version() {
        let platform = platform(&[("php", "8.3.4")]);
        let (set, _) = set_of(vec![]);
        let selector = VersionSelector::new(&set, Some(&platform)).unwrap();
        let cases: &[(&str, &str, Option<&str>, &str)] = &[
            ("1.2.1", "^1.2", None, "foo/bar"),
            ("1.2", "^1.2", None, "foo/bar"),
            ("v1.2.1", "^1.2", None, "foo/bar"),
            ("3.1.2-pl2", "^3.1", None, "foo/bar"),
            ("3.1.2-patch", "^3.1", None, "foo/bar"),
            ("2.0-beta.1", "^2.0@beta", None, "foo/bar"),
            ("3.1.2-alpha5", "^3.1@alpha", None, "foo/bar"),
            ("3.0-RC2", "^3.0@RC", None, "foo/bar"),
            ("0.1.0", "^0.1.0", None, "foo/bar"),
            ("0.1.3", "^0.1.3", None, "foo/bar"),
            ("0.0.3", "^0.0.3", None, "foo/bar"),
            ("0.0.3-alpha", "^0.0.3@alpha", None, "foo/bar"),
            ("0.0.3.4-alpha", "^0.0.3@alpha", None, "foo/bar"),
            ("3.0.0.2-RC2", "^3.0@RC", None, "foo/bar"),
            ("1.2.1.1020402", "^1.2", None, "foo/bar"),
            // date-based versions are not touched at all
            ("v20121020", "v20121020", None, "foo/bar"),
            ("v20121020.2", "v20121020.2", None, "foo/bar"),
            // dev packages without alias are not touched at all
            ("dev-master", "dev-master", None, "foo/bar"),
            ("3.1.2-dev", "3.1.2-dev", None, "foo/bar"),
            // dev packages with alias inherit the alias
            ("dev-master", "^2.1@dev", Some("2.1.x-dev"), "foo/bar"),
            ("dev-master", "^2.1@dev", Some("2.1-dev"), "foo/bar"),
            ("dev-master", "^2.1@dev", Some("2.1.3.x-dev"), "foo/bar"),
            ("dev-master", "^2.0@dev", Some("2.x-dev"), "foo/bar"),
            ("dev-master", "^0.3.0@dev", Some("0.3.x-dev"), "foo/bar"),
            ("dev-master", "^0.0.3@dev", Some("0.0.3.x-dev"), "foo/bar"),
            ("dev-master", "dev-master", Some(DEFAULT_BRANCH_ALIAS), "foo/bar"),
            // numeric alias
            ("3.x-dev", "^3.0@dev", Some("3.0.x-dev"), "foo/bar"),
            ("3.x-dev", "^3.0@dev", Some("3.0-dev"), "foo/bar"),
            // ext in sync with php
            ("8.3.4", "*", None, "ext-filter"),
            // ext versioned individually
            ("3.0.5", "^3.0", None, "ext-xdebug"),
        ];
        for (pretty, expected, branch_alias, name) in cases {
            let mut p = Package::new(name, &normalize(pretty).unwrap(), pretty);
            if let Some(alias) = branch_alias {
                p.extra = json!({"branch-alias": {*pretty: alias}});
            }
            assert_eq!(selector.find_recommended_require_version(&p), *expected, "{pretty} {branch_alias:?}");
        }
    }
}
