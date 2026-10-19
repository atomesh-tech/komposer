//! `Factory::createComposer()`: everything a command needs about the current project.

use crate::locker::Locker;
use crate::scripts::Scripts;
use anyhow::{Context, Result, bail};
use komposer_package::config::Config;
use komposer_package::io::Io;
use komposer_package::loader;
use komposer_package::repository::Repository;
use komposer_package::{Package, RootPackage, json};
use komposer_repository::http::HttpClient;
use komposer_repository::installed::InstalledRepository;
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub struct Composer {
    pub io: Arc<Io>,
    /// the project directory (where composer.json lives)
    pub dir: PathBuf,
    pub file: PathBuf,
    /// raw composer.json contents (for the lock file hash)
    pub contents: String,
    pub config: Config,
    pub root: RootPackage,
    pub http: HttpClient,
    pub locker: Locker,
    /// `RepositoryManager`: created once, so metadata loaded by one step is reused by the next.
    /// ponytail: never freed, as they hold every package version an update loaded and freeing those one by one
    /// takes ~20 ms right before the process exits (a leak if Composers are ever created over and over)
    repos: std::mem::ManuallyDrop<std::cell::OnceCell<Vec<Arc<dyn Repository>>>>,
}

/// `Factory::getComposerFile()`.
pub fn composer_file() -> Result<PathBuf> {
    if let Ok(env) = std::env::var("COMPOSER") {
        let env = env.trim();
        if !env.is_empty() {
            if Path::new(env).is_dir() {
                bail!(
                    "The COMPOSER environment variable is set to {env} which is a directory, this variable should point to a composer.json or be left unset."
                );
            }
            return Ok(PathBuf::from(env));
        }
    }
    Ok(PathBuf::from("./composer.json"))
}

/// `Factory::getLockFile()`.
pub fn lock_file(composer_file: &Path) -> PathBuf {
    match composer_file.extension() {
        Some(ext) if ext == "json" => composer_file.with_extension("lock"),
        _ => PathBuf::from(format!("{}.lock", composer_file.display())),
    }
}

impl Composer {
    /// Loads the project in the current directory.
    pub fn load(io: Arc<Io>) -> Result<Composer> {
        let file = composer_file()?;
        let dir = std::env::current_dir()?;
        if !file.exists() {
            let message = if file == Path::new("./composer.json") || file == Path::new("composer.json") {
                format!("Composer could not find a composer.json file in {}", dir.display())
            } else {
                format!("Composer could not find the config file: {}", file.display())
            };
            bail!("{message}\nTo initialize a project, please create a composer.json file. See https://getcomposer.org/basic-usage");
        }
        let contents = std::fs::read_to_string(&file).with_context(|| format!("Could not read {}", file.display()))?;
        let local = json::parse_object(&contents).with_context(|| format!("\"{}\" does not contain valid JSON", file.display()))?;
        Self::from_parts(io, dir, file, contents, local)
    }

    /// Builds a project from an already parsed composer.json.
    pub fn from_parts(io: Arc<Io>, dir: PathBuf, file: PathBuf, contents: String, local: Map<String, Value>) -> Result<Composer> {
        let mut config = Config::create(&dir)?;
        config.merge(&local);
        let local_auth = dir.join("auth.json");
        if local_auth.exists()
            && let Value::Object(auth) = json::read(&local_auth)?
        {
            config.merge_config(&Value::Object(auth));
        }
        let http = HttpClient::new(io.clone(), &config)?;
        // `BaseIO::loadConfiguration()`
        crate::scripts::TIMEOUT.store(config.process_timeout(), std::sync::atomic::Ordering::Relaxed);
        let root = loader::load_root(&local, || komposer_repository::version_guesser::guess_root_version(&local, &dir, &io))?;
        let auto_versioned = !local.contains_key("version") && root.package.pretty_version == komposer_package::ROOT_DEFAULT_PRETTY_VERSION;
        if auto_versioned && root.package.name != komposer_package::ROOT_NAME && root.package.package_type != "project" {
            io.warning(&format!(
                "Composer could not detect the root package ({}) version, defaulting to '1.0.0'. See https://getcomposer.org/root-version",
                root.package.pretty_name
            ));
        }
        let lock_path = if config.get_bool("lock") { lock_file(&file) } else { PathBuf::from("/dev/null") };
        if !config.get_bool("lock") && lock_file(&file).exists() {
            io.warning(&format!("{} is present but ignored as the \"lock\" config option is disabled.", lock_file(&file).display()));
        }
        let locker = Locker::new(&lock_path, &contents)?;
        Ok(Composer { io, dir, file, contents, config, root, http, locker, repos: Default::default() })
    }

    pub fn vendor_dir(&self) -> PathBuf {
        self.config.get_path("vendor-dir")
    }

    pub fn installed_json(&self) -> PathBuf {
        self.vendor_dir().join("composer/installed.json")
    }

    /// The local repository (vendor/composer/installed.json).
    pub fn installed(&self) -> Result<InstalledRepository> {
        InstalledRepository::new(&self.installed_json())
    }

    /// The configured repositories, highest priority first.
    pub fn repositories(&self) -> Result<Vec<Arc<dyn Repository>>> {
        if let Some(repos) = self.repos.get() {
            return Ok(repos.clone());
        }
        let repos = komposer_repository::factory::create_repositories(&self.http, &self.config.repositories())?;
        Ok(self.repos.get_or_init(|| repos).clone())
    }

    /// `InstallationManager::getInstallPath()` for libraries: vendor-dir/name[/target-dir]; none for metapackages.
    pub fn install_path(&self, p: &Package) -> Option<PathBuf> {
        if p.package_type == "metapackage" {
            return None;
        }
        let base = self.vendor_dir().join(&p.pretty_name);
        Some(match &p.target_dir {
            Some(t) if !t.is_empty() => base.join(t),
            _ => base,
        })
    }

    pub fn scripts(&self, enabled: bool) -> Scripts<'_> {
        let scripts = self.root.package.meta.get("scripts").and_then(Value::as_object).cloned().unwrap_or_default();
        let binaries = self.root.package.bin.clone();
        Scripts { io: &self.io, scripts, binaries, bin_dir: self.config.get_path("bin-dir"), cwd: self.dir.clone(), enabled, composer: Some(self) }
    }
}
