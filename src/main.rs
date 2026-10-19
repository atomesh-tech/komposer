use clap::Parser;
use komposer::cli::Cli;
use komposer::scripts;
use komposer_package::io::{Io, Verbosity};
use std::io::IsTerminal;
use std::process::ExitCode;
use std::sync::Arc;

// musl's allocator has one global lock, which the threads decoding and loading metadata fight over
#[cfg(target_env = "musl")]
#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() -> ExitCode {
    // what scripts call to run Composer (bin/composer exports its own path)
    if let Ok(exe) = std::env::current_exe() {
        // SAFETY: no other threads yet
        unsafe { std::env::set_var("COMPOSER_BINARY", exe) };
    }
    let mut cli = Cli::parse();
    cli.apply_script_alias_options();
    if cli.version {
        println!("Komposer version {} (Composer {} compatible)", env!("CARGO_PKG_VERSION"), komposer_repository::COMPOSER_VERSION);
        return ExitCode::SUCCESS;
    }
    let verbosity = match (cli.quiet, cli.verbose) {
        (true, _) => Verbosity::Quiet,
        (_, 0) => Verbosity::Normal,
        (_, 1) => Verbosity::Verbose,
        (_, 2) => Verbosity::VeryVerbose,
        _ => Verbosity::Debug,
    };
    let decorated = if cli.ansi {
        Some(true)
    } else if cli.no_ansi {
        Some(false)
    } else {
        None
    };
    let interactive = !cli.no_interaction && std::env::var_os("COMPOSER_NO_INTERACTION").is_none() && std::io::stdin().is_terminal();
    let io = Arc::new(Io::new(verbosity, decorated, interactive));

    match komposer::run(cli, &io) {
        Ok(code) => {
            scripts::skipped_hint(&io);
            ExitCode::from(code.clamp(0, 255) as u8)
        }
        Err(e) => {
            if let Some(failed) = e.downcast_ref::<scripts::ScriptFailed>() {
                return ExitCode::from(failed.0.clamp(1, 255) as u8);
            }
            // `Application::hintCommonErrors()`
            if e.downcast_ref::<scripts::ProcessTimedOut>().is_some() {
                io.write_error_at("<error>The following exception is caused by a process timeout</error>", Verbosity::Quiet);
                io.write_error_at("<error>Check https://getcomposer.org/doc/06-config.md#process-timeout for details</error>", Verbosity::Quiet);
            }
            io.write_error_at("", Verbosity::Quiet);
            for line in format!("{e:#}").lines() {
                io.write_error_at(&format!("  <error>{line}</error>"), Verbosity::Quiet);
            }
            io.write_error_at("", Verbosity::Quiet);
            ExitCode::FAILURE
        }
    }
}
