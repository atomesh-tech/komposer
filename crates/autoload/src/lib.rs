//! Composer's autoloader generation: `Composer\Autoload\AutoloadGenerator` (2.10.3) and the
//! `composer/class-map-generator` (1.7.3) it scans classes with.

mod classmap;
mod generator;
mod parser;
mod php;

pub use classmap::{ClassMap, ClassMapGenerator, Scan};
pub use generator::{AutoloadOptions, CLASS_LOADER, LoaderMap, PlatformReqFilter, dump, loader_map};
pub use parser::find_classes;
