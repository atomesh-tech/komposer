//! `global` (`GlobalCommand`): runs a command in the global composer dir (`COMPOSER_HOME`).

use crate::cli::{Cli, Command, GlobalArgs};
use anyhow::{Context, Result, bail};
use clap::Parser;
use komposer_package::config::Config;
use komposer_package::io::{Io, Verbosity};
use std::sync::Arc;

pub fn global(io: &Arc<Io>, args: &GlobalArgs, run_scripts: bool) -> Result<i32> {
    // the COMPOSER env var does not apply to the global execution scope
    // SAFETY: no other threads run yet
    unsafe { std::env::remove_var("COMPOSER") };
    let home = Config::create(&std::env::current_dir()?)?.home();
    if home.exists() && !home.is_dir() {
        bail!("{} exists and is not a directory.", home.display());
    }
    std::fs::create_dir_all(&home).with_context(|| format!("{} does not exist and could not be created", home.display()))?;
    std::env::set_current_dir(&home).with_context(|| format!("Could not switch to home directory \"{}\"", home.display()))?;
    io.write_error(&format!("<info>Changed current directory to {}</info>", home.display()));

    let argv = ["komposer", &args.command_name].into_iter().map(String::from).chain(args.args.iter().cloned());
    let mut cli = Cli::try_parse_from(argv)?;
    cli.no_scripts |= !run_scripts;
    let io = sub_io(io, &cli);
    let installs = matches!(cli.command, Some(Command::Install(_) | Command::Update(_) | Command::Require(_)));
    let code = crate::run(cli, &io)?;
    if installs && code == 0 && crate::scripts::find_php().is_none() {
        io.write_error("<comment>Note: global packages are PHP tools, running them (e.g. from vendor/bin) still requires PHP.</comment>");
    }
    Ok(code)
}

/// The IO with the output options given after the command name (`global require -v ...`).
fn sub_io(io: &Arc<Io>, cli: &Cli) -> Arc<Io> {
    if !(cli.quiet || cli.verbose > 0 || cli.ansi || cli.no_ansi || cli.no_interaction) {
        return io.clone();
    }
    let verbosity = match (cli.quiet, cli.verbose) {
        (true, _) => Verbosity::Quiet,
        (_, 0) => io.verbosity(),
        (_, 1) => Verbosity::Verbose,
        (_, 2) => Verbosity::VeryVerbose,
        _ => Verbosity::Debug,
    };
    let decorated = if cli.ansi {
        true
    } else if cli.no_ansi {
        false
    } else {
        io.is_decorated()
    };
    Arc::new(Io::new(verbosity, Some(decorated), io.is_interactive() && !cli.no_interaction))
}
