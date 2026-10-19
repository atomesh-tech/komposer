//! Small PHP-compat helpers shared by the repositories.

use komposer_package::repository::ArrayRepository;
use regex::Regex;
use serde_json::Value;
use sha1::Digest;
use std::fmt::Write as _;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, LazyLock, Mutex};

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// `hash('sha1', $data)`.
pub(crate) fn sha1_hex(data: &[u8]) -> String {
    hex(&sha1::Sha1::digest(data))
}

/// `hash('sha256', $data)`.
pub(crate) fn sha256_hex(data: &[u8]) -> String {
    hex(&sha2::Sha256::digest(data))
}

/// PHP `serialize()` of a decoded JSON value (`json_decode($json, true)` shapes).
pub(crate) fn php_serialize(v: &Value) -> String {
    match v {
        Value::Null => "N;".into(),
        Value::Bool(b) => format!("b:{};", u8::from(*b)),
        Value::Number(n) if n.is_f64() => format!("d:{};", n.as_f64().unwrap_or(0.0)),
        Value::Number(n) => format!("i:{n};"),
        Value::String(s) => format!("s:{}:\"{s}\";", s.len()),
        Value::Array(a) => {
            let body: String = a.iter().enumerate().map(|(i, v)| format!("i:{i};{}", php_serialize(v))).collect();
            format!("a:{}:{{{body}}}", a.len())
        }
        Value::Object(m) => {
            let key = |k: &String| match k.parse::<i64>() {
                // PHP turns canonical integer strings into integer keys
                Ok(i) if i.to_string() == *k => format!("i:{i};"),
                _ => format!("s:{}:\"{k}\";", k.len()),
            };
            let body: String = m.iter().map(|(k, v)| key(k) + &php_serialize(v)).collect();
            format!("a:{}:{{{body}}}", m.len())
        }
    }
}

/// `urlencode()`.
pub(crate) fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' => out.push(b as char),
            b' ' => out.push('+'),
            _ => {
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}

/// `rawurldecode()`.
pub(crate) fn rawurldecode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match (b[i], s.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok())) {
            (b'%', Some(v)) => {
                out.push(v);
                i += 3;
            }
            (c, _) => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `get_debug_type()` of a decoded JSON value.
pub(crate) fn php_type(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "string",
        _ => "array",
    }
}

/// Repositories that load everything on first use (`ArrayRepository::initialize()`).
pub(crate) fn lazy_repository(
    cell: &Mutex<Option<Arc<ArrayRepository>>>,
    init: impl FnOnce() -> anyhow::Result<ArrayRepository>,
) -> anyhow::Result<Arc<ArrayRepository>> {
    let mut loaded = cell.lock().unwrap();
    if let Some(repo) = &*loaded {
        return Ok(repo.clone());
    }
    let repo = Arc::new(init()?);
    *loaded = Some(repo.clone());
    Ok(repo)
}

/// `http_build_query([$key => $values])`.
pub(crate) fn http_build_query(key: &str, values: &[String]) -> String {
    let key = urlencode(&format!("{key}["));
    let values = values.iter().enumerate().map(|(i, v)| format!("{key}{i}%5D={}", urlencode(v)));
    values.collect::<Vec<_>>().join("&")
}

/// `ProcessExecutor::splitLines()`.
pub(crate) fn split_lines(output: &str) -> Vec<&str> {
    let output = output.trim();
    if output.is_empty() { vec![] } else { output.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l)).collect() }
}

/// `Filesystem::isLocalPath()`.
pub(crate) fn is_local_path(path: &str) -> bool {
    static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^(file://|/|/?[a-z]:[\\/]|\.\.[\\/]|[a-z0-9_.-]+[\\/])").unwrap());
    RE.is_match(path)
}

/// `Filesystem::getPlatformPath()`.
pub(crate) fn platform_path(path: &str) -> String {
    static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^file://").unwrap());
    RE.replace(path, "").into_owned()
}

/// `BasePackage::packageNameToRegexp()`: `*` globs, case-insensitive, anchored.
pub(crate) fn package_name_regex(pattern: &str) -> Regex {
    Regex::new(&format!("(?i)^{}$", regex::escape(pattern).replace(r"\*", ".*"))).unwrap()
}

/// `BasePackage::packageNamesToRegexp()`.
pub(crate) fn package_names_regex(patterns: &[String]) -> Regex {
    let parts: Vec<String> = patterns.iter().map(|p| regex::escape(p).replace(r"\*", ".*")).collect();
    Regex::new(&format!("(?i)^(?:{})$", parts.join("|"))).unwrap()
}

/// Output of [`execute`].
#[derive(Debug, Default)]
pub struct ProcessOutput {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl ProcessOutput {
    pub fn ok(&self) -> bool {
        self.code == 0
    }
}

/// `ProcessExecutor::execute()`: runs a command (no shell), capturing its output.
pub(crate) fn execute(mut cmd: Command, cwd: Option<&Path>) -> ProcessOutput {
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }
    match cmd.stdin(std::process::Stdio::null()).output() {
        Ok(o) => ProcessOutput {
            code: o.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
        },
        Err(e) => ProcessOutput { code: -1, stdout: String::new(), stderr: e.to_string() },
    }
}

/// `time()`.
pub(crate) fn now_ts() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// A process-unique scratch path next to `base` (Composer uses `bin2hex(random_bytes(5))`).
pub(crate) fn unique_suffix() -> String {
    use std::hash::{BuildHasher, Hasher};
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
    h.write_u32(std::process::id());
    format!("{:010x}", h.finish() & 0xff_ffff_ffff)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn serialize_like_php() {
        assert_eq!(php_serialize(&json!({"symlink": true, "relative": false})), "a:2:{s:7:\"symlink\";b:1;s:8:\"relative\";b:0;}");
        assert_eq!(
            php_serialize(&json!({"versions": {"a/b": "1.0"}, "0": [1, 2.5]})),
            "a:2:{s:8:\"versions\";a:1:{s:3:\"a/b\";s:3:\"1.0\";}i:0;a:2:{i:0;i:1;i:1;d:2.5;}}"
        );
    }

    #[test]
    fn build_query_like_php() {
        assert_eq!(http_build_query("packages", &["foo/bar".into(), "a b".into()]), "packages%5B0%5D=foo%2Fbar&packages%5B1%5D=a+b");
    }
}
