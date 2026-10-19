//! Composer's package model: `Package`, `AliasPackage`, `RootPackage`, `Link`,
//! `ArrayLoader`, `RootPackageLoader`, `ArrayDumper`, and PHP-compatible JSON.

pub mod config;
pub mod dumper;
pub mod io;
pub mod json;
pub mod loader;
pub mod operation;
mod package;
pub mod repository;

pub use package::*;

use regex::Regex;
use std::sync::LazyLock;

/// `PlatformRepository::isPlatformPackage()`.
pub fn is_platform_package(name: &str) -> bool {
    static RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)^(?:php(?:-64bit|-ipv6|-zts|-debug)?|hhvm|(?:ext|lib)-[a-z0-9](?:[_.-]?[a-z0-9]+)*|composer(?:-(?:plugin|runtime)-api)?)$")
            .unwrap()
    });
    RE.is_match(name)
}
