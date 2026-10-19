//! Composer's repositories (`composer`, `vcs`/git, `path`, `package`, `artifact`, platform,
//! installed.json) and the plumbing behind them: HTTP with Composer's auth handling, the
//! cache, git mirrors and the root version guesser.

pub mod artifact;
pub mod auth;
pub mod cache;
pub mod composer;
pub mod factory;
pub mod git;
pub mod http;
pub mod installed;
pub mod package_repository;
pub mod path;
pub mod platform;
pub mod url;
mod util;
pub mod vcs;
pub mod version_guesser;

#[cfg(test)]
mod test_support;

pub use util::ProcessOutput;

/// `Composer::VERSION` of the release komposer mirrors.
pub const COMPOSER_VERSION: &str = "2.10.3";
/// `PluginInterface::PLUGIN_API_VERSION` of Composer 2.10.3.
pub const PLUGIN_API_VERSION: &str = "2.9.0";
/// `Composer::RUNTIME_API_VERSION`.
pub const RUNTIME_API_VERSION: &str = "2.2.2";
