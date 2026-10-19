//! JSON the way PHP Composer reads and writes it.
//!
//! Composer decodes JSON into associative arrays (`json_decode($json, true)`), so
//! `{}` becomes `[]` and `{"0": .., "1": ..}` becomes a list when re-encoded. Files
//! are written with `JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES | JSON_UNESCAPED_UNICODE`,
//! hashes are computed over `json_encode($data, 0)`. Both matter for byte-identical
//! lock files and `content-hash` values.

use anyhow::{Context, Result};
use serde_json::{Map, Value};
use std::fmt::Write as _;
use std::path::Path;

/// Flags of PHP's `json_encode` that Composer uses.
#[derive(Clone, Copy, Debug, Default)]
pub struct Flags {
    pub pretty: bool,
    pub unescaped_slashes: bool,
    pub unescaped_unicode: bool,
}

impl Flags {
    /// `JsonFile::encode()` defaults, used for every file Composer writes.
    pub const FILE: Flags = Flags { pretty: true, unescaped_slashes: true, unescaped_unicode: true };
    /// `json_encode($data, 0)`, used for `content-hash`.
    pub const PLAIN: Flags = Flags { pretty: false, unescaped_slashes: false, unescaped_unicode: false };
}

/// Parses JSON with PHP associative-array semantics (see module docs).
pub fn parse(s: &str) -> Result<Value> {
    let v: Value = serde_json::from_str(s.strip_prefix('\u{feff}').unwrap_or(s))?;
    Ok(php_normalize(v))
}

/// Parses a JSON object (composer.json, config.json...). `{}` decodes to PHP's `[]`, so an
/// empty array is an empty object too.
pub fn parse_object(s: &str) -> Result<Map<String, Value>> {
    match parse(s)? {
        Value::Object(m) => Ok(m),
        Value::Array(a) if a.is_empty() => Ok(Map::new()),
        _ => anyhow::bail!("JSON is not an object"),
    }
}

/// Reads and parses a JSON file with PHP semantics.
pub fn read(path: &Path) -> Result<Value> {
    let s = std::fs::read_to_string(path).with_context(|| format!("Could not read {}", path.display()))?;
    parse(&s).with_context(|| format!("\"{}\" does not contain valid JSON", path.display()))
}

/// Writes `value` like `JsonFile::write()`: pretty-printed, trailing newline.
/// Only touches the file if its content changes.
pub fn write(path: &Path, value: &Value) -> Result<()> {
    let out = encode(value, Flags::FILE) + "\n";
    if std::fs::read_to_string(path).is_ok_and(|old| old == out) {
        return Ok(());
    }
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, out).with_context(|| format!("Could not write {}", path.display()))
}

/// Applies `json_decode(.., true)` semantics to an already parsed value.
pub fn php_normalize(v: Value) -> Value {
    match v {
        Value::Array(a) => Value::Array(a.into_iter().map(php_normalize).collect()),
        Value::Object(m) => {
            if m.keys().enumerate().all(|(i, k)| *k == i.to_string()) {
                // empty or 0..n-1 keyed objects are PHP lists
                Value::Array(m.into_iter().map(|(_, v)| php_normalize(v)).collect())
            } else {
                Value::Object(m.into_iter().map(|(k, v)| (k, php_normalize(v))).collect())
            }
        }
        v => v,
    }
}

/// PHP's `json_encode($value, $flags)`.
pub fn encode(value: &Value, flags: Flags) -> String {
    let mut out = String::new();
    write_value(&mut out, value, flags, 0);
    out
}

fn write_value(out: &mut String, v: &Value, flags: Flags, depth: usize) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => write_number(out, n),
        Value::String(s) => write_string(out, s, flags),
        Value::Array(a) if a.is_empty() => out.push_str("[]"),
        Value::Object(m) if m.is_empty() => out.push_str("{}"),
        Value::Array(a) => {
            out.push('[');
            for (i, item) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                newline(out, flags, depth + 1);
                write_value(out, item, flags, depth + 1);
            }
            newline(out, flags, depth);
            out.push(']');
        }
        Value::Object(m) => {
            out.push('{');
            for (i, (k, item)) in m.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                newline(out, flags, depth + 1);
                write_string(out, k, flags);
                out.push_str(if flags.pretty { ": " } else { ":" });
                write_value(out, item, flags, depth + 1);
            }
            newline(out, flags, depth);
            out.push('}');
        }
    }
}

fn newline(out: &mut String, flags: Flags, depth: usize) {
    if flags.pretty {
        out.push('\n');
        for _ in 0..depth {
            out.push_str("    ");
        }
    }
}

fn write_number(out: &mut String, n: &serde_json::Number) {
    match n.as_f64() {
        // PHP (serialize_precision = -1) prints integral floats as `1.0`
        Some(f) if n.is_f64() && f.fract() == 0.0 && f.abs() < 1e15 => {
            let _ = write!(out, "{f:.1}");
        }
        _ => {
            let _ = write!(out, "{n}");
        }
    }
}

fn write_string(out: &mut String, s: &str, flags: Flags) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '/' if !flags.unescaped_slashes => out.push_str("\\/"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            // escaped even with JSON_UNESCAPED_UNICODE unless JSON_UNESCAPED_LINE_TERMINATORS
            '\u{2028}' | '\u{2029}' => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c if !c.is_ascii() && !flags.unescaped_unicode => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    let _ = write!(out, "\\u{unit:04x}");
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Sorts an object's keys like PHP's `ksort()` on string keys.
pub fn ksort(m: &mut Map<String, Value>) {
    m.sort_keys();
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn decode_semantics() {
        assert_eq!(
            parse(r#"{"a": {}, "b": {"0": 1, "1": 2}, "c": {"1": 1, "0": 2}}"#).unwrap(),
            json!({"a": [], "b": [1, 2], "c": {"1": 1, "0": 2}})
        );
    }

    #[test]
    fn empty_object_is_an_object() {
        assert!(parse_object("{}").unwrap().is_empty());
        assert!(parse_object("[1]").is_err());
    }

    #[test]
    fn encode_plain_escapes_like_php() {
        let v = json!({"url": "https://x/y", "name": "é😀", "n": 1.0, "i": 3, "ctl": "\u{1}\n"});
        assert_eq!(encode(&v, Flags::PLAIN), r#"{"url":"https:\/\/x\/y","name":"\u00e9\ud83d\ude00","n":1.0,"i":3,"ctl":"\u0001\n"}"#);
    }

    #[test]
    fn encode_pretty_like_php() {
        let v = json!({"a": [], "b": {}, "c": [1, {"d": "é/"}]});
        assert_eq!(
            encode(&v, Flags::FILE),
            "{\n    \"a\": [],\n    \"b\": {},\n    \"c\": [\n        1,\n        {\n            \"d\": \"é/\"\n        }\n    ]\n}"
        );
    }
}
