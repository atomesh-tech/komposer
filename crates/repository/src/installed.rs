//! `InstalledFilesystemRepository` (read side): `vendor/composer/installed.json`.

use anyhow::Result;
use komposer_package::PackageRef;
use komposer_package::repository::{ArrayRepository, LoadRequest, LoadResult, RepoKind, Repository, read_installed_json};
use std::path::{Path, PathBuf};

pub struct InstalledRepository {
    path: PathBuf,
    repo: ArrayRepository,
    dev_mode: Option<bool>,
    dev_package_names: Vec<String>,
}

impl InstalledRepository {
    /// Reads `path`, either `{"packages": [...], "dev": .., "dev-package-names": [...]}` or the
    /// Composer 1 plain list. A missing file is an empty repository.
    pub fn new(path: &Path) -> Result<InstalledRepository> {
        let installed = read_installed_json(path)?;
        Ok(InstalledRepository {
            path: path.to_path_buf(),
            repo: installed.packages,
            dev_mode: installed.dev_mode,
            dev_package_names: installed.dev_package_names,
        })
    }

    /// `getDevMode()`: `None` for files written before Composer 2.
    pub fn dev_mode(&self) -> Option<bool> {
        self.dev_mode
    }

    /// `getDevPackageNames()`.
    pub fn dev_package_names(&self) -> &[String] {
        &self.dev_package_names
    }

    /// `isFresh()`: nothing installed yet.
    pub fn is_fresh(&self) -> bool {
        !self.path.exists()
    }
}

impl Repository for InstalledRepository {
    /// `'installed '.ArrayRepository::getRepoName()`.
    fn repo_name(&self) -> String {
        let n = self.repo.len();
        format!("installed array repo (defining {n} package{})", if n > 1 { "s" } else { "" })
    }

    fn kind(&self) -> RepoKind {
        RepoKind::Installed
    }

    fn load_packages(&self, req: &LoadRequest) -> Result<LoadResult> {
        self.repo.load_packages(req)
    }

    fn packages(&self) -> Result<Vec<PackageRef>> {
        self.repo.packages()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::temp_dir;

    #[test]
    fn reads_both_formats() {
        let dir = temp_dir("installed");
        let file = dir.join("installed.json");
        std::fs::write(&file, r#"[{"name": "package1", "version": "1.0.0-beta", "type": "vendor"}]"#).unwrap();
        let repo = InstalledRepository::new(&file).unwrap();
        let packages = repo.packages().unwrap();
        assert_eq!(
            (packages[0].name.as_str(), packages[0].version.as_str(), packages[0].package_type.as_str()),
            ("package1", "1.0.0.0-beta", "vendor")
        );
        assert_eq!(repo.dev_mode(), None);

        std::fs::write(
            &file,
            r#"{"packages": [{"name": "a/b", "version": "dev-main", "version_normalized": "dev-main", "extra": {"branch-alias": {"dev-main": "1.x-dev"}}, "transport-options": {"symlink": true}}], "dev": true, "dev-package-names": ["a/b"]}"#,
        )
        .unwrap();
        let repo = InstalledRepository::new(&file).unwrap();
        assert_eq!(repo.packages().unwrap().len(), 2);
        assert_eq!(repo.packages().unwrap()[0].transport_options["symlink"], true);
        assert_eq!((repo.dev_mode(), repo.dev_package_names()), (Some(true), &["a/b".to_string()][..]));
        assert_eq!(repo.repo_name(), "installed array repo (defining 2 packages)");
        assert!(!repo.is_fresh());
    }

    #[test]
    fn corrupted_and_missing_files() {
        let dir = temp_dir("installed-bad");
        let file = dir.join("installed.json");
        std::fs::write(&file, "\"foo\"").unwrap();
        assert!(InstalledRepository::new(&file).err().unwrap().to_string().starts_with("Invalid repository data in"));
        std::fs::write(&file, "{nope").unwrap();
        assert!(InstalledRepository::new(&file).is_err());
        let missing = InstalledRepository::new(&dir.join("missing.json")).unwrap();
        assert!(missing.packages().unwrap().is_empty());
        assert!(missing.is_fresh());
    }
}
