//! `ArtifactRepository`: packages from zip/tar archives in a directory (`Util\Zip`/`Util\Tar`
//! `getComposerJson()` find their composer.json).

use crate::util::sha1_hex;
use anyhow::{Context, Result, bail};
use komposer_package::PackageRef;
use komposer_package::io::{Io, Verbosity};
use komposer_package::json;
use komposer_package::loader::{self, LoadOptions};
use komposer_package::repository::{ArrayRepository, LoadRequest, LoadResult, RepoKind, Repository};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub struct ArtifactRepository {
    io: Arc<Io>,
    lookup: String,
    repo_config: Value,
    loaded: Mutex<Option<Arc<ArrayRepository>>>,
}

impl ArtifactRepository {
    pub fn new(repo_config: &Value, io: Arc<Io>) -> ArtifactRepository {
        let lookup = komposer_package::config::expand_path(repo_config.get("url").and_then(Value::as_str).unwrap_or(""));
        ArtifactRepository { io, lookup, repo_config: repo_config.clone(), loaded: Mutex::new(None) }
    }

    pub fn repo_config(&self) -> &Value {
        &self.repo_config
    }

    fn repository(&self) -> Result<Arc<ArrayRepository>> {
        crate::util::lazy_repository(&self.loaded, || self.initialize())
    }

    /// `ArtifactRepository::initialize()`: scans the directory for archives.
    fn initialize(&self) -> Result<ArrayRepository> {
        let mut repo = ArrayRepository::new(self.repo_name(), RepoKind::Remote);
        let mut files = vec![];
        collect_archives(Path::new(&self.lookup), &mut files).with_context(|| format!("Failed to scan artifact directory {}", self.lookup))?;
        for file in files {
            let base = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let Some(packages) = self.composer_information(&file)? else {
                self.io.write_error_at(&format!("File <comment>{base}</comment> doesn't seem to hold a package"), Verbosity::Verbose);
                continue;
            };
            self.io.write_error_at(
                &format!(
                    "Found package <info>{}</info> (<comment>{}</comment>) in file <info>{base}</info>",
                    packages[0].name, packages[0].pretty_version
                ),
                Verbosity::Verbose,
            );
            for p in packages {
                repo.add(p);
            }
        }
        Ok(repo)
    }

    /// `getComposerInformation()`.
    fn composer_information(&self, file: &Path) -> Result<Option<Vec<PackageRef>>> {
        let name = file.to_string_lossy();
        let ext = file.extension().and_then(|e| e.to_str()).unwrap_or("");
        let kind = match ext {
            "gz" | "tar" | "tgz" => "tar",
            "zip" => "zip",
            _ => bail!("Files with \"{ext}\" extensions aren't supported. Only ZIP and TAR/TAR.GZ/TGZ archives are supported."),
        };
        let found = if kind == "tar" { tar_composer_json(file) } else { zip_composer_json(file) };
        let json = match found {
            Ok(Some(json)) => json,
            Ok(None) => return Ok(None),
            Err(e) => {
                self.io.write_at(&format!("Failed loading package {name}: {e}"), Verbosity::Verbose);
                return Ok(None);
            }
        };
        let Value::Object(mut package) = json::parse(&json).with_context(|| format!("\"{name}#composer.json\" does not contain valid JSON"))? else {
            return Ok(None);
        };
        let shasum = sha1_hex(&std::fs::read(file)?);
        package.insert("dist".into(), json!({"type": kind, "url": name.replace('\\', "/"), "shasum": shasum}));
        let packages = loader::load(&package, LoadOptions::default()).with_context(|| format!("Failed loading package in {name}"))?;
        Ok(Some(packages))
    }
}

fn collect_archives(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)?.flatten().map(|e| e.path()).collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            collect_archives(&path, out)?;
        } else if path.is_file()
            && path.extension().and_then(|e| e.to_str()).is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "zip" | "tar" | "gz" | "tgz"))
        {
            out.push(path);
        }
    }
    Ok(())
}

const MULTIPLE_TOP_LEVEL: &str = "Archive has more than one top level directories, and no composer.json was found on the top level, so it's an invalid archive. Top level paths found were: ";

/// `Zip::getComposerJson()`.
fn zip_composer_json(file: &Path) -> Result<Option<String>> {
    let Ok(mut zip) = zip::ZipArchive::new(std::fs::File::open(file)?) else { return Ok(None) };
    if zip.is_empty() {
        return Ok(None);
    }
    let names: Vec<String> = zip.file_names().filter_map(|n| n.ok().map(|n| n.into_owned())).collect();
    let mut read = |name: &str| -> Option<String> {
        let mut entry = zip.by_name(name).ok()?;
        let mut s = String::new();
        entry.read_to_string(&mut s).ok()?;
        Some(s)
    };
    if let Some(json) = read("composer.json") {
        return Ok(Some(json));
    }
    let mut top: Vec<String> = vec![];
    for name in names.iter().filter(|n| !n.contains("__MACOSX")) {
        let dirname = match name.trim_end_matches('/').rsplit_once('/') {
            Some((d, _)) => d,
            None => ".",
        };
        // archives with and without TOC records for the directory itself
        let path = if dirname == "." {
            name.clone()
        } else if !dirname.contains(['/', '\\']) {
            format!("{dirname}/")
        } else {
            continue;
        };
        if !top.contains(&path) {
            top.push(path);
        }
        if top.len() > 1 {
            bail!("{MULTIPLE_TOP_LEVEL}{}", top.join(","));
        }
    }
    if let Some(json) = top.first().and_then(|t| read(&format!("{t}composer.json"))) {
        return Ok(Some(json));
    }
    bail!("No composer.json found either at the top level or within the topmost directory")
}

/// `Tar::getComposerJson()` for tar, tar.gz and tgz files.
fn tar_composer_json(file: &Path) -> Result<Option<String>> {
    let bytes = std::fs::read(file)?;
    let data = if bytes.starts_with(&[0x1f, 0x8b]) {
        let mut out = vec![];
        if flate2::read::GzDecoder::new(&bytes[..]).read_to_end(&mut out).is_err() {
            return Ok(None);
        }
        out
    } else {
        bytes
    };
    let mut entries: Vec<(String, bool, Vec<u8>)> = vec![];
    let mut archive = tar::Archive::new(&data[..]);
    let Ok(iter) = archive.entries() else { return Ok(None) };
    for entry in iter {
        let Ok(mut entry) = entry else { return Ok(None) };
        let path = entry.path().map(|p| p.to_string_lossy().trim_start_matches("./").to_string()).unwrap_or_default();
        let is_dir = entry.header().entry_type().is_dir();
        let mut contents = vec![];
        if !is_dir {
            entry.read_to_end(&mut contents)?;
        }
        entries.push((path, is_dir, contents));
    }
    if entries.is_empty() {
        return Ok(None);
    }
    let content = |name: &str| entries.iter().find(|(p, d, _)| !d && p == name).map(|(_, _, c)| String::from_utf8_lossy(c).into_owned());
    if let Some(json) = content("composer.json") {
        return Ok(Some(json));
    }
    let top: BTreeSet<&str> =
        entries.iter().filter(|(p, d, _)| *d || p.contains('/')).filter_map(|(p, _, _)| p.split('/').next()).filter(|p| !p.is_empty()).collect();
    if top.len() > 1 {
        bail!("{MULTIPLE_TOP_LEVEL}{}", top.into_iter().collect::<Vec<_>>().join(","));
    }
    if let Some(json) = top.first().and_then(|t| content(&format!("{t}/composer.json"))) {
        return Ok(Some(json));
    }
    bail!("No composer.json found either at the top level or within the topmost directory")
}

impl Repository for ArtifactRepository {
    fn repo_name(&self) -> String {
        format!("artifact repo ({})", self.lookup)
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

    fn fixtures() -> String {
        format!("{}/fixtures/artifacts", env!("CARGO_MANIFEST_DIR"))
    }

    #[test]
    fn extracts_configs_from_archives() {
        let repo = ArtifactRepository::new(&json!({"type": "artifact", "url": fixtures()}), Arc::new(Io::null()));
        let mut found: Vec<String> = repo.packages().unwrap().iter().map(|p| format!("{}-{}", p.pretty_name, p.pretty_version)).collect();
        found.sort();
        let mut expected = vec![
            "vendor0/package0-0.0.1",
            "composer/composer-1.0.0-alpha6",
            "vendor1/package2-4.3.2",
            "vendor3/package1-5.4.3",
            "test/jsonInRoot-1.0.0",
            "test/jsonInRootTarFile-1.0.0",
            "test/jsonInFirstLevel-1.0.0",
        ];
        expected.sort();
        assert_eq!(found, expected);
        let packages = repo.packages().unwrap();
        let tar = packages.iter().find(|p| p.pretty_name == "test/jsonInRootTarFile").unwrap();
        let dist = tar.dist.as_ref().unwrap();
        assert_eq!(dist.kind, "tar");
        assert!(dist.url.starts_with(&fixtures()));
        assert_eq!(dist.shasum.as_ref().unwrap().len(), 40);
    }

    #[test]
    fn relative_urls_stay_relative() {
        let repo = ArtifactRepository::new(&json!({"type": "artifact", "url": "fixtures/artifacts"}), Arc::new(Io::null()));
        // cargo runs tests from the crate directory
        assert!(repo.packages().unwrap().iter().all(|p| p.dist.as_ref().unwrap().url.starts_with("fixtures/artifacts")));
    }
}
