//! `PlatformRepository`: php, extensions, libraries and composer itself as installable
//! packages, plus `config.platform` overrides.
//!
//! komposer has no PHP of its own. When a `php` binary is available (`KOMPOSER_PHP`, else `php`
//! on PATH) it is run once with a script printing what `Platform\Runtime` would report; the
//! library detection of `PlatformRepository::initialize()` then runs on that data here.
//! Without PHP only the `composer*` packages and the overrides exist, see [`php_detected`].

use anyhow::{Result, anyhow, bail};
use komposer_package::config::Config;
use komposer_package::repository::{ArrayRepository, LoadRequest, LoadResult, RepoKind, Repository};
use komposer_package::{Link, LinkType, Package, PackageRef, is_platform_package};
use komposer_semver::{Constraint, Op};
use regex::Regex;
use serde_json::{Map, Value, json};
use std::collections::{HashMap, HashSet};
use std::process::Command;
use std::sync::{Arc, LazyLock, OnceLock};

/// What `Platform\Runtime` reports about the PHP the project runs on.
#[derive(Clone, Debug, Default)]
pub struct PhpRuntime {
    /// defined constants Composer reads; class constants as `Class::NAME`
    pub constants: Map<String, Value>,
    /// `get_loaded_extensions()` with `phpversion($ext)` (`0` when unknown)
    pub extensions: Vec<(String, String)>,
    /// `ReflectionExtension::info()` output, for the extensions whose libraries are detected
    pub extension_info: HashMap<String, String>,
    /// `inet_pton('::') !== false`
    pub inet_pton_ipv6: bool,
    /// `curl_version()['version']`
    pub curl_version: Option<String>,
    /// `ResourceBundle::create('root', 'ICUDATA', false)->get('Version')`
    pub icu_cldr_version: Option<String>,
    /// `IntlChar::getUnicodeVersion()`, first three parts joined
    pub icu_unicode_version: Option<String>,
    /// `(new Imagick())->getVersion()['versionString']`
    pub imagick_version: Option<String>,
    /// `HHVM_VERSION`
    pub hhvm_version: Option<String>,
    /// `IniHelper::getAll()`: the loaded php.ini (`""` for none), then the scanned ones
    pub ini_files: Vec<String>,
}

/// Prints the JSON [`PhpRuntime::from_json`] reads.
const DETECT_SCRIPT: &str = r#"
$c = array();
$names = array('PHP_VERSION', 'PHP_INT_SIZE', 'PHP_DEBUG', 'PHP_ZTS', 'AF_INET6', 'GD_VERSION', 'GMP_VERSION', 'ICONV_VERSION', 'INTL_ICU_VERSION', 'LIBXML_DOTTED_VERSION', 'OPENSSL_VERSION_TEXT', 'PCRE_VERSION', 'PGSQL_LIBPQ_VERSION', 'RD_KAFKA_VERSION', 'SODIUM_LIBRARY_VERSION', 'LIBXSLT_DOTTED_VERSION', 'ZLIB_VERSION', 'ZipArchive::LIBZIP_VERSION', 'HHVM_VERSION');
if (PHP_VERSION_ID < 90000) { $names[] = 'MB_ONIGURUMA_VERSION'; }
foreach ($names as $k) { if (defined($k)) { $c[$k] = @constant($k); } }
$withInfo = array('amqp', 'bz2', 'curl', 'date', 'fileinfo', 'gd', 'intl', 'ldap', 'mbstring', 'memcached', 'pcre', 'mysqlnd', 'pdo_mysql', 'mongodb', 'pgsql', 'pdo_pgsql', 'pq', 'sqlite3', 'pdo_sqlite', 'ssh2', 'xsl', 'yaml', 'zlib');
$exts = array(); $info = array();
foreach (get_loaded_extensions() as $n) {
    $v = phpversion($n);
    $exts[] = array($n, $v === false ? '0' : $v);
    if (in_array($n, $withInfo, true)) { $r = new ReflectionExtension($n); ob_start(); $r->info(); $info[$n] = (string) ob_get_clean(); }
}
$loaded = function ($e) { return extension_loaded($e); };
$out = array(
    'constants' => (object) $c,
    'extensions' => $exts,
    'info' => (object) $info,
    'ipv6' => @inet_pton('::') !== false,
    'curl' => $loaded('curl') && function_exists('curl_version') ? curl_version() : null,
    'cldr' => null, 'unicode' => null, 'imagick' => null,
    'ini' => array_merge(array((string) php_ini_loaded_file()), false !== ($s = php_ini_scanned_files()) ? array_map('trim', explode(',', $s)) : array()),
);
if (is_array($out['curl'])) { $out['curl'] = $out['curl']['version']; }
if ($loaded('intl') && class_exists('ResourceBundle', false)) { $b = ResourceBundle::create('root', 'ICUDATA', false); if ($b !== null) { $out['cldr'] = $b->get('Version'); } }
if ($loaded('intl') && class_exists('IntlChar', false)) { $out['unicode'] = implode('.', array_slice(IntlChar::getUnicodeVersion(), 0, 3)); }
if ($loaded('imagick') && class_exists('Imagick', false)) { $i = new Imagick(); $v = $i->getVersion(); $out['imagick'] = $v['versionString']; }
echo json_encode($out, defined('JSON_INVALID_UTF8_SUBSTITUTE') ? JSON_INVALID_UTF8_SUBSTITUTE : 0);
"#;

impl PhpRuntime {
    /// Runs `php` (or `$KOMPOSER_PHP`) once; `None` when there is no usable PHP.
    pub fn detect() -> Option<PhpRuntime> {
        let php = std::env::var("KOMPOSER_PHP").ok().filter(|p| !p.is_empty()).unwrap_or_else(|| "php".into());
        let mut cmd = Command::new(php);
        cmd.args(["-d", "display_errors=stderr", "-r", DETECT_SCRIPT]);
        let out = crate::util::execute(cmd, None);
        if !out.ok() {
            return None;
        }
        serde_json::from_str::<Value>(out.stdout.trim()).ok().map(|v| PhpRuntime::from_json(&v))
    }

    pub fn from_json(v: &Value) -> PhpRuntime {
        let s = |k: &str| v.get(k).and_then(Value::as_str).map(String::from);
        PhpRuntime {
            constants: v.get("constants").and_then(Value::as_object).cloned().unwrap_or_default(),
            extensions: v
                .get("extensions")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|e| Some((e.get(0)?.as_str()?.to_string(), e.get(1)?.as_str()?.to_string())))
                .collect(),
            extension_info: v
                .get("info")
                .and_then(Value::as_object)
                .into_iter()
                .flatten()
                .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
                .collect(),
            inet_pton_ipv6: v.get("ipv6") == Some(&Value::Bool(true)),
            curl_version: s("curl"),
            icu_cldr_version: s("cldr"),
            icu_unicode_version: s("unicode"),
            imagick_version: s("imagick"),
            hhvm_version: v.get("constants").and_then(|c| c.get("HHVM_VERSION")).and_then(Value::as_str).map(String::from),
            ini_files: v.get("ini").and_then(Value::as_array).into_iter().flatten().filter_map(|f| f.as_str().map(String::from)).collect(),
        }
    }

    fn has(&self, name: &str) -> bool {
        self.constants.contains_key(name)
    }

    /// A constant as PHP would print it.
    fn constant(&self, name: &str) -> Option<String> {
        match self.constants.get(name)? {
            Value::String(s) => Some(s.clone()),
            Value::Null => None,
            v => Some(v.to_string()),
        }
    }

    fn truthy(&self, name: &str) -> bool {
        self.constants.get(name).is_some_and(komposer_package::loader::php_truthy)
    }

    fn info(&self, ext: &str) -> &str {
        self.extension_info.get(ext).map_or("", String::as_str)
    }
}

/// The PHP runtime detected for this process, if any (detection runs once).
pub fn detected_runtime() -> Option<&'static PhpRuntime> {
    static RUNTIME: OnceLock<Option<PhpRuntime>> = OnceLock::new();
    RUNTIME.get_or_init(PhpRuntime::detect).as_ref()
}

/// Whether a PHP binary was found; without one only `config.platform` describes php and its
/// extensions, so callers may want to ignore platform requirements.
pub fn php_detected() -> bool {
    detected_runtime().is_some()
}

struct Override {
    name: String,
    /// `None` disables the package (`false`)
    version: Option<String>,
}

pub struct PlatformRepository {
    repo: ArrayRepository,
    disabled: Vec<PackageRef>,
}

impl PlatformRepository {
    /// The platform of this machine with the `config.platform` overrides of `config`.
    pub fn from_config(config: &Config) -> Result<PlatformRepository> {
        PlatformRepository::new(config.get("platform").as_ref().unwrap_or(&json!([])), detected_runtime())
    }

    /// `new PlatformRepository([], $overrides, $runtime)`.
    pub fn new(overrides: &Value, runtime: Option<&PhpRuntime>) -> Result<PlatformRepository> {
        let mut parsed = vec![];
        for (name, version) in overrides.as_object().into_iter().flatten() {
            let version = match version {
                Value::String(v) => Some(v.clone()),
                Value::Bool(false) if name == "php" => {
                    bail!("config.platform.{name} cannot be set to false as you cannot disable php entirely.")
                }
                Value::Bool(false) => None,
                v => bail!("config.platform.{name} should be a string or false, but got {} {v}", crate::util::php_type(v)),
            };
            parsed.push(Override { name: name.clone(), version });
        }
        let mut builder = Builder { overrides: parsed, packages: vec![], disabled: vec![], libraries: HashSet::new() };
        builder.initialize(runtime)?;
        let mut repo = ArrayRepository::new("platform repo", RepoKind::Platform);
        for p in builder.packages {
            repo.add_package(p);
        }
        Ok(PlatformRepository { repo, disabled: builder.disabled.into_iter().map(Arc::new).collect() })
    }

    /// `getDisabledPackages()`: packages set to `false` in `config.platform`, with their actual version.
    pub fn disabled_packages(&self) -> &[PackageRef] {
        &self.disabled
    }

    /// `isPlatformPackageDisabled()`.
    pub fn is_platform_package_disabled(&self, name: &str) -> bool {
        self.disabled.iter().any(|p| p.name == name)
    }
}

impl Repository for PlatformRepository {
    fn repo_name(&self) -> String {
        "platform repo".into()
    }

    fn kind(&self) -> RepoKind {
        RepoKind::Platform
    }

    fn load_packages(&self, req: &LoadRequest) -> Result<LoadResult> {
        self.repo.load_packages(req)
    }

    fn packages(&self) -> Result<Vec<PackageRef>> {
        self.repo.packages()
    }

    fn is_platform_package_disabled(&self, name: &str) -> bool {
        PlatformRepository::is_platform_package_disabled(self, name)
    }
}

fn normalize(v: &str) -> Result<String> {
    komposer_semver::normalize(v).map_err(|e| anyhow!("{e}"))
}

fn describe(p: &mut Package, description: String) {
    p.meta.insert("description".into(), description.into());
}

fn description(p: &Package) -> String {
    p.meta.get("description").and_then(Value::as_str).unwrap_or("").to_string()
}

struct Builder {
    overrides: Vec<Override>,
    packages: Vec<Package>,
    disabled: Vec<Package>,
    libraries: HashSet<String>,
}

impl Builder {
    fn override_for(&self, name: &str) -> Option<&Override> {
        self.overrides.iter().find(|o| o.name.to_lowercase() == name)
    }

    /// `PlatformRepository::initialize()`.
    fn initialize(&mut self, runtime: Option<&PhpRuntime>) -> Result<()> {
        for i in 0..self.overrides.len() {
            let name = self.overrides[i].name.clone();
            if !is_platform_package(&name) {
                bail!("Invalid platform package name in config.platform: {name}");
            }
            if let Some(version) = self.overrides[i].version.clone() {
                self.add_overridden(&name, &version, None)?;
            }
        }
        for (name, pretty, desc) in [
            ("composer", crate::COMPOSER_VERSION, "Composer package"),
            ("composer-plugin-api", crate::PLUGIN_API_VERSION, "The Composer Plugin API"),
            ("composer-runtime-api", crate::RUNTIME_API_VERSION, "The Composer Runtime API"),
        ] {
            self.add_simple(name, &normalize(pretty)?, pretty, desc)?;
        }
        let Some(rt) = runtime else { return Ok(()) };

        static PHP_BASE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^([^~+-]+).*$").unwrap());
        let raw = rt.constant("PHP_VERSION").unwrap_or_default();
        let (version, pretty) = match normalize(&raw) {
            Ok(v) => (v, raw),
            Err(_) => {
                let pretty = PHP_BASE.replace(&raw, "$1").into_owned();
                (normalize(&pretty)?, pretty)
            }
        };
        self.add_simple("php", &version, &pretty, "The PHP interpreter")?;
        if rt.truthy("PHP_DEBUG") {
            self.add_simple("php-debug", &version, &pretty, "The PHP interpreter, with debugging symbols")?;
        }
        if rt.truthy("PHP_ZTS") {
            self.add_simple("php-zts", &version, &pretty, "The PHP interpreter, with Zend Thread Safety")?;
        }
        if rt.constants.get("PHP_INT_SIZE").and_then(Value::as_i64) == Some(8) {
            self.add_simple("php-64bit", &version, &pretty, "The PHP interpreter, 64bit")?;
        }
        // AF_INET6 is only defined with ext-sockets, IPv6 support might still be available
        if rt.has("AF_INET6") || rt.inet_pton_ipv6 {
            self.add_simple("php-ipv6", &version, &pretty, "The PHP interpreter, with IPv6 support")?;
        }

        let loaded: Vec<&str> = rt.extensions.iter().map(|(n, _)| n.as_str()).collect();
        for (name, version) in &rt.extensions {
            if !matches!(name.as_str(), "standard" | "Core") {
                self.add_extension(name, version)?;
            }
        }
        for name in &loaded {
            self.add_libraries(rt, name, &loaded)?;
        }

        if let Some(hhvm) = &rt.hhvm_version {
            let (version, pretty) = match normalize(hhvm) {
                Ok(v) => (v, hhvm.clone()),
                Err(_) => {
                    let pretty = PHP_BASE.replace(hhvm, "$1").into_owned();
                    (normalize(&pretty)?, pretty)
                }
            };
            self.add_simple("hhvm", &version, &pretty, "The HHVM Runtime (64bit)")?;
        }
        Ok(())
    }

    fn add_simple(&mut self, name: &str, version: &str, pretty: &str, desc: &str) -> Result<()> {
        let mut p = Package::new(name, version, pretty);
        describe(&mut p, desc.into());
        self.add_package(p)
    }

    /// `addPackage()`: overridden packages only update the overrider's description.
    fn add_package(&mut self, mut p: Package) -> Result<()> {
        if let Some(o) = self.override_for(&p.name) {
            if o.version.is_none() {
                let desc = format!("{}. <warning>Package disabled via config.platform</warning>", description(&p));
                describe(&mut p, desc);
                p.extra = json!({"config.platform": true});
                self.disabled.push(p);
                return Ok(());
            }
            if let Some(overrider) = self.packages.iter_mut().find(|x| x.name == p.name) {
                let actual = if overrider.version == p.version { "same as actual".to_string() } else { format!("actual: {}", p.pretty_version) };
                describe(overrider, format!("{}, {actual}", description(overrider)));
            }
            return Ok(());
        }
        // php-* packages follow an overridden php
        if p.name.starts_with("php-")
            && let Some(php) = self.override_for("php").and_then(|o| o.version.clone())
        {
            let pretty_name = p.pretty_name.clone();
            let overrider = self.add_overridden("php", &php, Some(&pretty_name))?;
            let actual =
                if self.packages[overrider].version == p.version { "same as actual".to_string() } else { format!("actual: {}", p.pretty_version) };
            let o = &mut self.packages[overrider];
            describe(o, format!("{}, {actual}", description(o)));
            return Ok(());
        }
        self.packages.push(p);
        Ok(())
    }

    /// `addOverriddenPackage()`; returns the index of the new package.
    fn add_overridden(&mut self, name: &str, version: &str, as_name: Option<&str>) -> Result<usize> {
        let mut p = Package::new(as_name.unwrap_or(name), &normalize(version)?, version);
        describe(&mut p, "Package overridden via config.platform".into());
        p.extra = json!({"config.platform": true});
        self.packages.push(p);
        Ok(self.packages.len() - 1)
    }

    /// `addExtension()`.
    fn add_extension(&mut self, name: &str, pretty: &str) -> Result<()> {
        static NUMERIC: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(\d+\.\d+\.\d+(?:\.\d+)?)").unwrap());
        let (version, pretty, extra) = match normalize(pretty) {
            Ok(v) => (v, pretty.to_string(), String::new()),
            Err(_) => {
                let base = NUMERIC.captures(pretty).map_or("0".to_string(), |c| c[1].to_string());
                (normalize(&base)?, base, format!(" (actual version: {pretty})"))
            }
        };
        let package_name = format!("ext-{}", name.to_lowercase().replace(' ', "-"));
        let mut ext = Package::new(&package_name, &version, &pretty);
        describe(&mut ext, format!("The {name} PHP extension{extra}"));
        ext.package_type = "php-ext".into();
        if name == "uuid" {
            ext.replaces = vec![link("ext-uuid", "lib-uuid", &version, &pretty, LinkType::Replace)];
        }
        self.add_package(ext)
    }

    /// `addLibrary()`.
    fn add_library(&mut self, name: &str, pretty: Option<String>, desc: Option<String>, replaces: &[String], provides: &[String]) -> Result<()> {
        let Some(pretty) = pretty else { return Ok(()) };
        let Ok(version) = normalize(&pretty) else { return Ok(()) };
        let lib_name = format!("lib-{name}");
        // a parsing glitch must not produce a bogus package name
        if !is_platform_package(&lib_name) || !self.libraries.insert(lib_name.clone()) {
            return Ok(());
        }
        let mut lib = Package::new(&lib_name, &version, &pretty);
        describe(&mut lib, desc.unwrap_or_else(|| format!("The {name} library")));
        let links = |targets: &[String], kind| {
            targets.iter().map(|t| link(&lib_name, &format!("lib-{}", t.to_lowercase()), &version, &pretty, kind)).collect::<Vec<_>>()
        };
        lib.replaces = links(replaces, LinkType::Replace);
        lib.provides = links(provides, LinkType::Provide);
        self.add_package(lib)
    }

    /// The per-extension library detection of `initialize()`.
    fn add_libraries(&mut self, rt: &PhpRuntime, name: &str, loaded: &[&str]) -> Result<()> {
        let info = rt.info(name);
        let cap = |pattern: &str| -> Option<String> { Regex::new(pattern).ok()?.captures(info).map(|c| c["version"].to_string()) };
        let lib = |s: &mut Builder, n: String, v: Option<String>| s.add_library(&n, v, None, &[], &[]);
        match name {
            "amqp" => {
                self.add_library(
                    &format!("{name}-librabbitmq"),
                    cap(r"(?im)^librabbitmq version => (?<version>.+)$"),
                    Some("AMQP librabbitmq version".into()),
                    &[],
                    &[],
                )?;
                let protocol = cap(r"(?im)^AMQP protocol version => (?<version>.+)$").map(|v| v.replace('-', "."));
                self.add_library(&format!("{name}-protocol"), protocol, Some("AMQP protocol version".into()), &[], &[])?;
            }
            "bz2" => lib(self, name.into(), cap(r"(?im)^BZip2 Version => (?<version>.*),"))?,
            "curl" => {
                lib(self, name.into(), rt.curl_version.clone())?;
                static SSL: LazyLock<Regex> =
                    LazyLock::new(|| Regex::new(r"(?im)^SSL Version => (?<library>[^\r\n/]+)/(?<version>[^\r\n]+?)\r?$").unwrap());
                static ST: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\(securetransport\) ([a-z0-9]+)").unwrap());
                if let Some(c) = SSL.captures(info) {
                    let library = c["library"].to_lowercase();
                    let version = c["version"].to_string();
                    if library == "openssl" {
                        let (parsed, fips) = parse_openssl(&version).map_or((None, false), |(v, f)| (Some(v), f));
                        let desc = format!("curl OpenSSL version ({})", parsed.clone().unwrap_or_default());
                        let provides = if fips { vec!["curl-openssl".to_string()] } else { vec![] };
                        self.add_library(&format!("{name}-openssl{}", if fips { "-fips" } else { "" }), parsed, Some(desc), &[], &provides)?;
                    } else {
                        let (short, ssl_lib) = match ST.captures(&library) {
                            Some(m) if library.starts_with("(securetransport)") => ("securetransport".to_string(), format!("curl-{}", &m[1])),
                            _ => (library.clone(), "curl-openssl".to_string()),
                        };
                        let desc = format!("curl {library} version ({version})");
                        self.add_library(&format!("{name}-{short}"), Some(version), Some(desc), &[ssl_lib], &[])?;
                    }
                }
                static SSH: LazyLock<Regex> =
                    LazyLock::new(|| Regex::new(r"(?im)^libSSH Version => (?<library>[^\r\n/]+)/(?<version>.+?)(?:/.*)?$").unwrap());
                if let Some(c) = SSH.captures(info) {
                    let desc = format!("curl {} version", &c["library"]);
                    self.add_library(&format!("{name}-{}", c["library"].to_lowercase()), Some(c["version"].to_string()), Some(desc), &[], &[])?;
                }
                self.add_library(&format!("{name}-zlib"), cap(r"(?im)^ZLib Version => (?<version>.+)$"), Some("curl zlib version".into()), &[], &[])?;
            }
            "date" => {
                self.add_library(
                    &format!("{name}-timelib"),
                    cap(r"(?im)^timelib version => (?<version>.+)$"),
                    Some("date timelib version".into()),
                    &[],
                    &[],
                )?;
                if let Some(source) = Regex::new(r"(?im)^Timezone Database => (?<source>internal|external)$").unwrap().captures(info) {
                    let external = &source["source"] == "external";
                    if let Some(zoneinfo) = cap(r#"(?im)^"Olson" Timezone Database Version => (?<version>.+?)(?:\.system)?$"#) {
                        // a timezonedb extension replaces the bundled database
                        if external && loaded.contains(&"timezonedb") {
                            let desc = "zoneinfo (\"Olson\") database for date (replaced by timezonedb)".to_string();
                            self.add_library("timezonedb-zoneinfo", Some(zoneinfo), Some(desc), &[format!("{name}-zoneinfo")], &[])?;
                        } else {
                            self.add_library(
                                &format!("{name}-zoneinfo"),
                                Some(zoneinfo),
                                Some("zoneinfo (\"Olson\") database for date".into()),
                                &[],
                                &[],
                            )?;
                        }
                    }
                }
            }
            "fileinfo" => self.add_library(
                &format!("{name}-libmagic"),
                cap(r"(?im)^libmagic => (?<version>.+)$"),
                Some("fileinfo libmagic version".into()),
                &[],
                &[],
            )?,
            "gd" => {
                lib(self, name.into(), rt.constant("GD_VERSION"))?;
                let jpeg = cap(r"(?im)^libJPEG Version => (?<version>.+?)(?: compatible)?$").and_then(|v| parse_libjpeg(&v));
                self.add_library(&format!("{name}-libjpeg"), jpeg, Some("libjpeg version for gd".into()), &[], &[])?;
                self.add_library(
                    &format!("{name}-libpng"),
                    cap(r"(?im)^libPNG Version => (?<version>.+)$"),
                    Some("libpng version for gd".into()),
                    &[],
                    &[],
                )?;
                self.add_library(
                    &format!("{name}-freetype"),
                    cap(r"(?im)^FreeType Version => (?<version>.+)$"),
                    Some("freetype version for gd".into()),
                    &[],
                    &[],
                )?;
                let xpm = cap(r"(?im)^libXpm Version => (?<version>\d+)$").and_then(|v| v.parse().ok()).map(|id| convert_version_id(id, 100));
                self.add_library(&format!("{name}-libxpm"), xpm, Some("libxpm version for gd".into()), &[], &[])?;
            }
            "gmp" => lib(self, name.into(), rt.constant("GMP_VERSION"))?,
            "iconv" => lib(self, name.into(), rt.constant("ICONV_VERSION"))?,
            "intl" => {
                let desc = Some("The ICU unicode and globalization support library".to_string());
                let icu = if rt.has("INTL_ICU_VERSION") { rt.constant("INTL_ICU_VERSION") } else { cap(r"(?im)^ICU version => (?<version>.+)$") };
                self.add_library("icu", icu, desc, &[], &[])?;
                let tz = cap(r"(?im)^ICU TZData version => (?<version>.*)$").and_then(|v| parse_zoneinfo_version(&v));
                self.add_library("icu-zoneinfo", tz, Some("zoneinfo (\"Olson\") database for icu".into()), &[], &[])?;
                self.add_library("icu-cldr", rt.icu_cldr_version.clone(), Some("ICU CLDR project version".into()), &[], &[])?;
                self.add_library("icu-unicode", rt.icu_unicode_version.clone(), Some("ICU unicode version".into()), &[], &[])?;
            }
            "imagick" => {
                static IM: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^ImageMagick (?<version>[\d.]+)(?:-(?<patch>\d+))?").unwrap());
                if let Some(c) = rt.imagick_version.as_deref().and_then(|v| IM.captures(v)) {
                    let version = match c.name("patch") {
                        Some(p) => format!("{}.{}", &c["version"], p.as_str()),
                        None => c["version"].to_string(),
                    };
                    self.add_library(&format!("{name}-imagemagick"), Some(version), None, &["imagick".into()], &[])?;
                }
            }
            "ldap" => {
                let id = cap(r"(?im)^Vendor Version => (?<version>\d+)$").and_then(|v| v.parse().ok());
                let vendor = Regex::new(r"(?im)^Vendor Name => (?<vendor>.+)$").unwrap().captures(info).map(|c| c["vendor"].to_string());
                if let (Some(id), Some(vendor)) = (id, vendor) {
                    let desc = format!("{vendor} version of ldap");
                    self.add_library(&format!("{name}-{}", vendor.to_lowercase()), Some(convert_version_id(id, 100)), Some(desc), &[], &[])?;
                }
            }
            "libxml" => {
                // ext/dom, ext/simplexml, ext/xmlreader and ext/xmlwriter use the same libxml
                let provides: Vec<String> = loaded
                    .iter()
                    .filter(|e| ["dom", "simplexml", "xml", "xmlreader", "xmlwriter"].contains(e))
                    .map(|e| format!("{e}-libxml"))
                    .collect();
                self.add_library(name, rt.constant("LIBXML_DOTTED_VERSION"), Some("libxml library version".into()), &[], &provides)?;
            }
            "mbstring" => {
                self.add_library(
                    &format!("{name}-libmbfl"),
                    cap(r"(?im)^libmbfl version => (?<version>.+)$"),
                    Some("mbstring libmbfl version".into()),
                    &[],
                    &[],
                )?;
                let onig = if rt.has("MB_ONIGURUMA_VERSION") {
                    rt.constant("MB_ONIGURUMA_VERSION")
                } else {
                    cap(r"(?im)^(?:oniguruma|Multibyte regex \(oniguruma\)) version => (?<version>.+)$")
                };
                self.add_library(&format!("{name}-oniguruma"), onig, Some("mbstring oniguruma version".into()), &[], &[])?;
            }
            "memcached" => self.add_library(
                &format!("{name}-libmemcached"),
                cap(r"(?im)^libmemcached version => (?<version>.+)$"),
                Some("libmemcached version".into()),
                &[],
                &[],
            )?,
            "openssl" => {
                static TEXT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^(?:OpenSSL|LibreSSL)?\s*(?<version>\S+)").unwrap());
                let text = rt.constant("OPENSSL_VERSION_TEXT").unwrap_or_default();
                if let Some(c) = TEXT.captures(&text) {
                    let (parsed, fips) = parse_openssl(&c["version"]).map_or((None, false), |(v, f)| (Some(v), f));
                    let provides = if fips { vec![name.to_string()] } else { vec![] };
                    self.add_library(&format!("{name}{}", if fips { "-fips" } else { "" }), parsed, Some(text.clone()), &[], &provides)?;
                }
            }
            "pcre" => {
                let version = rt.constant("PCRE_VERSION").map(|v| v.split_whitespace().next().unwrap_or("").to_string());
                lib(self, name.into(), version)?;
                self.add_library(
                    &format!("{name}-unicode"),
                    cap(r"(?im)^PCRE Unicode Version => (?<version>.+)$"),
                    Some("PCRE Unicode version support".into()),
                    &[],
                    &[],
                )?;
            }
            "mysqlnd" | "pdo_mysql" => {
                let desc = format!("mysqlnd library version for {name}");
                self.add_library(
                    &format!("{name}-mysqlnd"),
                    cap(r"(?mi)^(?:Client API version|Version) => mysqlnd (?<version>.+?) "),
                    Some(desc),
                    &[],
                    &[],
                )?;
            }
            "mongodb" => {
                self.add_library(
                    &format!("{name}-libmongoc"),
                    cap(r"(?im)^libmongoc bundled version => (?<version>.+)$"),
                    Some("libmongoc version of mongodb".into()),
                    &[],
                    &[],
                )?;
                self.add_library(
                    &format!("{name}-libbson"),
                    cap(r"(?im)^libbson bundled version => (?<version>.+)$"),
                    Some("libbson version of mongodb".into()),
                    &[],
                    &[],
                )?;
            }
            "pgsql" if rt.has("PGSQL_LIBPQ_VERSION") => {
                self.add_library("pgsql-libpq", rt.constant("PGSQL_LIBPQ_VERSION"), Some("libpq for pgsql".into()), &[], &[])?;
            }
            "pgsql" | "pdo_pgsql" => {
                let desc = format!("libpq for {name}");
                self.add_library(&format!("{name}-libpq"), cap(r"(?im)^PostgreSQL\(libpq\) Version => (?<version>.*)$"), Some(desc), &[], &[])?;
            }
            "pq" => {
                // Used Library => Compiled => Linked
                let linked = Regex::new(r"(?im)^libpq => (?<compiled>.+) => (?<linked>.+)$").unwrap().captures(info).map(|c| c["linked"].to_string());
                self.add_library(&format!("{name}-libpq"), linked, Some(format!("libpq for {name}")), &[], &[])?;
            }
            "rdkafka" => {
                // MM.mm.rr.xx as hex, the pre-release id is ignored
                if let Some(v) = rt.constants.get("RD_KAFKA_VERSION").and_then(Value::as_i64) {
                    let version = format!("{}.{}.{}", (v & 0x7F000000) >> 24, (v & 0x00FF0000) >> 16, (v & 0x0000FF00) >> 8);
                    self.add_library(&format!("{name}-librdkafka"), Some(version), Some(format!("librdkafka for {name}")), &[], &[])?;
                }
            }
            "libsodium" | "sodium" if rt.has("SODIUM_LIBRARY_VERSION") => lib(self, "libsodium".into(), rt.constant("SODIUM_LIBRARY_VERSION"))?,
            "sqlite3" | "pdo_sqlite" => lib(self, format!("{name}-sqlite"), cap(r"(?im)^SQLite Library => (?<version>.+)$"))?,
            "ssh2" => lib(self, format!("{name}-libssh2"), cap(r"(?im)^libssh2 version => (?<version>.+)$"))?,
            "xsl" => {
                self.add_library("libxslt", rt.constant("LIBXSLT_DOTTED_VERSION"), None, &["xsl".into()], &[])?;
                let libxml = cap(r"(?im)^libxslt compiled against libxml Version => (?<version>.+)$");
                self.add_library("libxslt-libxml", libxml, Some("libxml version libxslt is compiled against".into()), &[], &[])?;
            }
            "yaml" => self.add_library(
                &format!("{name}-libyaml"),
                cap(r"(?im)^LibYAML Version => (?<version>.+)$"),
                Some("libyaml version of yaml".into()),
                &[],
                &[],
            )?,
            "zip" if rt.has("ZipArchive::LIBZIP_VERSION") => {
                self.add_library(&format!("{name}-libzip"), rt.constant("ZipArchive::LIBZIP_VERSION"), None, &["zip".into()], &[])?;
            }
            "zlib" => {
                let version = if rt.has("ZLIB_VERSION") { rt.constant("ZLIB_VERSION") } else { cap(r"(?im)^Linked Version => (?<version>.+)$") };
                lib(self, name.into(), version)?;
            }
            _ => {}
        }
        Ok(())
    }
}

fn link(source: &str, target: &str, version: &str, pretty: &str, kind: LinkType) -> Link {
    Link { source: source.into(), target: target.into(), constraint: Constraint::single(Op::Eq, version), kind, pretty_constraint: pretty.into() }
}

/// `Version::parseOpenssl()`: `(version, is_fips)`.
fn parse_openssl(version: &str) -> Option<(String, bool)> {
    static RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^(?<version>[0-9.]+)(?<patch>[a-z]{0,2})(?<suffix>(?:-?(?:dev|pre|alpha|beta|rc|fips)[\d]*)*)(?:-\w+)?(?: \(.+?\))?$").unwrap()
    });
    let c = RE.captures(version)?;
    // OpenSSL 1 used 1.2.3a style versioning, 3+ uses semver
    let patch = if komposer_semver::version_compare(&c["version"], "3.0.0") == std::cmp::Ordering::Less {
        format!(".{}", alpha_to_int(&c["patch"]))
    } else {
        String::new()
    };
    let suffix = &c["suffix"];
    let fips = suffix.contains("fips");
    let suffix = format!("-{}", suffix.trim_start_matches('-')).replace("-fips", "").replace("-pre", "-alpha");
    Some((format!("{}{patch}{suffix}", &c["version"]).trim_end_matches('-').to_string(), fips))
}

/// `Version::parseLibjpeg()`.
fn parse_libjpeg(version: &str) -> Option<String> {
    let c = Regex::new(r"^(?<major>\d+)(?<minor>[a-z]*)$").unwrap().captures(version)?;
    Some(format!("{}.{}", &c["major"], alpha_to_int(&c["minor"])))
}

/// `Version::parseZoneinfoVersion()`.
fn parse_zoneinfo_version(version: &str) -> Option<String> {
    let c = Regex::new(r"^(?<year>\d{4})(?<revision>[a-z]*)$").unwrap().captures(version)?;
    Some(format!("{}.{}", &c["year"], alpha_to_int(&c["revision"])))
}

/// `convertAlphaVersionToIntVersion()`: a => 1, z => 26, zg => 33.
fn alpha_to_int(alpha: &str) -> u32 {
    alpha.bytes().map(|b| u32::from(b) - u32::from(b'a') + 1).sum()
}

/// `convertVersionId()` (libxpm, openldap).
fn convert_version_id(id: i64, base: i64) -> String {
    format!("{}.{}.{}", id / (base * base), (id / base) % base, id % base)
}

#[cfg(test)]
mod tests {
    use super::*;
    use komposer_package::repository::Repository;

    fn runtime(constants: Value, extensions: &[&str], info: Option<&str>) -> PhpRuntime {
        let mut constants = constants.as_object().cloned().unwrap_or_default();
        constants.entry("PHP_VERSION").or_insert("7.1.0".into());
        PhpRuntime {
            constants,
            extensions: extensions.iter().map(|e| (e.to_string(), "100.200.300".to_string())).collect(),
            extension_info: extensions.iter().filter_map(|e| Some((e.to_string(), info?.to_string()))).collect(),
            ..Default::default()
        }
    }

    fn find(repo: &PlatformRepository, name: &str) -> Option<PackageRef> {
        repo.packages().unwrap().into_iter().find(|p| p.name == name)
    }

    #[test]
    #[allow(clippy::type_complexity)]
    fn php_flavors() {
        let cases: Vec<(Value, bool, Vec<(&str, &str)>)> = vec![
            (json!({"PHP_VERSION": "7.1.33"}), false, vec![("php", "7.1.33")]),
            (
                json!({"PHP_VERSION": "7.2.31-1+ubuntu16.04.1+deb.sury.org+1", "PHP_DEBUG": true}),
                false,
                vec![("php", "7.2.31"), ("php-debug", "7.2.31")],
            ),
            (json!({"PHP_VERSION": "7.2.31-1+ubuntu16.04.1+deb.sury.org+1", "PHP_ZTS": true}), false, vec![("php", "7.2.31"), ("php-zts", "7.2.31")]),
            (json!({"PHP_VERSION": "7.2.31-1+ubuntu16.04.1+deb.sury.org+1", "PHP_INT_SIZE": 8}), false, vec![("php-64bit", "7.2.31")]),
            (json!({"PHP_VERSION": "7.2.31-1+ubuntu16.04.1+deb.sury.org+1", "AF_INET6": 30}), false, vec![("php-ipv6", "7.2.31")]),
            (json!({"PHP_VERSION": "7.2.31-1+ubuntu16.04.1+deb.sury.org+1"}), true, vec![("php-ipv6", "7.2.31")]),
        ];
        for (constants, ipv6, expected) in cases {
            let mut rt = runtime(constants, &[], None);
            rt.inet_pton_ipv6 = ipv6;
            let repo = PlatformRepository::new(&json!([]), Some(&rt)).unwrap();
            for (name, version) in expected {
                assert_eq!(find(&repo, name).unwrap_or_else(|| panic!("{name}")).pretty_version, version);
            }
        }
        // inet_pton regression: no AF_INET6 and inet_pton failing means no php-ipv6
        let rt = runtime(json!({"PHP_VERSION": "7.0.0", "PHP_DEBUG": false}), &[], None);
        let repo = PlatformRepository::new(&json!([]), Some(&rt)).unwrap();
        assert!(find(&repo, "php-ipv6").is_none());
        assert!(find(&repo, "php-debug").is_none());
    }

    #[test]
    fn composer_packages_and_no_php() {
        let repo = PlatformRepository::new(&json!({"php": "8.2.1", "ext-foo": "1.0"}), None).unwrap();
        assert_eq!(find(&repo, "composer").unwrap().pretty_version, "2.10.3");
        assert_eq!(find(&repo, "composer-plugin-api").unwrap().pretty_version, "2.9.0");
        assert_eq!(find(&repo, "composer-runtime-api").unwrap().version, "2.2.2.0");
        assert_eq!(find(&repo, "php").unwrap().pretty_version, "8.2.1");
        assert_eq!(find(&repo, "ext-foo").unwrap().meta["description"], "Package overridden via config.platform");
    }

    #[test]
    fn overrides_and_disabled_packages() {
        let rt = runtime(json!({"PHP_VERSION": "8.3.4", "PHP_INT_SIZE": 8}), &["mbstring", "json"], None);
        let repo = PlatformRepository::new(&json!({"php": "8.1.0", "ext-json": false, "ext-mbstring": "100.200.300"}), Some(&rt)).unwrap();
        let php = find(&repo, "php").unwrap();
        assert_eq!(php.pretty_version, "8.1.0");
        assert_eq!(php.meta["description"], "Package overridden via config.platform, actual: 8.3.4");
        assert_eq!(find(&repo, "php-64bit").unwrap().pretty_version, "8.1.0");
        assert_eq!(find(&repo, "ext-mbstring").unwrap().meta["description"], "Package overridden via config.platform, same as actual");
        assert!(find(&repo, "ext-json").is_none());
        assert!(repo.is_platform_package_disabled("ext-json"));
        assert_eq!(
            repo.disabled_packages()[0].meta["description"],
            "The json PHP extension. <warning>Package disabled via config.platform</warning>"
        );

        assert!(PlatformRepository::new(&json!({"php": false}), None).err().unwrap().to_string().contains("cannot be set to false"));
        assert_eq!(
            PlatformRepository::new(&json!({"foo/bar": "1.0"}), None).err().unwrap().to_string(),
            "Invalid platform package name in config.platform: foo/bar"
        );
    }

    #[test]
    fn extensions_with_odd_versions() {
        let mut rt = runtime(json!({}), &[], None);
        rt.extensions = vec![("Zend OPcache".into(), "8.3.4".into()), ("weird".into(), "1.2.3-weird-stuff".into()), ("uuid".into(), "1.2.0".into())];
        let repo = PlatformRepository::new(&json!([]), Some(&rt)).unwrap();
        assert_eq!(find(&repo, "ext-zend-opcache").unwrap().package_type, "php-ext");
        let weird = find(&repo, "ext-weird").unwrap();
        assert_eq!(weird.pretty_version, "1.2.3");
        assert_eq!(weird.meta["description"], "The weird PHP extension (actual version: 1.2.3-weird-stuff)");
        assert_eq!(find(&repo, "ext-uuid").unwrap().replaces[0].target, "lib-uuid");
    }

    /// `PlatformRepositoryTest::provideLibraryTestCases()` (abridged info texts).
    #[test]
    #[allow(clippy::type_complexity)]
    fn library_information() {
        type Expect<'a> = Vec<(&'a str, Option<&'a str>, &'a [&'a str], &'a [&'a str])>;
        let curl_info = |ssl: &str, ssh: &str| {
            format!(
                "curl\n\ncURL support => enabled\nHost => x86_64-pc-linux-gnu\nSSL Version => {ssl}\nZLib Version => 1.2.8\nlibSSH Version => {ssh}\n\nDirective => Local Value => Master Value\ncurl.cainfo => no value => no value"
            )
        };
        let cases: Vec<(&[&str], String, Value, Option<&str>, Expect)> = vec![
            (&["amqp"], "amqp\n\nVersion => 1.9.4\nAMQP protocol version => 0-9-1\nlibrabbitmq version => 0.9.0\nDefault max channels per connection => 256".into(), json!({}), None,
                vec![("lib-amqp-protocol", Some("0.9.1"), &[], &[]), ("lib-amqp-librabbitmq", Some("0.9.0"), &[], &[])]),
            (&["bz2"], "bz2\n\nBZip2 Support => Enabled\nBZip2 Version => 1.0.5, 6-Sept-2010".into(), json!({}), None, vec![("lib-bz2", Some("1.0.5"), &[], &[])]),
            (&["curl"], curl_info("OpenSSL/1.0.1t", "libssh2/1.4.3"), json!({}), Some("2.0.0"),
                vec![("lib-curl", Some("2.0.0"), &[], &[]), ("lib-curl-openssl", Some("1.0.1.20"), &[], &[]), ("lib-curl-zlib", Some("1.2.8"), &[], &[]), ("lib-curl-libssh2", Some("1.4.3"), &[], &[])]),
            (&["curl"], curl_info("Schannel", "libssh2/1.11.1"), json!({}), Some("8.16.0"),
                vec![("lib-curl", Some("8.16.0"), &[], &[]), ("lib-curl-openssl", None, &[], &[]), ("lib-curl-schannel", None, &[], &[]), ("lib-curl-zlib", Some("1.2.8"), &[], &[]), ("lib-curl-libssh2", Some("1.11.1"), &[], &[])]),
            (&["curl"], curl_info("OpenSSL/3.0.16", "Unknown"), json!({}), Some("8.16.0"),
                vec![("lib-curl", Some("8.16.0"), &[], &[]), ("lib-curl-openssl", Some("3.0.16"), &[], &[]), ("lib-curl-zlib", Some("1.2.8"), &[], &[]), ("lib-curl-libssh2", None, &[], &[])]),
            (&["curl"], curl_info("OpenSSL/1.0.1t-fips", "libssh2/1.4.3"), json!({}), Some("2.0.0"),
                vec![("lib-curl", Some("2.0.0"), &[], &[]), ("lib-curl-openssl-fips", Some("1.0.1.20"), &[], &["lib-curl-openssl"]), ("lib-curl-zlib", Some("1.2.8"), &[], &[]), ("lib-curl-libssh2", Some("1.4.3"), &[], &[])]),
            (&["curl"], curl_info("GnuTLS/2.12.14", "libssh/0.9.3/openssl/zlib"), json!({}), Some("7.22.0"),
                vec![("lib-curl", Some("7.22.0"), &[], &[]), ("lib-curl-gnutls", Some("2.12.14"), &["lib-curl-openssl"], &[]), ("lib-curl-zlib", Some("1.2.8"), &[], &[]), ("lib-curl-libssh", Some("0.9.3"), &[], &[])]),
            (&["curl"], curl_info("(SecureTransport) LibreSSL/2.8.3", "libssh2/1.11.0"), json!({}), Some("8.1.2"),
                vec![("lib-curl", Some("8.1.2"), &[], &[]), ("lib-curl-securetransport", Some("2.8.3"), &["lib-curl-libressl"], &[]), ("lib-curl-zlib", Some("1.2.8"), &[], &[]), ("lib-curl-libssh2", Some("1.11.0"), &[], &[])]),
            (&["curl"], curl_info("(Schannel) OpenSSL/3.0.5", "libssh2/1.11.0"), json!({}), Some("8.1.2"),
                vec![("lib-curl", Some("8.1.2"), &[], &[]), ("lib-curl-openssl", None, &[], &[]), ("lib-curl-schannel", None, &[], &[]), ("lib-curl-zlib", Some("1.2.8"), &[], &[]), ("lib-curl-libssh2", Some("1.11.0"), &[], &[])]),
            (&["date"], "date\n\ndate/time support => enabled\ntimelib version => 2018.03\n\"Olson\" Timezone Database Version => 2020.1\nTimezone Database => external\nDefault timezone => Europe/Berlin".into(), json!({}), None,
                vec![("lib-date-timelib", Some("2018.03"), &[], &[]), ("lib-date-zoneinfo", Some("2020.1"), &[], &[])]),
            (&["date", "timezonedb"], "date\n\n\"Olson\" Timezone Database Version => 2020.1\nTimezone Database => external\nDefault timezone => UTC".into(), json!({}), None,
                vec![("lib-timezonedb-zoneinfo", Some("2020.1"), &["lib-date-zoneinfo"], &[])]),
            (&["date"], "\ndate/time support => enabled\ntimelib version => 2018.03\n\"Olson\" Timezone Database Version => 0.system\nTimezone Database => internal".into(), json!({}), None,
                vec![("lib-date-zoneinfo", Some("0"), &[], &[]), ("lib-date-timelib", Some("2018.03"), &[], &[])]),
            (&["fileinfo"], "fileinfo\n\nfileinfo support => enabled\nlibmagic => 537".into(), json!({}), None, vec![("lib-fileinfo-libmagic", Some("537"), &[], &[])]),
            (&["gd"], "gd\n\nGD Support => enabled\nFreeType Version => 2.10.0\nlibJPEG Version => 9 compatible\nlibPNG Version => 1.6.34\n\nDirective => Local Value => Master Value".into(), json!({"GD_VERSION": "1.2.3"}), None,
                vec![("lib-gd", Some("1.2.3"), &[], &[]), ("lib-gd-freetype", Some("2.10.0"), &[], &[]), ("lib-gd-libjpeg", Some("9.0"), &[], &[]), ("lib-gd-libpng", Some("1.6.34"), &[], &[])]),
            (&["gd"], "gd\n\nFreeType Version => 2.6.3\nlibJPEG Version => 6b\nlibPNG Version => 1.6.28\nlibXpm Version => 30411\n".into(), json!({"GD_VERSION": "2.2.5"}), None,
                vec![("lib-gd", Some("2.2.5"), &[], &[]), ("lib-gd-freetype", Some("2.6.3"), &[], &[]), ("lib-gd-libjpeg", Some("6.2"), &[], &[]), ("lib-gd-libpng", Some("1.6.28"), &[], &[]), ("lib-gd-libxpm", Some("3.4.11"), &[], &[])]),
            (&["iconv"], String::new(), json!({"ICONV_VERSION": "1.2.4"}), None, vec![("lib-iconv", Some("1.2.4"), &[], &[])]),
            (&["gmp"], String::new(), json!({"GMP_VERSION": "6.1.0"}), None, vec![("lib-gmp", Some("6.1.0"), &[], &[])]),
            (&["intl"], "intl\n\nICU version => 57.1\nICU Data version => 57.1\nICU TZData version => 2016b\nICU Unicode version => 8.0".into(), json!({"INTL_ICU_VERSION": "100"}), None,
                vec![("lib-icu", Some("100"), &[], &[]), ("lib-icu-cldr", Some("32.0.1"), &[], &[]), ("lib-icu-unicode", Some("7.0.0"), &[], &[]), ("lib-icu-zoneinfo", Some("2016.2"), &[], &[])]),
            (&["ldap"], "ldap\n\nAPI Version => 3001\nVendor Name => OpenLDAP\nVendor Version => 20450\nSASL Support => Enabled".into(), json!({}), None, vec![("lib-ldap-openldap", Some("2.4.50"), &[], &[])]),
            (&["libxml", "dom", "simplexml", "xml", "xmlreader", "xmlwriter"], String::new(), json!({"LIBXML_DOTTED_VERSION": "2.1.5"}), None,
                vec![("lib-libxml", Some("2.1.5"), &[], &["lib-dom-libxml", "lib-simplexml-libxml", "lib-xml-libxml", "lib-xmlreader-libxml", "lib-xmlwriter-libxml"])]),
            (&["mbstring"], "mbstring\n\nlibmbfl version => 1.3.2\n\nMultibyte regex (oniguruma) version => 6.1.3".into(), json!({"MB_ONIGURUMA_VERSION": "7.0.0"}), None,
                vec![("lib-mbstring-libmbfl", Some("1.3.2"), &[], &[]), ("lib-mbstring-oniguruma", Some("7.0.0"), &[], &[])]),
            (&["mbstring"], "mbstring\n\nlibmbfl version => 1.3.2\noniguruma version => 6.9.4\n".into(), json!({}), None,
                vec![("lib-mbstring-libmbfl", Some("1.3.2"), &[], &[]), ("lib-mbstring-oniguruma", Some("6.9.4"), &[], &[])]),
            (&["memcached"], "memcached\n\nVersion => 3.1.5\nlibmemcached version => 1.0.18\nSASL support => yes".into(), json!({}), None, vec![("lib-memcached-libmemcached", Some("1.0.18"), &[], &[])]),
            (&["openssl"], String::new(), json!({"OPENSSL_VERSION_TEXT": "OpenSSL 1.1.1g  21 Apr 2020"}), None, vec![("lib-openssl", Some("1.1.1.7"), &[], &[])]),
            (&["openssl"], String::new(), json!({"OPENSSL_VERSION_TEXT": "OpenSSL 1.1.1g-freebsd  21 Apr 2020"}), None, vec![("lib-openssl", Some("1.1.1.7"), &[], &[])]),
            (&["openssl"], String::new(), json!({"OPENSSL_VERSION_TEXT": "OpenSSL 0.9.8zg  21 Apr 2020"}), None, vec![("lib-openssl", Some("0.9.8.33"), &[], &[])]),
            (&["openssl"], String::new(), json!({"OPENSSL_VERSION_TEXT": "OpenSSL 1.1.1g-pre1  21 Apr 2020"}), None, vec![("lib-openssl", Some("1.1.1.7-alpha1"), &[], &[])]),
            (&["openssl"], String::new(), json!({"OPENSSL_VERSION_TEXT": "OpenSSL 1.1.1g-rc2  21 Apr 2020"}), None, vec![("lib-openssl", Some("1.1.1.7-rc2"), &[], &[])]),
            (&["openssl"], String::new(), json!({"OPENSSL_VERSION_TEXT": "OpenSSL 1.1.1g-fips  21 Apr 2020"}), None, vec![("lib-openssl-fips", Some("1.1.1.7"), &[], &["lib-openssl"])]),
            (&["openssl"], String::new(), json!({"OPENSSL_VERSION_TEXT": "LibreSSL 2.0.1"}), None, vec![("lib-openssl", Some("2.0.1.0"), &[], &[])]),
            (&["mysqlnd"], "                mysqlnd\n\nmysqlnd => enabled\nVersion => mysqlnd 5.0.11-dev - 20150407 - $Id: 38fea24f2847fa7519001be390c98ae0acafe387 $\nCompression => supported".into(), json!({}), None,
                vec![("lib-mysqlnd-mysqlnd", Some("5.0.11-dev"), &[], &[])]),
            (&["pdo_mysql"], "pdo_mysql\n\nClient API version => mysqlnd 5.0.10-dev - 20150407 - $Id: 38fea24f2847fa7519001be390c98ae0acafe387 $".into(), json!({}), None,
                vec![("lib-pdo_mysql-mysqlnd", Some("5.0.10-dev"), &[], &[])]),
            (&["mongodb"], "mongodb\n\nlibbson bundled version => 1.15.2\nlibmongoc bundled version => 1.15.2\nlibmongoc SSL => enabled".into(), json!({}), None,
                vec![("lib-mongodb-libmongoc", Some("1.15.2"), &[], &[]), ("lib-mongodb-libbson", Some("1.15.2"), &[], &[])]),
            (&["pcre"], "pcre\n\nPCRE Library Version => 10.33 2019-04-16\nPCRE Unicode Version => 11.0.0\nPCRE JIT Support => enabled".into(), json!({"PCRE_VERSION": "10.33 2019-04-16"}), None,
                vec![("lib-pcre", Some("10.33"), &[], &[]), ("lib-pcre-unicode", Some("11.0.0"), &[], &[])]),
            (&["pgsql"], "pgsql\n\nPostgreSQL(libpq) Version => 12.2\n".into(), json!({"PGSQL_LIBPQ_VERSION": "12.2"}), None, vec![("lib-pgsql-libpq", Some("12.2"), &[], &[])]),
            (&["pdo_pgsql"], "pdo_pgsql\n\nPDO Driver for PostgreSQL => enabled\nPostgreSQL(libpq) Version => 12.1\nModule version => 7.1.33".into(), json!({}), None, vec![("lib-pdo_pgsql-libpq", Some("12.1"), &[], &[])]),
            (&["pq"], "pq\n\nUsed Library => Compiled => Linked\nlibpq => 14.3 (Ubuntu 14.3-1.pgdg22.04+1) => 15.0.2\n".into(), json!({}), None, vec![("lib-pq-libpq", Some("15.0.2"), &[], &[])]),
            (&["rdkafka"], String::new(), json!({"RD_KAFKA_VERSION": 17367807}), None, vec![("lib-rdkafka-librdkafka", Some("1.9.2"), &[], &[])]),
            (&["sodium"], String::new(), json!({"SODIUM_LIBRARY_VERSION": "1.0.15"}), None, vec![("lib-libsodium", Some("1.0.15"), &[], &[])]),
            (&["pdo_sqlite"], "pdo_sqlite\n\nPDO Driver for SQLite 3.x => enabled\nSQLite Library => 3.32.3\n".into(), json!({}), None, vec![("lib-pdo_sqlite-sqlite", Some("3.32.3"), &[], &[])]),
            (&["ssh2"], "ssh2\n\nextension version => 1.2\nlibssh2 version => 1.8.0\nbanner => SSH-2.0-libssh2_1.8.0".into(), json!({}), None, vec![("lib-ssh2-libssh2", Some("1.8.0"), &[], &[])]),
            (&["yaml"], "yaml\n\nModule Version => 2.0.2\nLibYAML Version => 0.2.2\n".into(), json!({}), None, vec![("lib-yaml-libyaml", Some("0.2.2"), &[], &[])]),
            (&["xsl"], "xsl\n\nlibxslt Version => 1.1.33\nlibxslt compiled against libxml Version => 2.9.8\nEXSLT => enabled".into(), json!({"LIBXSLT_DOTTED_VERSION": "1.1.29"}), None,
                vec![("lib-libxslt", Some("1.1.29"), &["lib-xsl"], &[]), ("lib-libxslt-libxml", Some("2.9.8"), &[], &[])]),
            (&["zip"], String::new(), json!({"ZipArchive::LIBZIP_VERSION": "1.5.0"}), None, vec![("lib-zip-libzip", Some("1.5.0"), &["lib-zip"], &[])]),
            (&["zlib"], String::new(), json!({"ZLIB_VERSION": "1.2.10"}), None, vec![("lib-zlib", Some("1.2.10"), &[], &[])]),
            (&["zlib"], "zlib\n\nCompiled Version => 1.2.8\nLinked Version => 1.2.11".into(), json!({}), None, vec![("lib-zlib", Some("1.2.11"), &[], &[])]),
        ];
        for (extensions, info, constants, curl, expected) in cases {
            let mut rt = runtime(constants, extensions, Some(&info));
            rt.curl_version = curl.map(String::from);
            if extensions == ["intl"] {
                rt.icu_cldr_version = Some("32.0.1".into());
                rt.icu_unicode_version = Some("7.0.0".into());
            }
            let repo = PlatformRepository::new(&json!([]), Some(&rt)).unwrap();
            let libs: Vec<String> = repo.packages().unwrap().iter().filter(|p| p.name.starts_with("lib-")).map(|p| p.name.clone()).collect();
            let wanted: Vec<&str> = expected.iter().filter(|e| e.1.is_some()).map(|e| e.0).collect();
            assert_eq!(libs.len(), wanted.len(), "{extensions:?}: {libs:?} vs {wanted:?}");
            for ext in extensions {
                assert_eq!(find(&repo, &format!("ext-{ext}")).unwrap().pretty_version, "100.200.300");
            }
            for (name, version, replaces, provides) in expected {
                let p = find(&repo, name);
                let Some(version) = version else {
                    assert!(p.is_none(), "{name} should be missing");
                    continue;
                };
                let p = p.unwrap_or_else(|| panic!("{name} missing for {extensions:?}"));
                assert_eq!(p.pretty_version, version, "{name}");
                let targets = |links: &[Link]| links.iter().map(|l| l.target.clone()).collect::<Vec<_>>();
                assert_eq!(targets(&p.replaces), replaces, "{name} replaces");
                assert_eq!(targets(&p.provides), provides, "{name} provides");
                assert!(
                    p.replaces.iter().chain(&p.provides).all(|l| l.source == p.name && l.constraint == Constraint::single(Op::Eq, p.version.clone()))
                );
            }
        }
    }

    #[test]
    fn imagick_and_hhvm() {
        for (version_string, expected) in [
            ("ImageMagick 6.2.9 Q16 x86_64 2018-05-18 http://www.imagemagick.org", "6.2.9"),
            ("ImageMagick 7.0.8-34 Q16 x86_64 2019-03-23 https://imagemagick.org", "7.0.8.34"),
        ] {
            let mut rt = runtime(json!({}), &["imagick"], None);
            rt.imagick_version = Some(version_string.into());
            let repo = PlatformRepository::new(&json!([]), Some(&rt)).unwrap();
            let lib = find(&repo, "lib-imagick-imagemagick").unwrap();
            assert_eq!(lib.pretty_version, expected);
            assert_eq!(lib.replaces[0].target, "lib-imagick");
        }
        let mut rt = runtime(json!({}), &[], None);
        rt.hhvm_version = Some("2.1.0".into());
        assert_eq!(find(&PlatformRepository::new(&json!([]), Some(&rt)).unwrap(), "hhvm").unwrap().pretty_version, "2.1.0");
    }

    #[test]
    fn parses_runtime_json() {
        let rt = PhpRuntime::from_json(&json!({
            "constants": {"PHP_VERSION": "8.3.4", "PHP_INT_SIZE": 8}, "extensions": [["Core", "8.3.4"], ["json", "8.3.4"]],
            "info": {"curl": "x"}, "ipv6": true, "curl": "8.5.0", "cldr": null, "unicode": null, "imagick": null
        }));
        assert_eq!(rt.extensions[1], ("json".to_string(), "8.3.4".to_string()));
        assert!(rt.inet_pton_ipv6);
        assert_eq!(rt.curl_version.as_deref(), Some("8.5.0"));
    }

    #[test]
    #[ignore = "needs a php binary"]
    fn detects_local_php() {
        let rt = PhpRuntime::detect().expect("php on PATH");
        let repo = PlatformRepository::new(&json!([]), Some(&rt)).unwrap();
        assert!(find(&repo, "php").is_some());
    }
}
