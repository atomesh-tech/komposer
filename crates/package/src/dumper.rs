//! `ArrayDumper`: [`Package`] back to the JSON shape used in composer.lock and installed.json.

use crate::loader::php_empty;
use crate::package::{LinkType, Location, Package};
use serde_json::{Map, Value};
use std::cmp::Ordering;

/// `ArrayDumper::dump()`.
pub fn dump(p: &Package) -> Map<String, Value> {
    let mut d = Map::new();
    d.insert("name".into(), p.pretty_name.clone().into());
    d.insert("version".into(), p.pretty_version.clone().into());
    d.insert("version_normalized".into(), p.version.clone().into());
    if let Some(t) = &p.target_dir {
        d.insert("target-dir".into(), t.clone().into());
    }
    if let Some(s) = &p.source {
        d.insert("source".into(), location(s, false));
    }
    if let Some(s) = &p.dist {
        d.insert("dist".into(), location(s, true));
    }
    for kind in LinkType::ALL {
        let links = p.links(kind);
        if links.is_empty() {
            continue;
        }
        let mut m: Map<String, Value> = links.iter().map(|l| (l.target.clone(), l.pretty_constraint.clone().into())).collect();
        m.sort_keys();
        d.insert(kind.key().into(), Value::Object(m));
    }
    if !p.suggests.is_empty() {
        let mut s = p.suggests.clone();
        s.sort_keys();
        d.insert("suggest".into(), Value::Object(s));
    }
    if let Some(t) = &p.time {
        d.insert("time".into(), t.clone().into());
    }
    if p.default_branch {
        d.insert("default-branch".into(), true.into());
    }

    let bin = Value::Array(p.bin.iter().cloned().map(Value::from).collect());
    let optional = |v: Option<&String>| v.map(|s| Value::String(s.clone())).unwrap_or(Value::Null);
    let values: [(&str, Value); 9] = [
        ("bin", bin),
        ("type", p.package_type.clone().into()),
        ("extra", p.extra.clone()),
        ("installation-source", optional(p.installation_source.as_ref())),
        ("autoload", p.autoload.clone()),
        ("autoload-dev", p.autoload_dev.clone()),
        ("notification-url", optional(p.notification_url.as_ref())),
        ("include-path", p.include_path.clone()),
        ("php-ext", p.php_ext.clone()),
    ];
    for (k, v) in values {
        insert_value(&mut d, k, v);
    }

    if let Some(a) = p.meta.get("archive") {
        d.insert("archive".into(), a.clone());
    }
    for k in ["scripts", "license", "authors", "description", "homepage", "keywords", "repositories", "support", "funding"] {
        if let Some(v) = p.meta.get(k) {
            insert_value(&mut d, k, v.clone());
        }
    }
    if let Some(Value::Array(k)) = d.get_mut("keywords") {
        k.sort_by(|a, b| php_sort_regular(a.as_str().unwrap_or(""), b.as_str().unwrap_or("")));
    }
    if p.is_abandoned() {
        d.insert("abandoned".into(), p.replacement_package().map(Value::from).unwrap_or(Value::Bool(true)));
    }
    insert_value(&mut d, "transport-options", p.transport_options.clone());
    d
}

/// `Locker::lockPackages()` for one (non-alias) package: the dump without
/// `version_normalized`/`installation-source`, with `time` moved last.
pub fn dump_for_lock(p: &Package, source_commit_time: Option<String>) -> Map<String, Value> {
    let mut spec = dump(p);
    spec.shift_remove("version_normalized");
    spec.shift_remove("installation-source");
    let time = spec.shift_remove("time");
    if let Some(t) = source_commit_time.map(Value::String).or(time) {
        spec.insert("time".into(), t);
    }
    spec
}

/// `dumpValues()`: skip nulls and empty arrays.
fn insert_value(d: &mut Map<String, Value>, key: &str, v: Value) {
    let empty_array = match &v {
        Value::Array(a) => a.is_empty(),
        Value::Object(o) => o.is_empty(),
        _ => false,
    };
    if !v.is_null() && !empty_array {
        d.insert(key.into(), v);
    }
}

fn location(l: &Location, dist: bool) -> Value {
    let mut m = Map::new();
    m.insert("type".into(), l.kind.clone().into());
    m.insert("url".into(), l.url.clone().into());
    if let Some(r) = &l.reference {
        m.insert("reference".into(), r.clone().into());
    }
    if dist && let Some(s) = &l.shasum {
        m.insert("shasum".into(), s.clone().into());
    }
    if let Some(mirrors) = l.mirrors.as_ref().filter(|m| !php_empty(m)) {
        m.insert("mirrors".into(), mirrors.clone());
    }
    Value::Object(m)
}

/// PHP 8 `sort()` with `SORT_REGULAR` on strings: numeric strings compare numerically.
pub fn php_sort_regular(a: &str, b: &str) -> Ordering {
    match (php_numeric(a), php_numeric(b)) {
        (Some(x), Some(y)) => x.partial_cmp(&y).unwrap_or(Ordering::Equal),
        _ => a.cmp(b),
    }
}

fn php_numeric(s: &str) -> Option<f64> {
    let t = s.trim_start_matches([' ', '\t', '\n', '\r', '\x0b', '\x0c']).trim_end_matches([' ', '\t', '\n', '\r', '\x0b', '\x0c']);
    if t.is_empty() || t.starts_with('+') && t.len() == 1 {
        return None;
    }
    let ok = t.bytes().all(|c| c.is_ascii_digit() || matches!(c, b'.' | b'-' | b'+' | b'e' | b'E'));
    if ok { t.parse().ok() } else { None }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loader::{LoadOptions, load};
    use serde_json::json;

    #[test]
    fn round_trips_in_composer_key_order() {
        let cfg = json!({
            "name": "Monolog/monolog", "version": "v3.0.0", "type": "Library",
            "require": {"psr/log": "^2.0 || ^3.0", "php": ">=8.1"},
            "time": "2022-05-10 09:36:00", "description": "Logging", "keywords": ["psr-3", "log"],
            "dist": {"type": "zip", "url": "https://x/y.zip", "reference": "abc", "shasum": ""},
            "license": "MIT", "autoload": {"psr-4": {"Monolog\\": "src/Monolog"}}, "extra": {},
        });
        let p = &load(cfg.as_object().unwrap(), LoadOptions::default()).unwrap()[0];
        let out = Value::Object(dump_for_lock(p, None));
        assert_eq!(
            crate::json::encode(&out, crate::json::Flags::PLAIN),
            r#"{"name":"Monolog\/monolog","version":"v3.0.0","dist":{"type":"zip","url":"https:\/\/x\/y.zip","reference":"abc","shasum":""},"require":{"php":">=8.1","psr\/log":"^2.0 || ^3.0"},"type":"library","autoload":{"psr-4":{"Monolog\\":"src\/Monolog"}},"license":["MIT"],"description":"Logging","keywords":["log","psr-3"],"time":"2022-05-10T09:36:00+00:00"}"#
        );
    }
}

#[cfg(test)]
mod lock_round_trip {
    use crate::json::{self, Flags};
    use crate::loader::{LoadOptions, load};
    use serde_json::Value;

    /// Every package of a lock file written by Composer 2.10.3 must dump back identically.
    #[test]
    fn laravel_lock_packages_round_trip() {
        let lock = json::parse(include_str!("../../../tests/fixtures/laravel-13/composer.lock")).unwrap();
        let opts = LoadOptions { transport_options: true, ..Default::default() };
        for key in ["packages", "packages-dev"] {
            for original in lock[key].as_array().unwrap() {
                let loaded = load(original.as_object().unwrap(), opts).unwrap();
                let pkg = loaded.iter().find(|p| !p.is_alias()).unwrap();
                let dumped = Value::Object(super::dump_for_lock(pkg, None));
                assert_eq!(json::encode(&dumped, Flags::FILE), json::encode(original, Flags::FILE), "{}", pkg.name);
            }
        }
    }
}
