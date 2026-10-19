//! `--ignore-platform-req(s)` for the autoload generator's platform_check.php.

use crate::commands::install::platform_filter;
use komposer_autoload::PlatformReqFilter;
use komposer_solver::PlatformRequirementFilter;

/// `PlatformRequirementFilterInterface::isIgnored()` as the autoload generator takes it.
pub fn autoload(ignore_all: bool, list: &[String]) -> Option<Box<PlatformReqFilter>> {
    let filter = platform_filter(ignore_all, list);
    if matches!(filter, PlatformRequirementFilter::IgnoreNothing) {
        return None;
    }
    Some(Box::new(move |name: &str| filter.is_ignored(name)))
}
