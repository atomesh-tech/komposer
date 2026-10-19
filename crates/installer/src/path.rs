//! `Downloader\PathDownloader`: `path` repository packages, symlinked or mirrored.

use crate::fs::{self, find_shortest_path, normalize_path, realpath};
use crate::manager::InstallationManager;
use anyhow::{Result, bail};
use komposer_package::Package;
use komposer_package::operation::Operation;
use serde_json::Value;
use std::path::Path;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Strategy {
    Symlink,
    Mirror,
}

fn dist_url<'p>(p: &'p Package, action: &str) -> Result<&'p str> {
    match p.dist.as_ref().map(|d| d.url.as_str()).filter(|u| !u.is_empty()) {
        Some(url) => Ok(url),
        None => bail!("The package {} has no dist url configured, cannot {action}.", p.pretty_name),
    }
}

fn real_dist_url<'p>(p: &'p Package, action: &str) -> Result<(&'p str, String)> {
    let url = dist_url(p, action)?;
    match realpath(url) {
        Some(real) => Ok((url, real)),
        None => bail!("Failed to realpath {url}"),
    }
}

/// `computeAllowedStrategies()`: `options.symlink` true/false forces one, null allows a mirror fallback.
fn strategies(p: &Package) -> (Strategy, Vec<Strategy>) {
    let mirror_env = std::env::var("COMPOSER_MIRROR_PATH_REPOS").is_ok_and(|v| !v.is_empty() && v != "0");
    match p.transport_options.get("symlink") {
        Some(Value::Bool(true)) => (Strategy::Symlink, vec![Strategy::Symlink]),
        Some(Value::Bool(false)) => (Strategy::Mirror, vec![Strategy::Mirror]),
        _ => (if mirror_env { Strategy::Mirror } else { Strategy::Symlink }, vec![Strategy::Symlink, Strategy::Mirror]),
    }
}

impl InstallationManager<'_> {
    /// `PathDownloader::download()`: only validates the source path.
    pub(crate) fn path_download(&self, p: &Package, path: &str) -> Result<()> {
        let path = fs::trim_trailing_slash(path);
        let url = dist_url(p, "download")?;
        let real_url = realpath(url).filter(|u| Path::new(u).is_dir());
        let Some(real_url) = real_url else {
            bail!("Source path \"{url}\" is not found for package {}", p.name);
        };
        let real_path = realpath(path);
        if real_path.as_deref() == Some(&real_url) {
            return Ok(());
        }
        let real_path = real_path.unwrap_or_default();
        if format!("{real_path}/").starts_with(&format!("{real_url}/")) {
            bail!("Package {} cannot install to \"{real_path}\" inside its source at \"{real_url}\"", p.name);
        }
        Ok(())
    }

    /// `PathDownloader::install()`.
    pub(crate) fn path_install(&self, p: &Package, path: &str, output: bool) -> Result<()> {
        let path = fs::trim_trailing_slash(path);
        let (url, real_url) = real_dist_url(p, "install")?;
        let line = format!("  - {}", Operation::Install(p.clone().into()).show(false));
        if realpath(path).as_deref() == Some(&real_url) {
            if output {
                self.out.write(&format!("{line}: Source already present"));
            }
            return Ok(());
        }
        let relative = p.transport_options.get("relative").is_none_or(|v| v == &Value::Bool(true));
        let (mut strategy, allowed) = strategies(p);
        fs::remove_directory(path)?;

        let mut line = format!("{line}: ");
        let mut fallback = false;
        if strategy == Strategy::Symlink {
            line.push_str(&format!("Symlinking from {url}"));
            let path = path.trim_end_matches('/');
            let target = if relative {
                let abs = if fs::is_absolute_path(path) { path.to_string() } else { format!("{}/{path}", fs::cwd()?) };
                format!("{}/", find_shortest_path(&abs, &real_url, false, true)?)
            } else {
                format!("{real_url}/")
            };
            if symlink(&target, path).is_err() {
                if !allowed.contains(&Strategy::Mirror) {
                    bail!("Symlink from \"{real_url}\" to \"{path}\" failed!");
                }
                if output {
                    self.out.write(&line);
                    self.out.write("    <error>Symlink failed, fallback to use mirroring!</error>");
                }
                line.clear();
                strategy = Strategy::Mirror;
                fallback = true;
            }
        }
        if strategy == Strategy::Mirror {
            line.push_str(&format!("{}Mirroring from {url}", if fallback { "    " } else { "" }));
            mirror(&normalize_path(&real_url), path)?;
        }
        if output {
            self.out.write(&line);
        }
        Ok(())
    }

    /// `PathDownloader::remove()`: prints the operation and says whether to delete `path`,
    /// which is never the source itself.
    pub(crate) fn path_remove(&self, p: &Package, path: &str, output: bool) -> Result<bool> {
        let path = fs::trim_trailing_slash(path);
        let url = dist_url(p, "remove")?;
        let line = format!("  - {}", Operation::Uninstall(p.clone().into()).show(false));
        // not realpath'd: a symlink to the dist url must still be removed
        let cwd = fs::cwd()?;
        let abs = |s: &str| normalize_path(&if fs::is_absolute_path(s) { s.to_string() } else { format!("{cwd}/{s}") });
        if abs(path) == abs(url) {
            if output {
                self.out.write(&format!("{line}, source is still present in {path}"));
            }
            return Ok(false);
        }
        if output {
            self.out.write(&line);
        }
        Ok(true)
    }

    /// `PathDownloader::getInstallOperationAppendix()`.
    pub(crate) fn path_appendix(&self, p: &Package, path: &str) -> Result<String> {
        let (url, real_url) = real_dist_url(p, "install")?;
        if realpath(path).as_deref() == Some(&real_url) {
            return Ok(": Source already present".into());
        }
        Ok(match strategies(p).0 {
            Strategy::Symlink => format!(": Symlinking from {url}"),
            Strategy::Mirror => format!(": Mirroring from {url}"),
        })
    }
}

/// Symfony `Filesystem::symlink()`.
fn symlink(origin: &str, target: &str) -> Result<()> {
    fs::ensure_directory_exists(&fs::dirname(target))?;
    if let Ok(existing) = std::fs::read_link(target) {
        if existing.to_string_lossy() == origin {
            return Ok(());
        }
        fs::unlink(target)?;
    }
    fs::symlink(origin, target)
}

/// Symfony `Filesystem::mirror()` over `ArchivableFilesFinder`: VCS directories and symlinks
/// leaving the source are skipped.
// ponytail: .gitattributes export-ignore rules are not applied, port GitExcludeFilter if a mirrored path package needs them
fn mirror(source: &str, target: &str) -> Result<()> {
    const VCS: [&str; 9] = [".svn", "_svn", "CVS", "_darcs", ".arch-params", ".monotone", ".bzr", ".git", ".hg"];
    fs::ensure_directory_exists(target)?;
    let mut entries: Vec<_> = std::fs::read_dir(source)?.collect::<Result<_, _>>()?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        if VCS.contains(&name.as_str()) {
            continue;
        }
        let (from, to) = (format!("{source}/{name}"), format!("{target}/{name}"));
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            let inside = realpath(&from).is_some_and(|r| r.starts_with(source));
            if inside {
                fs::symlink(&std::fs::read_link(&from)?.to_string_lossy(), &to)?;
            }
        } else if file_type.is_dir() {
            mirror(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::installed::InstalledRepo;
    use crate::manager::InstallationManager;
    use crate::tests::{CWD, config, package, tmp_dir};
    use komposer_package::io::{Io, Verbosity};
    use komposer_package::operation::Operation;
    use serde_json::json;
    use std::path::Path;

    /// Runs `f` in a project dir containing `packages/foo` (with a `.git` dir).
    fn in_project(name: &str, f: impl FnOnce(&str)) {
        let _cwd = CWD.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tmp_dir(name);
        std::fs::create_dir_all(format!("{dir}/packages/foo/src")).unwrap();
        std::fs::create_dir_all(format!("{dir}/packages/foo/.git")).unwrap();
        std::fs::write(format!("{dir}/packages/foo/src/Foo.php"), "<?php").unwrap();
        let previous = std::env::current_dir().unwrap();
        std::env::set_current_dir(&*dir).unwrap();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&dir)));
        std::env::set_current_dir(previous).unwrap();
        result.unwrap();
    }

    fn foo(options: serde_json::Value) -> komposer_package::PackageRef {
        package(json!({"name": "acme/foo", "version": "dev-main", "dist": {"type": "path", "url": "packages/foo"}, "transport-options": options}))
    }

    #[test]
    fn symlinks_relatively_and_never_removes_the_source() {
        in_project("path-symlink", |dir| {
            let (config, io) = (config(dir, json!({})), Io::new(Verbosity::Normal, Some(false), false));
            let fetch = |_: Vec<crate::FetchJob>| -> Vec<anyhow::Result<()>> { panic!("nothing to download") };
            let mut im = InstallationManager::new(&config, &io, &fetch);
            let mut repo = InstalledRepo::load(&format!("{dir}/vendor/composer/installed.json"), None).unwrap();
            // komposer loads transport-options from the repository's options
            let mut p = (*foo(json!(null))).clone();
            p.transport_options = json!({});
            let p: komposer_package::PackageRef = p.into();
            im.execute(&mut repo, &[Operation::Install(p.clone())], true, false, None).unwrap();
            assert_eq!(im.output(), ["  - Installing acme/foo (dev-main): Symlinking from packages/foo"]);
            let link = format!("{dir}/vendor/acme/foo");
            assert_eq!(std::fs::read_link(&link).unwrap().to_string_lossy(), "../../packages/foo/");
            assert!(Path::new(&format!("{link}/src/Foo.php")).is_file());
            let json = std::fs::read_to_string(format!("{dir}/vendor/composer/installed.json")).unwrap();
            assert!(json.contains("\"installation-source\": \"dist\",\n            \"install-path\": \"../acme/foo\""), "{json}");

            im.execute(&mut repo, &[Operation::Update { initial: p.clone(), target: p.clone() }], true, false, None).unwrap();
            assert_eq!(im.output(), ["  - Upgrading acme/foo (dev-main => dev-main): Source already present"]);

            im.execute(&mut repo, &[Operation::Uninstall(p)], true, false, None).unwrap();
            assert_eq!(im.output(), ["  - Removing acme/foo (dev-main)"]);
            assert!(std::fs::symlink_metadata(&link).is_err());
            assert!(Path::new(&format!("{dir}/packages/foo/src/Foo.php")).is_file(), "the source survives");
        });
    }

    #[test]
    fn mirrors_without_vcs_dirs_when_symlinks_are_disabled() {
        in_project("path-mirror", |dir| {
            let (config, io) = (config(dir, json!({})), Io::new(Verbosity::Normal, Some(false), false));
            let fetch = |_: Vec<crate::FetchJob>| -> Vec<anyhow::Result<()>> { panic!("nothing to download") };
            let mut im = InstallationManager::new(&config, &io, &fetch);
            let mut repo = InstalledRepo::in_memory();
            let mut p = (*foo(json!(null))).clone();
            p.transport_options = json!({"symlink": false});
            im.execute(&mut repo, &[Operation::Install(p.into())], true, false, None).unwrap();
            assert_eq!(im.output(), ["  - Installing acme/foo (dev-main): Mirroring from packages/foo"]);
            let target = format!("{dir}/vendor/acme/foo");
            assert!(!std::fs::symlink_metadata(&target).unwrap().is_symlink());
            assert!(Path::new(&format!("{target}/src/Foo.php")).is_file());
            assert!(!Path::new(&format!("{target}/.git")).exists());
        });
    }

    #[test]
    fn refuses_to_install_inside_its_source() {
        in_project("path-inside", |dir| {
            let (config, io) = (config(dir, json!({})), Io::new(Verbosity::Normal, Some(false), false));
            let fetch = |_: Vec<crate::FetchJob>| -> Vec<anyhow::Result<()>> { panic!("nothing to download") };
            let mut im = InstallationManager::new(&config, &io, &fetch);
            let p = package(json!({"name": "acme/root", "version": "dev-main", "dist": {"type": "path", "url": "."}}));
            // like Composer, only detected once the install path exists (realpath fails before)
            std::fs::create_dir_all(format!("{dir}/vendor/acme/root")).unwrap();
            let err = im.execute(&mut InstalledRepo::in_memory(), &[Operation::Install(p)], true, false, None).unwrap_err();
            assert_eq!(err.to_string(), format!("Package acme/root cannot install to \"{dir}/vendor/acme/root\" inside its source at \"{dir}\""));
            let missing = package(json!({"name": "acme/gone", "version": "dev-main", "dist": {"type": "path", "url": "packages/gone"}}));
            let err = im.execute(&mut InstalledRepo::in_memory(), &[Operation::Install(missing)], true, false, None).unwrap_err();
            assert_eq!(err.to_string(), "Source path \"packages/gone\" is not found for package acme/gone");
        });
    }
}
