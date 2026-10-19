use komposer_semver::{Constraint, DEFAULT_BRANCH_ALIAS, Op, Stability};
use serde_json::{Map, Value};
use std::fmt;
use std::sync::Arc;

pub type PackageRef = Arc<Package>;

/// `Link::TYPE_*`, in `BasePackage::$supportedLinkTypes` order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LinkType {
    Require,
    Conflict,
    Provide,
    Replace,
    DevRequire,
}

impl LinkType {
    pub const ALL: [LinkType; 5] = [LinkType::Require, LinkType::Conflict, LinkType::Provide, LinkType::Replace, LinkType::DevRequire];

    /// The composer.json key.
    pub fn key(self) -> &'static str {
        match self {
            LinkType::Require => "require",
            LinkType::Conflict => "conflict",
            LinkType::Provide => "provide",
            LinkType::Replace => "replace",
            LinkType::DevRequire => "require-dev",
        }
    }

    /// `Link::getDescription()`.
    pub fn description(self) -> &'static str {
        match self {
            LinkType::Require => "requires",
            LinkType::Conflict => "conflicts",
            LinkType::Provide => "provides",
            LinkType::Replace => "replaces",
            LinkType::DevRequire => "requires (for development)",
        }
    }
}

/// `Composer\Package\Link`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub source: String,
    pub target: String,
    pub constraint: Constraint,
    pub kind: LinkType,
    pub pretty_constraint: String,
}

impl Link {
    /// `Link::getPrettyString($sourcePackage)`.
    pub fn pretty_string(&self, source: &Package) -> String {
        format!("{} {} {} {}", source.pretty_string(), self.kind.description(), self.target, self.pretty_constraint)
    }
}

impl fmt::Display for Link {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {} {} ({})", self.source, self.kind.description(), self.target, self.constraint)
    }
}

/// `PackageInterface::DISPLAY_*`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisplayMode {
    SourceRefIfDev,
    SourceRef,
    DistRef,
}

/// `source` or `dist` of a package.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Location {
    pub kind: String,
    pub url: String,
    pub reference: Option<String>,
    /// dist only
    pub shasum: Option<String>,
    pub mirrors: Option<Value>,
}

/// `CompletePackage` / `CompleteAliasPackage`. Fields Composer only carries around for
/// display or lock files (description, authors, license...) live in `meta`, keyed
/// by their composer.json name and already normalized the way `ArrayLoader` does.
#[derive(Clone, Debug)]
pub struct Package {
    /// lowercased
    pub name: String,
    pub pretty_name: String,
    /// normalized
    pub version: String,
    pub pretty_version: String,
    pub stability: Stability,
    pub package_type: String,
    pub target_dir: Option<String>,
    pub source: Option<Location>,
    pub dist: Option<Location>,
    pub requires: Vec<Link>,
    pub conflicts: Vec<Link>,
    pub provides: Vec<Link>,
    pub replaces: Vec<Link>,
    pub dev_requires: Vec<Link>,
    pub suggests: Map<String, Value>,
    /// RFC 3339
    pub time: Option<String>,
    pub default_branch: bool,
    pub bin: Vec<String>,
    pub extra: Value,
    pub installation_source: Option<String>,
    pub autoload: Value,
    pub autoload_dev: Value,
    pub notification_url: Option<String>,
    pub include_path: Value,
    pub php_ext: Value,
    pub meta: Map<String, Value>,
    pub transport_options: Value,
    /// Set on `AliasPackage`s.
    pub alias_of: Option<PackageRef>,
    pub root_package_alias: bool,
    pub has_self_version_requires: bool,
}

impl Package {
    pub fn new(name: &str, version: &str, pretty_version: &str) -> Package {
        Package {
            name: name.to_lowercase(),
            pretty_name: name.to_string(),
            version: version.to_string(),
            pretty_version: pretty_version.to_string(),
            stability: komposer_semver::parse_stability(version),
            package_type: "library".into(),
            target_dir: None,
            source: None,
            dist: None,
            requires: vec![],
            conflicts: vec![],
            provides: vec![],
            replaces: vec![],
            dev_requires: vec![],
            suggests: Map::new(),
            time: None,
            default_branch: false,
            bin: vec![],
            extra: Value::Array(vec![]),
            installation_source: None,
            autoload: Value::Array(vec![]),
            autoload_dev: Value::Array(vec![]),
            notification_url: None,
            include_path: Value::Array(vec![]),
            php_ext: Value::Null,
            meta: Map::new(),
            transport_options: Value::Array(vec![]),
            alias_of: None,
            root_package_alias: false,
            has_self_version_requires: false,
        }
    }

    pub fn links(&self, kind: LinkType) -> &[Link] {
        match kind {
            LinkType::Require => &self.requires,
            LinkType::Conflict => &self.conflicts,
            LinkType::Provide => &self.provides,
            LinkType::Replace => &self.replaces,
            LinkType::DevRequire => &self.dev_requires,
        }
    }

    pub fn links_mut(&mut self, kind: LinkType) -> &mut Vec<Link> {
        match kind {
            LinkType::Require => &mut self.requires,
            LinkType::Conflict => &mut self.conflicts,
            LinkType::Provide => &mut self.provides,
            LinkType::Replace => &mut self.replaces,
            LinkType::DevRequire => &mut self.dev_requires,
        }
    }

    pub fn is_dev(&self) -> bool {
        self.stability == Stability::Dev
    }

    pub fn is_alias(&self) -> bool {
        self.alias_of.is_some()
    }

    /// The package an alias points to, or the package itself.
    pub fn base(&self) -> &Package {
        match &self.alias_of {
            Some(p) => p.base(),
            None => self,
        }
    }

    /// `BasePackage::isPlatform()` (by name).
    pub fn is_platform(&self) -> bool {
        crate::is_platform_package(&self.name)
    }

    /// `getUniqueName()`: `name-version`.
    pub fn unique_name(&self) -> String {
        format!("{}-{}", self.name, self.version)
    }

    /// `getPrettyString()`: `PrettyName prettyVersion`.
    pub fn pretty_string(&self) -> String {
        format!("{} {}", self.pretty_name, self.pretty_version)
    }

    /// `getNames($provides)`: own name plus provided (optionally) and replaced names.
    pub fn names(&self, with_provides: bool) -> Vec<String> {
        let mut names = vec![self.name.clone()];
        let provided = self.provides.iter().filter(|_| with_provides);
        for link in provided.chain(&self.replaces) {
            if !names.contains(&link.target) {
                names.push(link.target.clone());
            }
        }
        names
    }

    pub fn source_reference(&self) -> Option<&str> {
        self.source.as_ref().and_then(|s| s.reference.as_deref())
    }

    pub fn dist_reference(&self) -> Option<&str> {
        self.dist.as_ref().and_then(|d| d.reference.as_deref())
    }

    /// `getFullPrettyVersion()` with the default `DISPLAY_SOURCE_REF_IF_DEV`, truncated.
    pub fn full_pretty_version(&self) -> String {
        self.full_pretty_version_with(true, DisplayMode::SourceRefIfDev)
    }

    /// `getFullPrettyVersion($truncate, $displayMode)`.
    pub fn full_pretty_version_with(&self, truncate: bool, mode: DisplayMode) -> String {
        let source_type = self.source.as_ref().map(|s| s.kind.as_str()).unwrap_or("");
        let dist_ref = self.dist_reference().unwrap_or("");
        if mode == DisplayMode::SourceRefIfDev
            && (!self.is_dev() || (!matches!(source_type, "hg" | "git") && (!source_type.is_empty() || dist_ref.is_empty())))
        {
            return self.pretty_version.clone();
        }
        let reference = match mode {
            DisplayMode::SourceRefIfDev => self.source_reference().filter(|r| !r.is_empty()).or(self.dist_reference()),
            DisplayMode::SourceRef => self.source_reference(),
            DisplayMode::DistRef => self.dist_reference(),
        };
        match reference {
            None => self.pretty_version.clone(),
            // if source reference is a sha1 hash -- truncate
            Some(r) if truncate && r.len() == 40 && source_type != "svn" => format!("{} {}", self.pretty_version, &r[..7]),
            Some(r) => format!("{} {}", self.pretty_version, r),
        }
    }

    /// `CompletePackage::isAbandoned()`.
    pub fn is_abandoned(&self) -> bool {
        match self.meta.get("abandoned") {
            Some(Value::Bool(b)) => *b,
            Some(Value::String(s)) => !s.is_empty(),
            _ => false,
        }
    }

    /// `CompletePackage::getReplacementPackage()`.
    pub fn replacement_package(&self) -> Option<&str> {
        self.meta.get("abandoned").and_then(Value::as_str).filter(|s| !s.is_empty())
    }

    pub fn funding(&self) -> Option<&Vec<Value>> {
        self.meta.get("funding").and_then(Value::as_array).filter(|f| !f.is_empty())
    }

    /// `new AliasPackage($aliasOf, $version, $prettyVersion)`, including the
    /// `self.version` link rewriting of `replaceSelfVersionDependencies()`.
    pub fn alias(alias_of: &PackageRef, version: &str, pretty_version: &str) -> Package {
        let mut alias = (**alias_of).clone();
        alias.version = version.to_string();
        alias.pretty_version = pretty_version.to_string();
        alias.stability = komposer_semver::parse_stability(version);
        alias.alias_of = Some(alias_of.clone());
        alias.root_package_alias = false;
        alias.has_self_version_requires = false;

        let self_pretty = if pretty_version == DEFAULT_BRANCH_ALIAS { &alias_of.pretty_version } else { pretty_version };
        let self_link = |link: &Link| Link {
            source: link.source.clone(),
            target: link.target.clone(),
            constraint: Constraint::single(Op::Eq, version),
            kind: link.kind,
            pretty_constraint: self_pretty.to_string(),
        };
        let mut has_self_version_requires = false;
        for kind in LinkType::ALL {
            let links = alias.links_mut(kind);
            if matches!(kind, LinkType::Conflict | LinkType::Provide | LinkType::Replace) {
                // keep the original link, but also cover the aliased version
                let extra: Vec<Link> = links.iter().filter(|l| l.pretty_constraint == "self.version").map(self_link).collect();
                links.extend(extra);
            } else {
                for link in links.iter_mut().filter(|l| l.pretty_constraint == "self.version") {
                    *link = self_link(link);
                    has_self_version_requires |= kind == LinkType::Require;
                }
            }
        }
        alias.has_self_version_requires = has_self_version_requires;
        alias
    }
}

impl fmt::Display for Package {
    /// `__toString()`: unique name, plus alias info for aliases.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.unique_name())?;
        if let Some(of) = &self.alias_of {
            write!(f, " ({}alias of {})", if self.root_package_alias { "root " } else { "" }, of.version)?;
        }
        Ok(())
    }
}

/// An inline alias from the root requirements (`"dev-main as 1.0.x-dev"`), as stored
/// in the `aliases` section of composer.lock.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RootAlias {
    pub package: String,
    pub version: String,
    pub alias: String,
    pub alias_normalized: String,
}

/// `RootPackage`: the project's own composer.json.
#[derive(Clone, Debug)]
pub struct RootPackage {
    pub package: Package,
    pub minimum_stability: Stability,
    pub prefer_stable: bool,
    /// package name => stability, from `@dev` flags and unstable explicit versions
    pub stability_flags: Vec<(String, Stability)>,
    /// package name => commit, from `dev-main#abc123` requirements
    pub references: Vec<(String, String)>,
    pub aliases: Vec<RootAlias>,
    pub config: Map<String, Value>,
    pub repositories: Vec<Value>,
    /// `extra.branch-alias` of the root package, if any (normalized, pretty)
    pub branch_alias: Option<(String, String)>,
    /// The raw composer.json.
    pub raw: Map<String, Value>,
}

/// Name of the root package when composer.json has none.
pub const ROOT_NAME: &str = "__root__";
/// `RootPackage::DEFAULT_PRETTY_VERSION`.
pub const ROOT_DEFAULT_PRETTY_VERSION: &str = "1.0.0+no-version-set";
