//! Command line definition, mirroring Composer's commands and options.

use clap::{ArgAction, Args, Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "komposer", version, about = "Dependency Manager for PHP, in Rust", disable_version_flag = true)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Increase the verbosity of messages: 1 for normal output, 2 for more verbose output and 3 for debug
    #[arg(short, long, action = ArgAction::Count, global = true)]
    pub verbose: u8,
    /// Do not output any message
    #[arg(short, long, global = true)]
    pub quiet: bool,
    /// Force ANSI output
    #[arg(long, global = true, overrides_with = "no_ansi")]
    pub ansi: bool,
    /// Disable ANSI output
    #[arg(long, global = true)]
    pub no_ansi: bool,
    /// Do not ask any interactive question
    #[arg(short = 'n', long, global = true)]
    pub no_interaction: bool,
    /// Whether to disable plugins (komposer never runs plugins)
    #[arg(long, global = true)]
    pub no_plugins: bool,
    /// Skips the execution of all scripts defined in composer.json file
    #[arg(long, global = true)]
    pub no_scripts: bool,
    /// If specified, use the given directory as working directory
    #[arg(short = 'd', long, global = true, value_name = "WORKING-DIR")]
    pub working_dir: Option<PathBuf>,
    /// Prevent use of the cache
    #[arg(long, global = true)]
    pub no_cache: bool,
    /// Display timing and memory usage information
    #[arg(long, global = true)]
    pub profile: bool,
    /// Display this application version
    #[arg(short = 'V', long)]
    pub version: bool,
}

impl Cli {
    /// `komposer <script> ...`: the input of a script alias is not validated, but the global options in it apply
    /// like anywhere else (until `--`).
    pub fn apply_script_alias_options(&mut self) {
        let Some(Command::Script(argv)) = &self.command else { return };
        let mut tokens = argv.iter().skip(1).take_while(|t| *t != "--");
        while let Some(token) = tokens.next() {
            match token.as_str() {
                "-v" | "--verbose" => self.verbose = self.verbose.max(1),
                "-vv" => self.verbose = self.verbose.max(2),
                "-vvv" => self.verbose = 3,
                "-q" | "--quiet" => self.quiet = true,
                "--ansi" => (self.ansi, self.no_ansi) = (true, false),
                "--no-ansi" => (self.ansi, self.no_ansi) = (false, true),
                "-n" | "--no-interaction" => self.no_interaction = true,
                "--no-plugins" => self.no_plugins = true,
                "--no-scripts" => self.no_scripts = true,
                "--no-cache" => self.no_cache = true,
                "--profile" => self.profile = true,
                "-d" | "--working-dir" => self.working_dir = tokens.next().map(PathBuf::from),
                t => {
                    if let Some(dir) = t.strip_prefix("--working-dir=").or_else(|| t.strip_prefix("-d").filter(|d| !d.is_empty())) {
                        self.working_dir = Some(PathBuf::from(dir));
                    }
                }
            }
        }
    }
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Installs the project dependencies from the composer.lock file if present, or falls back on the composer.json
    #[command(visible_alias = "i")]
    Install(InstallArgs),
    /// Updates your dependencies to the latest version according to composer.json, and updates the composer.lock file
    #[command(visible_aliases = ["u", "upgrade"])]
    Update(UpdateArgs),
    /// Adds required packages to your composer.json and installs them
    #[command(visible_alias = "r")]
    Require(RequireArgs),
    /// Removes a package from the require or require-dev
    #[command(visible_aliases = ["rm", "uninstall"])]
    Remove(RemoveArgs),
    /// Dumps the autoloader
    #[command(name = "dump-autoload", visible_alias = "dumpautoload")]
    DumpAutoload(DumpAutoloadArgs),
    /// Creates new project from a package into given directory
    #[command(name = "create-project")]
    CreateProject(CreateProjectArgs),
    /// Shows information about packages
    #[command(visible_alias = "info")]
    Show(ShowArgs),
    /// Shows a list of installed packages that have updates available, including their latest version
    Outdated(OutdatedArgs),
    /// Shows which packages cause the given package to be installed
    #[command(visible_alias = "why")]
    Depends(DependsArgs),
    /// Shows which packages prevent the given package from being installed
    #[command(visible_alias = "why-not")]
    Prohibits(ProhibitsArgs),
    /// Runs the scripts defined in composer.json
    #[command(name = "run-script", visible_alias = "run")]
    RunScript(RunScriptArgs),
    /// Executes a vendored binary/script
    Exec(ExecArgs),
    /// Validates a composer.json and composer.lock
    Validate(ValidateArgs),
    /// Clears composer's internal package cache
    #[command(name = "clear-cache", visible_aliases = ["clearcache", "cc"])]
    ClearCache(ClearCacheArgs),
    /// Checks that your PHP and extensions versions match the platform requirements of the installed packages
    #[command(name = "check-platform-reqs")]
    CheckPlatformReqs(CheckPlatformReqsArgs),
    /// Shows information about licenses of dependencies
    Licenses(LicensesArgs),
    /// Discover how to help fund the maintenance of your dependencies
    Fund(FundArgs),
    /// Shows package suggestions
    Suggests(SuggestsArgs),
    /// Checks for security vulnerability advisories for installed packages
    Audit(AuditArgs),
    /// Shows a short information about Composer
    About,
    /// Searches for packages
    Search(SearchArgs),
    /// Increases the lower limit of your composer.json requirements to the currently installed versions
    Bump(BumpArgs),
    /// Uninstalls and reinstalls the given package names
    Reinstall(ReinstallArgs),
    /// Shows a list of locally modified packages
    Status,
    /// Sets config options
    Config(ConfigArgs),
    /// Creates a basic composer.json file in current directory
    Init(InitArgs),
    /// Allows running commands in the global composer dir ($COMPOSER_HOME)
    Global(GlobalArgs),
    /// Opens the package's repository URL or homepage in your browser
    #[command(visible_alias = "home")]
    Browse(BrowseArgs),
    /// Diagnoses the system to identify common errors
    Diagnose,
    /// Runs a script defined in composer.json (`komposer <script>`)
    #[command(external_subcommand)]
    Script(Vec<String>),
}

/// Options shared by commands that install packages.
#[derive(Args, Debug, Default, Clone)]
pub struct InstallFlags {
    /// Forces installation from package sources when possible, including VCS information
    #[arg(long)]
    pub prefer_source: bool,
    /// Forces installation from package dist (default behavior)
    #[arg(long)]
    pub prefer_dist: bool,
    /// Forces installation from package dist|source|auto (auto chooses source for dev versions, dist for the rest)
    #[arg(long, value_name = "PREFER-INSTALL")]
    pub prefer_install: Option<String>,
    /// Outputs the operations but will not execute anything (implicitly enables --verbose)
    #[arg(long)]
    pub dry_run: bool,
    /// Do not output download progress
    #[arg(long)]
    pub no_progress: bool,
    /// DEPRECATED: This flag does not exist anymore
    #[arg(long, hide = true)]
    pub no_suggest: bool,
    /// Optimize autoloader during autoloader dump
    #[arg(short = 'o', long)]
    pub optimize_autoloader: bool,
    /// Autoload classes from the classmap only. Implicitly enables `--optimize-autoloader`
    #[arg(short = 'a', long)]
    pub classmap_authoritative: bool,
    /// Use APCu to cache found/not-found classes
    #[arg(long)]
    pub apcu_autoloader: bool,
    /// Use a custom prefix for the APCu autoloader cache. Implicitly enables --apcu-autoloader
    #[arg(long, value_name = "APCU-AUTOLOADER-PREFIX")]
    pub apcu_autoloader_prefix: Option<String>,
    /// Ignore a specific platform requirement (php & ext- packages)
    #[arg(long, value_name = "IGNORE-PLATFORM-REQ")]
    pub ignore_platform_req: Vec<String>,
    /// Ignore all platform requirements (php & ext- packages)
    #[arg(long)]
    pub ignore_platform_reqs: bool,
    /// Disables all policy blocking during this command
    #[arg(long, alias = "no-security-blocking")]
    pub no_blocking: bool,
    /// Audit output format. Must be "table", "plain", "json", or "summary"
    #[arg(long, default_value = "summary", value_name = "AUDIT-FORMAT")]
    pub audit_format: String,
}

#[derive(Args, Debug)]
pub struct InstallArgs {
    #[command(flatten)]
    pub flags: InstallFlags,
    /// Download only, do not install packages
    #[arg(long)]
    pub download_only: bool,
    /// DEPRECATED: Enables installation of require-dev packages (enabled by default, only present for BC)
    #[arg(long, hide = true)]
    pub dev: bool,
    /// Disables installation of require-dev packages
    #[arg(long)]
    pub no_dev: bool,
    /// Skips autoloader generation
    #[arg(long)]
    pub no_autoloader: bool,
    /// Do not use, only defined here to catch misuse of the install command
    #[arg(long, hide = true)]
    pub no_install: bool,
    /// Run an audit after installation is complete
    #[arg(long)]
    pub audit: bool,
    /// Return a failed status code (6) if PSR-4 or PSR-0 mapping errors are present
    #[arg(long)]
    pub strict_psr_autoloader: bool,
    /// Should not be provided, use komposer require instead to add a given package to composer.json
    pub packages: Vec<String>,
}

#[derive(Args, Debug)]
pub struct UpdateArgs {
    #[command(flatten)]
    pub flags: InstallFlags,
    /// Packages that should be updated, if not provided all packages are
    pub packages: Vec<String>,
    /// Temporary version constraint to add, e.g. foo/bar:1.0.0 or foo/bar=1.0.0
    #[arg(long)]
    pub with: Vec<String>,
    #[arg(long, hide = true)]
    pub dev: bool,
    /// Disables installation of require-dev packages
    #[arg(long)]
    pub no_dev: bool,
    /// Overwrites the lock file hash to suppress warning about the lock file being out of date without updating package versions
    #[arg(long)]
    pub lock: bool,
    /// Skip the install step after updating the composer.lock file
    #[arg(long)]
    pub no_install: bool,
    /// Skip the audit step after updating the composer.lock file
    #[arg(long)]
    pub no_audit: bool,
    /// Skips autoloader generation
    #[arg(long)]
    pub no_autoloader: bool,
    /// Update also dependencies of packages in the argument list, except those which are root requirements
    #[arg(short = 'w', long)]
    pub with_dependencies: bool,
    /// Update also dependencies of packages in the argument list, including those which are root requirements
    #[arg(short = 'W', long)]
    pub with_all_dependencies: bool,
    /// Prefer stable versions of dependencies
    #[arg(long)]
    pub prefer_stable: bool,
    /// Prefer lowest versions of dependencies
    #[arg(long)]
    pub prefer_lowest: bool,
    /// Only perform absolutely necessary changes to dependencies
    #[arg(short = 'm', long)]
    pub minimal_changes: bool,
    /// Only allow patch version updates for currently installed dependencies
    #[arg(long)]
    pub patch_only: bool,
    /// Restricts the update to your first degree dependencies
    #[arg(long)]
    pub root_reqs: bool,
    /// Runs bump after performing the update (dev, no-dev or all)
    #[arg(long, num_args = 0..=1, default_missing_value = "all", value_name = "BUMP-AFTER-UPDATE")]
    pub bump_after_update: Option<String>,
    /// Return a failed status code (6) if PSR-4 or PSR-0 mapping errors are present
    #[arg(long)]
    pub strict_psr_autoloader: bool,
}

#[derive(Args, Debug)]
pub struct RequireArgs {
    #[command(flatten)]
    pub flags: InstallFlags,
    /// Package name(s), optionally with a version constraint, e.g. foo/bar or foo/bar:1.0.0 or "foo/bar 1.0.0"
    pub packages: Vec<String>,
    /// Add requirement to require-dev
    #[arg(long)]
    pub dev: bool,
    /// Write fixed version to the composer.json
    #[arg(long)]
    pub fixed: bool,
    /// Disables the automatic update of the dependencies (implies --no-install)
    #[arg(long)]
    pub no_update: bool,
    /// Skip the install step after updating the composer.lock file
    #[arg(long)]
    pub no_install: bool,
    /// Skip the audit step after updating the composer.lock file
    #[arg(long)]
    pub no_audit: bool,
    /// Run the dependency update with the --no-dev option
    #[arg(long)]
    pub update_no_dev: bool,
    /// Allows inherited dependencies to be updated, except those that are root requirements
    #[arg(short = 'w', long, alias = "with-dependencies")]
    pub update_with_dependencies: bool,
    /// Allows all inherited dependencies to be updated, including those that are root requirements
    #[arg(short = 'W', long, alias = "with-all-dependencies")]
    pub update_with_all_dependencies: bool,
    /// Prefer stable versions of dependencies
    #[arg(long)]
    pub prefer_stable: bool,
    /// Prefer lowest versions of dependencies
    #[arg(long)]
    pub prefer_lowest: bool,
    /// During an update with -w/-W, only perform absolutely necessary changes to transitive dependencies
    #[arg(short = 'm', long)]
    pub minimal_changes: bool,
    /// Sorts packages when adding/updating a new dependency
    #[arg(long)]
    pub sort_packages: bool,
}

#[derive(Args, Debug)]
pub struct RemoveArgs {
    #[command(flatten)]
    pub flags: InstallFlags,
    /// Packages that should be removed
    pub packages: Vec<String>,
    /// Removes a package from the require-dev section
    #[arg(long)]
    pub dev: bool,
    /// Disables the automatic update of the dependencies (implies --no-install)
    #[arg(long)]
    pub no_update: bool,
    /// Skip the install step after updating the composer.lock file
    #[arg(long)]
    pub no_install: bool,
    /// Skip the audit step after updating the composer.lock file
    #[arg(long)]
    pub no_audit: bool,
    /// Run the dependency update with the --no-dev option
    #[arg(long)]
    pub update_no_dev: bool,
    /// (Deprecated, is now default behavior)
    #[arg(short = 'w', long, hide = true)]
    pub update_with_dependencies: bool,
    /// Allows all inherited dependencies to be updated, including those that are root requirements
    #[arg(short = 'W', long, alias = "with-all-dependencies")]
    pub update_with_all_dependencies: bool,
    /// Does not allow inherited dependencies to be updated with explicit dependencies
    #[arg(long)]
    pub no_update_with_dependencies: bool,
    /// During an update with -w/-W, only perform absolutely necessary changes to transitive dependencies
    #[arg(short = 'm', long)]
    pub minimal_changes: bool,
    /// Remove all packages which are locked but not required by any other package
    #[arg(long)]
    pub unused: bool,
}

#[derive(Args, Debug)]
pub struct DumpAutoloadArgs {
    /// Optimizes PSR0 and PSR4 packages to be loaded with classmaps too, good for production
    #[arg(short = 'o', long)]
    pub optimize: bool,
    /// Autoload classes from the classmap only. Implicitly enables `--optimize`
    #[arg(short = 'a', long)]
    pub classmap_authoritative: bool,
    /// Use APCu to cache found/not-found classes
    #[arg(long)]
    pub apcu: bool,
    /// Use a custom prefix for the APCu autoloader cache. Implicitly enables --apcu
    #[arg(long)]
    pub apcu_prefix: Option<String>,
    /// Outputs the operations but will not execute anything
    #[arg(long)]
    pub dry_run: bool,
    /// Enables autoload-dev rules
    #[arg(long)]
    pub dev: bool,
    /// Disables autoload-dev rules
    #[arg(long)]
    pub no_dev: bool,
    /// Ignore a specific platform requirement (php & ext- packages)
    #[arg(long)]
    pub ignore_platform_req: Vec<String>,
    /// Ignore all platform requirements (php & ext- packages)
    #[arg(long)]
    pub ignore_platform_reqs: bool,
    /// Return a failed status code (1) if PSR-4 or PSR-0 mapping errors are present
    #[arg(long)]
    pub strict_psr: bool,
    /// Return a failed status code (2) if the same class is found in multiple files
    #[arg(long)]
    pub strict_ambiguous: bool,
}

#[derive(Args, Debug)]
pub struct CreateProjectArgs {
    #[command(flatten)]
    pub flags: InstallFlags,
    /// Package name to be installed
    pub package: Option<String>,
    /// Directory where the files should be created
    pub directory: Option<PathBuf>,
    /// Version, will default to latest
    pub version: Option<String>,
    /// Minimum-stability allowed (unless a version is specified)
    #[arg(short = 's', long)]
    pub stability: Option<String>,
    /// Add custom repositories to look the package up, either by URL or using JSON arrays
    #[arg(long)]
    pub repository: Vec<String>,
    /// DEPRECATED: Use --repository instead
    #[arg(long, hide = true)]
    pub repository_url: Option<String>,
    /// Add the custom repository in the composer.json
    #[arg(long)]
    pub add_repository: bool,
    /// Require additional package(s) to composer.json after installing the project
    #[arg(long)]
    pub require: Vec<String>,
    #[arg(long, hide = true)]
    pub dev: bool,
    /// Disables installation of require-dev packages
    #[arg(long)]
    pub no_dev: bool,
    /// Disable the secure-http config option temporarily while installing the root package
    #[arg(long)]
    pub no_secure_http: bool,
    /// Whether to prevent deleting the vcs folder
    #[arg(long)]
    pub keep_vcs: bool,
    /// Whether to force deletion of the vcs folder without prompting
    #[arg(long)]
    pub remove_vcs: bool,
    /// Whether to skip installation of the package dependencies
    #[arg(long)]
    pub no_install: bool,
    /// Whether to skip auditing of the installed package dependencies
    #[arg(long)]
    pub no_audit: bool,
    /// Whether to ask for project directory
    #[arg(long)]
    pub ask: bool,
}

#[derive(Args, Debug)]
pub struct ShowArgs {
    /// Package to inspect. Or a name including a wildcard (*) to filter lists of packages instead
    pub package: Option<String>,
    /// Version or version constraint to inspect
    pub version: Option<String>,
    /// List all packages
    #[arg(long)]
    pub all: bool,
    /// List all locked packages
    #[arg(long)]
    pub locked: bool,
    /// List installed packages only (enabled by default, only present for BC)
    #[arg(short = 'i', long)]
    pub installed: bool,
    /// List platform packages only
    #[arg(short = 'p', long)]
    pub platform: bool,
    /// List available packages only
    #[arg(short = 'a', long)]
    pub available: bool,
    /// Show the root package information
    #[arg(short = 's', long = "self")]
    pub self_: bool,
    /// List package names only
    #[arg(short = 'N', long)]
    pub name_only: bool,
    /// Show package paths
    #[arg(short = 'P', long)]
    pub path: bool,
    /// List the dependencies as a tree
    #[arg(short = 't', long)]
    pub tree: bool,
    /// Show the latest version
    #[arg(short = 'l', long)]
    pub latest: bool,
    /// Show the latest version but only for packages that are outdated
    #[arg(short = 'o', long)]
    pub outdated: bool,
    /// Ignore specified package(s). Can contain wildcards (*)
    #[arg(long)]
    pub ignore: Vec<String>,
    /// Show only packages that have major SemVer-compatible updates
    #[arg(short = 'M', long)]
    pub major_only: bool,
    /// Show only packages that have minor SemVer-compatible updates
    #[arg(short = 'm', long)]
    pub minor_only: bool,
    /// Show only packages that have patch SemVer-compatible updates
    #[arg(long)]
    pub patch_only: bool,
    /// Displays the installed version's age, and sorts packages oldest first
    #[arg(short = 'A', long)]
    pub sort_by_age: bool,
    /// Shows only packages that are directly required by the root package
    #[arg(short = 'D', long)]
    pub direct: bool,
    /// Return a non-zero exit code when there are outdated packages
    #[arg(long)]
    pub strict: bool,
    /// Format of the output: text or json
    #[arg(short = 'f', long, default_value = "text")]
    pub format: String,
    /// Disables search in require-dev packages
    #[arg(long)]
    pub no_dev: bool,
    /// Ignore a specific platform requirement (php & ext- packages)
    #[arg(long)]
    pub ignore_platform_req: Vec<String>,
    /// Ignore all platform requirements (php & ext- packages)
    #[arg(long)]
    pub ignore_platform_reqs: bool,
}

#[derive(Args, Debug)]
pub struct OutdatedArgs {
    /// A package name or a pattern to filter the list
    pub package: Option<String>,
    /// Show only packages that are outdated (this is the default, but present here for compat with `show`)
    #[arg(short = 'o', long)]
    pub outdated: bool,
    /// Show all installed packages with their latest versions
    #[arg(short = 'a', long)]
    pub all: bool,
    /// Shows updates for packages from the lock file, regardless of what is currently in vendor dir
    #[arg(long)]
    pub locked: bool,
    /// Shows only packages that are directly required by the root package
    #[arg(short = 'D', long)]
    pub direct: bool,
    /// Return a non-zero exit code when there are outdated packages
    #[arg(long)]
    pub strict: bool,
    /// Show only packages that have major SemVer-compatible updates
    #[arg(short = 'M', long)]
    pub major_only: bool,
    /// Show only packages that have minor SemVer-compatible updates
    #[arg(short = 'm', long)]
    pub minor_only: bool,
    /// Show only packages that have patch SemVer-compatible updates
    #[arg(short = 'p', long)]
    pub patch_only: bool,
    /// Displays the installed version's age, and sorts packages oldest first
    #[arg(short = 'A', long)]
    pub sort_by_age: bool,
    /// Format of the output: text or json
    #[arg(short = 'f', long, default_value = "text")]
    pub format: String,
    /// Ignore specified package(s)
    #[arg(long)]
    pub ignore: Vec<String>,
    /// Disables search in require-dev packages
    #[arg(long)]
    pub no_dev: bool,
    #[arg(long)]
    pub ignore_platform_req: Vec<String>,
    #[arg(long)]
    pub ignore_platform_reqs: bool,
}

#[derive(Args, Debug)]
pub struct DependsArgs {
    /// Package to inspect
    pub package: String,
    /// Recursively resolves up to the root package
    #[arg(short = 'r', long)]
    pub recursive: bool,
    /// Prints the results as a nested tree
    #[arg(short = 't', long)]
    pub tree: bool,
    /// Read dependency information from composer.lock
    #[arg(long)]
    pub locked: bool,
}

#[derive(Args, Debug)]
pub struct ProhibitsArgs {
    /// Package to inspect
    pub package: String,
    /// Version constraint, which version you expected to be installed
    pub version: String,
    /// Recursively resolves up to the root package
    #[arg(short = 'r', long)]
    pub recursive: bool,
    /// Prints the results as a nested tree
    #[arg(short = 't', long)]
    pub tree: bool,
    /// Read dependency information from composer.lock
    #[arg(long)]
    pub locked: bool,
}

#[derive(Args, Debug)]
pub struct RunScriptArgs {
    /// Script name to run
    pub script: Option<String>,
    /// Arguments passed to the script; options before <info>--</info> are komposer's
    pub args: Vec<String>,
    /// Sets script timeout in seconds, or 0 for never
    #[arg(long)]
    pub timeout: Option<String>,
    /// Sets the dev mode
    #[arg(long)]
    pub dev: bool,
    /// Disables the dev mode
    #[arg(long)]
    pub no_dev: bool,
    /// List scripts
    #[arg(short = 'l', long)]
    pub list: bool,
}

#[derive(Args, Debug)]
pub struct ExecArgs {
    /// The binary to run, e.g. phpunit
    pub binary: Option<String>,
    /// Arguments to pass to the binary. Use <info>--</info> to separate from composer arguments
    pub args: Vec<String>,
    #[arg(short = 'l', long)]
    pub list: bool,
}

#[derive(Args, Debug)]
pub struct ValidateArgs {
    /// Path to composer.json file
    pub file: Option<PathBuf>,
    /// Do not validate requires for overly strict/loose constraints
    #[arg(long)]
    pub no_check_all: bool,
    /// Check if lock file is up to date (even when config.lock is false)
    #[arg(long)]
    pub check_lock: bool,
    /// Do not check if lock file is up to date
    #[arg(long)]
    pub no_check_lock: bool,
    /// Do not check for publish errors
    #[arg(long)]
    pub no_check_publish: bool,
    /// Do not report a warning if the version field is present
    #[arg(long)]
    pub no_check_version: bool,
    /// Also validate the composer.json of all installed dependencies
    #[arg(short = 'A', long)]
    pub with_dependencies: bool,
    /// Return a non-zero exit code for warnings as well as errors
    #[arg(long)]
    pub strict: bool,
}

#[derive(Args, Debug)]
pub struct ClearCacheArgs {
    /// Only run garbage collection, not a full cache clear
    #[arg(long)]
    pub gc: bool,
}

#[derive(Args, Debug)]
pub struct CheckPlatformReqsArgs {
    /// Disables checking of require-dev packages requirements
    #[arg(long)]
    pub no_dev: bool,
    /// Checks requirements only from the lock file, not from installed packages
    #[arg(long)]
    pub lock: bool,
    /// Format of the output: text or json
    #[arg(short = 'f', long, default_value = "text")]
    pub format: String,
}

#[derive(Args, Debug)]
pub struct LicensesArgs {
    /// Format of the output: text, json or summary
    #[arg(short = 'f', long, default_value = "text")]
    pub format: String,
    /// Disables search in require-dev packages
    #[arg(long)]
    pub no_dev: bool,
    /// Shows licenses from the lock file instead of installed packages
    #[arg(long)]
    pub locked: bool,
}

#[derive(Args, Debug)]
pub struct SuggestsArgs {
    /// Packages that you want to list suggestions from
    pub packages: Vec<String>,
    /// Group output by package
    #[arg(long)]
    pub by_package: bool,
    /// Group output by suggestion
    #[arg(long)]
    pub by_suggestion: bool,
    /// Show suggestions from all dependencies, including transitive ones
    #[arg(short = 'a', long)]
    pub all: bool,
    /// Show only list of suggested package names
    #[arg(long)]
    pub list: bool,
    /// Exclude suggestions from require-dev packages
    #[arg(long)]
    pub no_dev: bool,
}

#[derive(Args, Debug)]
pub struct AuditArgs {
    /// Disables auditing of require-dev packages
    #[arg(long)]
    pub no_dev: bool,
    /// Output format. Must be "table", "plain", "json", or "summary"
    #[arg(short = 'f', long, default_value = "table")]
    pub format: String,
    /// Audit based on the lock file instead of the installed packages
    #[arg(long)]
    pub locked: bool,
    /// Behavior on abandoned packages. Must be "ignore", "report", or "fail"
    #[arg(long)]
    pub abandoned: Option<String>,
    /// Ignore advisories of a certain severity level
    #[arg(long, value_parser = ["low", "medium", "high", "critical"])]
    pub ignore_severity: Vec<String>,
    /// Ignore repositories that are unreachable or return a non-200 status code
    #[arg(long)]
    pub ignore_unreachable: bool,
}

#[derive(Args, Debug)]
pub struct SearchArgs {
    /// tokens to search for
    #[arg(required = true)]
    pub tokens: Vec<String>,
    /// Search only in package names
    #[arg(short = 'N', long)]
    pub only_name: bool,
    /// Search only for vendor / organization names, returns only "vendor" as result
    #[arg(short = 'O', long)]
    pub only_vendor: bool,
    /// Search for a specific package type
    #[arg(short = 't', long = "type")]
    pub package_type: Option<String>,
    /// Format of the output: text or json
    #[arg(short = 'f', long, default_value = "text")]
    pub format: String,
}

#[derive(Args, Debug)]
pub struct BumpArgs {
    /// Optional package name(s) to restrict which packages are bumped
    pub packages: Vec<String>,
    /// Only bump requirements in "require-dev"
    #[arg(short = 'D', long)]
    pub dev_only: bool,
    /// Only bump requirements in "require"
    #[arg(short = 'R', long)]
    pub no_dev_only: bool,
    /// Outputs the packages to bump, but will not execute anything
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Args, Debug)]
pub struct ReinstallArgs {
    /// List of package names to reinstall, can include a wildcard (*) to match any substring
    pub packages: Vec<String>,
    #[command(flatten)]
    pub flags: InstallFlags,
    /// Skips autoloader generation
    #[arg(long)]
    pub no_autoloader: bool,
    /// Filter packages to reinstall by type(s)
    #[arg(long = "type")]
    pub types: Vec<String>,
}

#[derive(Args, Debug)]
pub struct ConfigArgs {
    /// Setting key
    pub setting_key: Option<String>,
    /// Setting value
    pub setting_value: Vec<String>,
    /// Apply command to the global config file
    #[arg(short = 'g', long)]
    pub global: bool,
    /// Open editor
    #[arg(short = 'e', long)]
    pub editor: bool,
    /// Affect auth config file (only used for --editor)
    #[arg(short = 'a', long)]
    pub auth: bool,
    /// Unset the given setting-key
    #[arg(long)]
    pub unset: bool,
    /// List configuration settings
    #[arg(short = 'l', long)]
    pub list: bool,
    /// If you want to choose a different composer.json or config.json
    #[arg(short = 'f', long)]
    pub file: Option<PathBuf>,
    /// Returns absolute paths when fetching *-dir config values instead of relative
    #[arg(long)]
    pub absolute: bool,
    /// JSON decode the setting value, to be used with extra.* keys
    #[arg(short = 'j', long)]
    pub json: bool,
    /// Merge the setting value with the current value, to be used with extra.* or audit.ignore[-abandoned] keys in combination with --json
    #[arg(short = 'm', long)]
    pub merge: bool,
    /// When adding a repository, append it (lowest priority) to the existing ones instead of prepending it
    #[arg(long)]
    pub append: bool,
    /// Display where the config value is loaded from
    #[arg(long)]
    pub source: bool,
}

#[derive(Args, Debug, Clone)]
pub struct InitArgs {
    /// Name of the package
    #[arg(long)]
    pub name: Option<String>,
    /// Description of package
    #[arg(long)]
    pub description: Option<String>,
    /// Author name of package
    #[arg(long)]
    pub author: Option<String>,
    /// Type of package (e.g. library, project, metapackage, composer-plugin)
    #[arg(long = "type")]
    pub package_type: Option<String>,
    /// Homepage of package
    #[arg(long)]
    pub homepage: Option<String>,
    /// Package to require with a version constraint, e.g. foo/bar:1.0.0
    #[arg(long)]
    pub require: Vec<String>,
    /// Package to require for development with a version constraint, e.g. foo/bar:1.0.0
    #[arg(long)]
    pub require_dev: Vec<String>,
    /// Minimum stability (empty or one of: stable, RC, beta, alpha, dev)
    #[arg(short = 's', long)]
    pub stability: Option<String>,
    /// License of package
    #[arg(short = 'l', long)]
    pub license: Option<String>,
    /// Add custom repositories, either by URL or using JSON arrays
    #[arg(long)]
    pub repository: Vec<String>,
    /// Add PSR-4 autoload mapping. Maps your package's namespace to the provided directory (e.g. src/)
    #[arg(short = 'a', long)]
    pub autoload: Option<String>,
}

#[derive(Args, Debug)]
pub struct GlobalArgs {
    /// The command to run in the global directory
    pub command_name: String,
    /// Arguments of the command
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub args: Vec<String>,
}

#[derive(Args, Debug)]
pub struct BrowseArgs {
    /// Package(s) to browse to
    pub packages: Vec<String>,
    /// Open the homepage instead of the repository URL
    #[arg(short = 'H', long)]
    pub homepage: bool,
    /// Only show the homepage or repository URL
    #[arg(short = 's', long)]
    pub show: bool,
}

#[derive(Args, Debug)]
pub struct FundArgs {
    /// Format of the output: text or json
    #[arg(short = 'f', long, default_value = "text")]
    pub format: String,
}
