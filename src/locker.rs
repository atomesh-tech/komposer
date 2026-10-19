//! `Composer\Package\Locker`: reading and writing composer.lock.

use anyhow::{Context, Result, bail};
use komposer_package::json::{self, Flags};
use komposer_package::loader::{self, LoadOptions};
use komposer_package::repository::{ArrayRepository, RepoKind, Repository, find_with_replacers_and_providers};
use komposer_package::{Link, LinkType, Package, PackageRef, RootAlias, RootPackage, is_platform_package};
use komposer_semver::{DEFAULT_BRANCH_ALIAS, Stability};
use md5::{Digest, Md5};
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// `PluginInterface::PLUGIN_API_VERSION` of Composer 2.10.
pub const PLUGIN_API_VERSION: &str = "2.9.0";

const CONFLICTED_CONTENT_HASH: &str = "VCS merge conflict detected. Please run `composer update --lock`.";

pub struct Locker {
    path: PathBuf,
    hash: String,
    content_hash: String,
    data: std::cell::OnceCell<Map<String, Value>>,
}

/// Everything `setLockData()` takes.
pub struct LockData<'a> {
    pub packages: &'a [PackageRef],
    /// `None` when installed with --no-dev
    pub dev_packages: Option<&'a [PackageRef]>,
    pub platform: Map<String, Value>,
    pub platform_dev: Map<String, Value>,
    pub aliases: &'a [RootAlias],
    pub minimum_stability: Stability,
    pub stability_flags: &'a [(String, Stability)],
    pub prefer_stable: bool,
    pub prefer_lowest: bool,
    pub platform_overrides: Map<String, Value>,
}

impl Locker {
    pub fn new(path: &Path, composer_json: &str) -> Result<Locker> {
        Ok(Locker {
            path: path.to_path_buf(),
            hash: md5_hex(composer_json.as_bytes()),
            content_hash: content_hash(composer_json)?,
            data: std::cell::OnceCell::new(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn content_hash(&self) -> &str {
        &self.content_hash
    }

    /// `isLocked()`. Like `JsonFile::exists()` this needs a regular file, so the `/dev/null` of a
    /// disabled lock file is never locked.
    pub fn is_locked(&self) -> Result<bool> {
        if self.data.get().is_none() && !self.path.is_file() {
            return Ok(false);
        }
        Ok(self.data()?.contains_key("packages"))
    }

    /// The `content-hash` of the lock file, when there is one.
    pub fn locked_content_hash(&self) -> Option<String> {
        if !self.is_locked().unwrap_or(false) {
            return None;
        }
        self.data().ok()?.get("content-hash").and_then(Value::as_str).map(String::from)
    }

    /// `isFresh()`.
    pub fn is_fresh(&self) -> Result<bool> {
        let content_hash = self.content_hash.clone();
        let hash = self.hash.clone();
        let data = self.data()?;
        if let Some(h) = data.get("content-hash").and_then(Value::as_str).filter(|h| !h.is_empty()) {
            return Ok(h == content_hash);
        }
        // BC support for old lock files without content-hash
        Ok(data.get("hash").and_then(Value::as_str).is_some_and(|h| !h.is_empty() && h == hash))
    }

    /// `getLockData()`.
    pub fn data(&self) -> Result<&Map<String, Value>> {
        if self.data.get().is_none() {
            if !self.path.is_file() {
                bail!("No lockfile found. Unable to read locked packages");
            }
            let raw = std::fs::read_to_string(&self.path).with_context(|| format!("Could not read {}", self.path.display()))?;
            let parsed =
                json::parse(&resolve_hash_conflict(&raw)).with_context(|| format!("\"{}\" does not contain valid JSON", self.path.display()))?;
            let Value::Object(m) = parsed else { bail!("Your composer.lock is invalid. Run \"composer update\" to generate a new one.") };
            let _ = self.data.set(m);
        }
        Ok(self.data.get().unwrap())
    }

    /// `getLockedRepository($withDevReqs)`.
    pub fn locked_repository(&self, with_dev: bool) -> Result<ArrayRepository> {
        let data = self.data()?.clone();
        let mut repo = ArrayRepository::new("lock repo", RepoKind::Lock);
        let mut locked: Vec<Value> = data.get("packages").and_then(Value::as_array).cloned().unwrap_or_default();
        if with_dev {
            match data.get("packages-dev") {
                Some(Value::Array(dev)) => locked.extend(dev.iter().cloned()),
                Some(Value::Null) | None => bail!(
                    "The lock file does not contain require-dev information, run install with the --no-dev option or delete it and run composer update to generate a new lock file."
                ),
                _ => {}
            }
        }
        if locked.is_empty() {
            return Ok(repo);
        }
        if !locked[0].get("name").is_some_and(Value::is_string) {
            bail!("Your composer.lock is invalid. Run \"composer update\" to generate a new one.");
        }
        let opts = LoadOptions { transport_options: true, ..Default::default() };
        let mut by_name: Vec<(String, PackageRef)> = vec![];
        for info in &locked {
            let Some(info) = info.as_object() else { continue };
            for p in loader::load(info, opts)? {
                repo.add(p.clone());
                let base = p.alias_of.clone().unwrap_or_else(|| p.clone());
                by_name.retain(|(n, _)| *n != base.name);
                by_name.push((base.name.clone(), base));
            }
        }
        for alias in self.aliases()? {
            if let Some((_, of)) = by_name.iter().find(|(n, _)| *n == alias.package) {
                let mut a = Package::alias(of, &alias.alias_normalized, &alias.alias);
                a.root_package_alias = true;
                repo.add(Arc::new(a));
            }
        }
        Ok(repo)
    }

    /// `getDevPackageNames()`.
    pub fn dev_package_names(&self) -> Result<Vec<String>> {
        Ok(self
            .data()?
            .get("packages-dev")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|p| p.get("name").and_then(Value::as_str).map(str::to_lowercase))
            .collect())
    }

    /// `getPlatformRequirements($withDevReqs)`.
    pub fn platform_requirements(&self, with_dev: bool) -> Result<Vec<Link>> {
        let data = self.data()?;
        let mut out = vec![];
        let mut keys = vec!["platform"];
        if with_dev {
            keys.push("platform-dev");
        }
        for key in keys {
            if let Some(Value::Object(m)) = data.get(key) {
                for link in loader::parse_links("__root__", "1.0.0", LinkType::Require, m, false)? {
                    out.retain(|l: &Link| l.target != link.target);
                    out.push(link);
                }
            }
        }
        Ok(out)
    }

    /// `getMinimumStability()`.
    pub fn minimum_stability(&self) -> Result<Stability> {
        Ok(self.data()?.get("minimum-stability").and_then(Value::as_str).and_then(Stability::parse).unwrap_or(Stability::Stable))
    }

    /// `getStabilityFlags()`: name => numeric stability in the file.
    pub fn stability_flags(&self) -> Result<Vec<(String, Stability)>> {
        let from_priority = |n: i64| match n {
            0 => Stability::Stable,
            5 => Stability::RC,
            10 => Stability::Beta,
            15 => Stability::Alpha,
            _ => Stability::Dev,
        };
        Ok(match self.data()?.get("stability-flags") {
            Some(Value::Object(m)) => m.iter().filter_map(|(k, v)| v.as_i64().map(|n| (k.clone(), from_priority(n)))).collect(),
            _ => vec![],
        })
    }

    /// `getPreferStable()`: `None` for old lock files.
    pub fn prefer_stable(&self) -> Result<Option<bool>> {
        Ok(self.data()?.get("prefer-stable").and_then(Value::as_bool))
    }

    /// `getPreferLowest()`: `None` for old lock files.
    pub fn prefer_lowest(&self) -> Result<Option<bool>> {
        Ok(self.data()?.get("prefer-lowest").and_then(Value::as_bool))
    }

    /// `getPlatformOverrides()`.
    pub fn platform_overrides(&self) -> Result<Map<String, Value>> {
        Ok(self.data()?.get("platform-overrides").and_then(Value::as_object).cloned().unwrap_or_default())
    }

    /// `getAliases()`.
    pub fn aliases(&self) -> Result<Vec<RootAlias>> {
        let s = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
        Ok(self
            .data()?
            .get("aliases")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|a| RootAlias {
                package: s(a, "package"),
                version: s(a, "version"),
                alias: s(a, "alias"),
                alias_normalized: s(a, "alias_normalized"),
            })
            .collect())
    }

    /// `getMissingRequirementInfo()`: root requirements the lock file does not satisfy, as messages.
    pub fn missing_requirement_info(&self, root: &RootPackage, include_dev: bool) -> Result<Vec<String>> {
        let package = &root.package;
        let mut sets = vec![(self.locked_repository(false)?, &package.requires, "Required")];
        if include_dev {
            sets.push((self.locked_repository(true)?, &package.dev_requires, "Required (in require-dev)"));
        }
        let mut info = vec![];
        for (repo, links, description) in sets {
            let mut packages = repo.packages()?;
            packages.push(Arc::new(package.clone()));
            for link in links.iter().filter(|l| !is_platform_package(&l.target) && l.pretty_constraint != "self.version") {
                if !find_with_replacers_and_providers(&packages, &link.target, Some(&link.constraint)).is_empty() {
                    continue;
                }
                let Some(provider) = find_with_replacers_and_providers(&packages, &link.target, None).into_iter().next() else {
                    info.push(format!("- {description} package \"{}\" is not present in the lock file.", link.target));
                    continue;
                };
                let mut desc = provider.pretty_version.clone();
                if provider.name != link.target {
                    let found = [(&provider.replaces, "replaced as"), (&provider.provides, "provided as")]
                        .into_iter()
                        .find_map(|(links, text)| Some((links.iter().find(|l| l.target == link.target)?, text)));
                    if let Some((l, text)) = found {
                        desc = format!("{text} {} by {} {}", l.pretty_constraint, provider.pretty_name, provider.pretty_version);
                    }
                }
                info.push(format!(
                    "- {description} package \"{}\" is in the lock file as \"{desc}\" but that does not satisfy your constraint \"{}\".",
                    link.target, link.pretty_constraint
                ));
            }
        }
        if !info.is_empty() {
            info.push("This usually happens when composer files are incorrectly merged or the composer.json file is manually edited.".into());
            info.push("Read more about correctly resolving merge conflicts https://getcomposer.org/doc/articles/resolving-merge-conflicts.md".into());
            info.push("and prefer using the \"require\" command over editing the composer.json file directly https://getcomposer.org/doc/03-cli.md#require-r".into());
        }
        Ok(info)
    }

    /// `setLockData()`. Returns whether the lock data changed (Composer then prints
    /// "Writing lock file"); writes only when `write` is set.
    pub fn set_lock_data(&mut self, d: LockData, write: bool, commit_time: impl Fn(&Package) -> Option<String>) -> Result<bool> {
        let aliases: Vec<Value> = d
            .aliases
            .iter()
            .map(|a| {
                let version =
                    if matches!(a.version.as_str(), "dev-master" | "dev-trunk" | "dev-default") { DEFAULT_BRANCH_ALIAS } else { &a.version };
                json!({"package": a.package, "version": version, "alias": a.alias, "alias_normalized": a.alias_normalized})
            })
            .collect();
        let mut flags: Map<String, Value> = d.stability_flags.iter().map(|(n, s)| (n.clone(), json!(s.priority()))).collect();
        flags.sort_keys();

        let mut lock = Map::new();
        lock.insert(
            "_readme".into(),
            json!([
                "This file locks the dependencies of your project to a known state",
                "Read more about it at https://getcomposer.org/doc/01-basic-usage.md#installing-dependencies",
                "This file is @generated automatically",
            ]),
        );
        lock.insert("content-hash".into(), self.content_hash.clone().into());
        lock.insert("packages".into(), lock_packages(d.packages, &commit_time));
        lock.insert("packages-dev".into(), d.dev_packages.map(|p| lock_packages(p, &commit_time)).unwrap_or(Value::Null));
        lock.insert("aliases".into(), Value::Array(aliases));
        lock.insert("minimum-stability".into(), d.minimum_stability.as_str().into());
        lock.insert("stability-flags".into(), Value::Object(flags));
        lock.insert("prefer-stable".into(), d.prefer_stable.into());
        lock.insert("prefer-lowest".into(), d.prefer_lowest.into());
        lock.insert("platform".into(), Value::Object(d.platform));
        lock.insert("platform-dev".into(), Value::Object(d.platform_dev));
        if !d.platform_overrides.is_empty() {
            lock.insert("platform-overrides".into(), Value::Object(d.platform_overrides));
        }
        lock.insert("plugin-api-version".into(), PLUGIN_API_VERSION.into());

        // PHP compares the new array (holding stdClass for empty maps) with the decoded file
        // (holding [] for them), so empty maps always count as a change
        let has_std_class = ["stability-flags", "platform", "platform-dev"].iter().any(|k| lock[*k].as_object().is_some_and(Map::is_empty));
        let lock = Value::Object(lock);
        let current = if self.is_locked().unwrap_or(false) { self.data.get().cloned().map(Value::Object) } else { None };
        if has_std_class || current.as_ref() != Some(&json::php_normalize(lock.clone())) {
            self.data = std::cell::OnceCell::new();
            if write {
                json::write(&self.path, &lock)?;
            } else if let Some(m) = json::php_normalize(lock).as_object() {
                // dry runs keep the virtual lock file in memory
                let _ = self.data.set(m.clone());
            }
            return Ok(true);
        }
        Ok(false)
    }

    /// `updateHash()`: refresh only `content-hash`, keeping the lock file's mtime.
    pub fn update_hash(&mut self, composer_json: &str) -> Result<()> {
        let mtime = std::fs::metadata(&self.path).and_then(|m| m.modified()).ok();
        let mut data = self.data()?.clone();
        data.insert("content-hash".into(), content_hash(composer_json)?.into());
        // fixupJsonDataType()
        for key in ["stability-flags", "platform", "platform-dev"] {
            if data.get(key).is_some_and(|v| v.as_array().is_some_and(Vec::is_empty)) {
                data.insert(key.into(), Value::Object(Map::new()));
            }
        }
        if let Some(Value::Object(flags)) = data.get_mut("stability-flags") {
            flags.sort_keys();
        }
        json::write(&self.path, &Value::Object(data))?;
        self.data = std::cell::OnceCell::new();
        if let Some(t) = mtime {
            let _ = std::fs::File::options().write(true).open(&self.path).and_then(|f| f.set_modified(t));
        }
        Ok(())
    }
}

fn lock_packages(packages: &[PackageRef], commit_time: &impl Fn(&Package) -> Option<String>) -> Value {
    let mut locked: Vec<Map<String, Value>> = packages
        .iter()
        .filter(|p| !p.is_alias())
        .map(|p| {
            // use the exact commit time of the current reference if it's a dev package installed from source
            let t = (p.is_dev() && p.installation_source.as_deref() == Some("source")).then(|| commit_time(p)).flatten();
            komposer_package::dumper::dump_for_lock(p, t)
        })
        .collect();
    let key = |m: &Map<String, Value>, k: &str| m.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
    locked.sort_by(|a, b| key(a, "name").cmp(&key(b, "name")).then_with(|| key(a, "version").cmp(&key(b, "version"))));
    Value::Array(locked.into_iter().map(Value::Object).collect())
}

/// `Locker::getContentHash()`.
pub fn content_hash(composer_json: &str) -> Result<String> {
    let content = json::parse(composer_json).context("composer.json does not contain valid JSON")?;
    let content = content.as_object().cloned().unwrap_or_default();
    const RELEVANT: [&str; 11] = [
        "name",
        "version",
        "require",
        "require-dev",
        "conflict",
        "replace",
        "provide",
        "minimum-stability",
        "prefer-stable",
        "repositories",
        "extra",
    ];
    let mut relevant = Map::new();
    for key in RELEVANT {
        if let Some(v) = content.get(key) {
            relevant.insert(key.into(), v.clone());
        }
    }
    if let Some(platform) = content.get("config").and_then(|c| c.get("platform")) {
        relevant.insert("config".into(), json!({"platform": platform}));
    }
    json::ksort(&mut relevant);
    Ok(md5_hex(json::encode(&Value::Object(relevant), Flags::PLAIN).as_bytes()))
}

pub(crate) fn md5_hex(data: &[u8]) -> String {
    Md5::digest(data).iter().map(|b| format!("{b:02x}")).collect()
}

/// `JsonFile::parseJson()` for lock files: a git conflict on `content-hash` only is
/// replaced by a marker so the file still parses.
fn resolve_hash_conflict(raw: &str) -> String {
    if !raw.contains("\"content-hash\"") || !raw.contains("<<<<<<< ") {
        return raw.to_string();
    }
    let lines: Vec<&str> = raw.split_inclusive('\n').collect();
    let is_hash = |l: &str| {
        let t = l.trim();
        t.starts_with("\"content-hash\"") && t.ends_with(',')
    };
    for i in 0..lines.len() {
        if !lines[i].starts_with("<<<<<<< ") || i + 1 >= lines.len() || !is_hash(lines[i + 1]) {
            continue;
        }
        let mut j = i + 2;
        if lines.get(j).is_some_and(|l| l.starts_with("|||||||")) && lines.get(j + 1).is_some_and(|l| is_hash(l)) {
            j += 2;
        }
        if lines.get(j).is_some_and(|l| l.trim_end() == "=======")
            && lines.get(j + 1).is_some_and(|l| is_hash(l))
            && lines.get(j + 2).is_some_and(|l| l.starts_with(">>>>>>> "))
        {
            let eol = if lines[j + 2].ends_with("\r\n") { "\r\n" } else { "\n" };
            let mut out: String = lines[..i].concat();
            out.push_str(&format!("    \"content-hash\": \"{CONFLICTED_CONTENT_HASH}\",{eol}"));
            out.push_str(&lines[j + 3..].concat());
            return out;
        }
    }
    raw.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_hash_matches_composer() {
        // composer.json of laravel/laravel v13.11.0; hash taken from the lock file Composer 2.10.3 wrote
        let composer_json = include_str!("../tests/fixtures/laravel-13/composer.json");
        assert_eq!(content_hash(composer_json).unwrap(), "cda2add6397422fb1a2eb5b9207f1575");
    }

    #[test]
    fn conflicted_hash_is_tolerated() {
        let raw = "{\n<<<<<<< HEAD\n    \"content-hash\": \"aa\",\n=======\n    \"content-hash\": \"bb\",\n>>>>>>> branch\n    \"packages\": []\n}\n";
        let fixed = resolve_hash_conflict(raw);
        assert!(json::parse(&fixed).unwrap()["content-hash"] == CONFLICTED_CONTENT_HASH);
    }
}
