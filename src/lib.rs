//! komposer: Composer, the PHP dependency manager, in Rust.

pub mod audit;
pub mod cli;
pub mod commands;
pub mod composer;
pub mod installer;
pub mod json_manipulator;
mod json_schema;
pub mod locker;
pub mod platform_filter;
pub mod scripts;
pub mod suggest;

use cli::{Cli, Command};
use komposer_package::io::Io;
use std::sync::Arc;

/// Runs a parsed command line; returns the exit code.
pub fn run(cli: Cli, io: &Arc<Io>) -> anyhow::Result<i32> {
    if let Some(dir) = &cli.working_dir {
        std::env::set_current_dir(dir)
            .map_err(|e| anyhow::anyhow!("Invalid working directory specified, {} does not exist. ({e})", dir.display()))?;
    }
    if cli.no_cache {
        // SAFETY: nothing else runs yet
        unsafe { std::env::set_var("COMPOSER_CACHE_DIR", "/dev/null") };
    }
    let run_scripts = !cli.no_scripts;
    let Some(command) = cli.command else {
        use clap::CommandFactory;
        Cli::command().print_help()?;
        return Ok(0);
    };
    match command {
        Command::Install(args) => commands::install::install(&mut composer::Composer::load(io.clone())?, &args, run_scripts),
        Command::Update(args) => commands::install::update(&mut composer::Composer::load(io.clone())?, &args, run_scripts),
        Command::CreateProject(args) => commands::create_project::run(io, &args, run_scripts),
        Command::Require(args) => commands::require::require(io, &args, run_scripts),
        Command::Remove(args) => commands::require::remove(io, &args, run_scripts),
        Command::DumpAutoload(args) => commands::dump_autoload::run(&mut composer::Composer::load(io.clone())?, &args, run_scripts),
        Command::RunScript(args) => commands::misc::run_script(&composer::Composer::load(io.clone())?, &args, run_scripts),
        Command::Script(argv) => commands::misc::script_alias(&composer::Composer::load(io.clone())?, &argv, run_scripts),
        Command::Exec(args) => commands::misc::exec(&composer::Composer::load(io.clone())?, &args),
        Command::ClearCache(args) => {
            let config = match composer::Composer::load(io.clone()) {
                Ok(c) => c.config,
                Err(_) => komposer_package::config::Config::create(&std::env::current_dir()?)?,
            };
            commands::misc::clear_cache(io, &config, &args)
        }
        Command::About => commands::misc::about(io),
        Command::Show(args) => commands::show::show(io, &args, run_scripts),
        Command::Outdated(args) => commands::show::outdated(io, &args, run_scripts),
        Command::Depends(args) => commands::depends::depends(io, &args, run_scripts),
        Command::Prohibits(args) => commands::depends::prohibits(io, &args, run_scripts),
        Command::Licenses(args) => commands::info::licenses(io, &args, run_scripts),
        Command::Fund(args) => commands::info::fund(io, &args, run_scripts),
        Command::Suggests(args) => commands::info::suggests(io, &args, run_scripts),
        Command::Browse(args) => commands::info::browse(io, &args, run_scripts),
        Command::Validate(args) => commands::validate::validate(io, &args, run_scripts),
        Command::Audit(args) => commands::audit_cmd::audit(io, &args, run_scripts),
        Command::CheckPlatformReqs(args) => commands::platform_reqs::check_platform_reqs(io, &args, run_scripts),
        Command::Search(args) => commands::search::search(io, &args, run_scripts),
        Command::Bump(args) => commands::bump::bump(io, &args, run_scripts),
        Command::Reinstall(args) => commands::reinstall::reinstall(io, &args, run_scripts),
        Command::Status => commands::misc::status(io, run_scripts),
        Command::Config(args) => commands::config::config(io, &args, run_scripts),
        Command::Init(args) => commands::init::init(io, &args, run_scripts),
        Command::Global(args) => commands::global::global(io, &args, run_scripts),
        Command::Diagnose => commands::misc::diagnose(io),
    }
}
