//! PHP builtins and `Composer\Util\Filesystem` path helpers whose exact output ends up in
//! the generated files.

use regex::Regex;
use std::cmp::Ordering;
use std::sync::LazyLock;

/// `var_export()` of a string.
pub fn export_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        match c {
            '\\' | '\'' => {
                out.push('\\');
                out.push(c);
            }
            '\0' => out.push_str("' . \"\\0\" . '"),
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}

/// `preg_quote()` without delimiter.
pub fn preg_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\0' => out.push_str("\\000"),
            '.' | '\\' | '+' | '*' | '?' | '[' | '^' | ']' | '$' | '(' | ')' | '{' | '}' | '=' | '!' | '<' | '>' | '|' | ':' | '-' | '#' => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out
}

/// Turns a PCRE pattern made of `preg_quote()`d text and plain regex syntax into a `regex` one.
pub fn pcre_to_regex(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len());
    let mut chars = pattern.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
        } else if let Some(escaped) = chars.next() {
            out.push_str(&regex::escape(escaped.encode_utf8(&mut [0; 4])));
        }
    }
    out
}

/// PHP `empty()` of a string.
pub fn empty(s: &str) -> bool {
    s.is_empty() || s == "0"
}

/// `substr($s, $start)` on bytes, `""` past the end.
pub fn substr(s: &str, start: usize) -> &str {
    s.get(start..).unwrap_or("")
}

/// `dirname()`.
pub fn dirname(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return if path.is_empty() { "" } else { "/" };
    }
    match trimmed.rfind('/') {
        None => ".",
        Some(i) => match trimmed[..i].trim_end_matches('/') {
            "" => "/",
            parent => parent,
        },
    }
}

/// `basename()`.
pub fn basename(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    trimmed.rsplit('/').next().unwrap_or("")
}

/// `Filesystem::isAbsolutePath()`.
pub fn is_absolute_path(path: &str) -> bool {
    path.starts_with('/') || path.as_bytes().get(1) == Some(&b':') || path.starts_with("\\\\")
}

/// `Filesystem::normalizePath()`.
pub fn normalize_path(path: &str) -> String {
    static PREFIX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^(?:[0-9a-z]{2,}:(?://(?:[a-z]:)?)?|[a-z]:)").unwrap());
    static DRIVE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)(?:^|://)[a-z]:$").unwrap());
    let path = path.replace('\\', "/");
    let mut rest = path.as_str();
    let mut absolute = "";
    // windows UNC paths e.g. \\foo\bar
    if rest.starts_with("//") && rest.len() > 2 {
        absolute = "//";
        rest = &rest[2..];
    }
    let prefix_len = PREFIX.find(rest).map_or(0, |m| m.end());
    let prefix = DRIVE.replace(&rest[..prefix_len], |c: &regex::Captures| c[0].to_uppercase()).into_owned();
    rest = &rest[prefix_len..];
    if let Some(r) = rest.strip_prefix('/') {
        absolute = "/";
        rest = r;
    }
    let mut parts: Vec<&str> = vec![];
    let mut up = false;
    for chunk in rest.split('/') {
        if chunk == ".." && (!absolute.is_empty() || up) {
            parts.pop();
            up = !(parts.is_empty() || parts.last() == Some(&".."));
        } else if chunk != "." && !chunk.is_empty() {
            parts.push(chunk);
            up = chunk != "..";
        }
    }
    format!("{prefix}{absolute}{}", parts.join("/"))
}

fn is_drive_root(path: &str) -> bool {
    let b = path.as_bytes();
    (b.len() == 2 || (b.len() == 3 && b[2] == b'/')) && b[0].is_ascii_alphabetic() && b[1] == b':'
}

/// Walks `to` up until it is a parent of `from`, as both `findShortestPath*` do.
fn common_path<'a>(from: &str, to: &'a str, stop_at_dot: bool) -> &'a str {
    let mut common = to;
    while !format!("{from}/").starts_with(&format!("{common}/")) && common != "/" && !is_drive_root(common) && !(stop_at_dot && common == ".") {
        common = dirname(common);
    }
    common
}

/// `Filesystem::findShortestPath($from, $to, $directories)`; both paths must be absolute.
pub fn find_shortest_path(from: &str, to: &str, directories: bool) -> String {
    let (mut from, to) = (normalize_path(from), normalize_path(to));
    if directories {
        from = format!("{}/dummy_file", from.trim_end_matches('/'));
    }
    if dirname(&from) == dirname(&to) {
        return format!("./{}", basename(&to));
    }
    let common = common_path(&from, &to, false);
    // no commonality at all
    if !from.starts_with(common) {
        return to.clone();
    }
    let common = format!("{}/", common.trim_end_matches('/'));
    let depth = substr(&from, common.len()).matches('/').count();
    // allow top level /foo & /bar dirs to be addressed relatively as this is common in Docker setups
    if common == "/" && depth > 1 {
        return to.clone();
    }
    let result = format!("{}{}", "../".repeat(depth), substr(&to, common.len()));
    if result.is_empty() { "./".into() } else { result }
}

/// `Filesystem::findShortestPathCode($from, $to, $directories, $staticCode)`; both paths must be absolute.
pub fn find_shortest_path_code(from: &str, to: &str, directories: bool, static_code: bool) -> String {
    let (from, to) = (normalize_path(from), normalize_path(to));
    if from == to {
        return if directories { "__DIR__" } else { "__FILE__" }.into();
    }
    let common = common_path(&from, &to, true);
    // no commonality at all
    if !from.starts_with(common) || common == "." {
        return export_str(&to);
    }
    let common = format!("{}/", common.trim_end_matches('/'));
    if to.starts_with(&format!("{from}/")) {
        return format!("__DIR__ . {}", export_str(substr(&to, from.len())));
    }
    let depth = substr(&from, common.len()).matches('/').count() + directories as usize;
    // allow top level /foo & /bar dirs to be addressed relatively as this is common in Docker setups
    if common == "/" && depth > 1 {
        return export_str(&to);
    }
    let code = if static_code {
        format!("__DIR__ . '{}'", "/..".repeat(depth))
    } else {
        format!("{}__DIR__{}", "dirname(".repeat(depth), ")".repeat(depth))
    };
    match substr(&to, common.len()) {
        "" => code,
        rel => format!("{code}.{}", export_str(&format!("/{rel}"))),
    }
}

/// `strnatcasecmp()`.
pub fn strnatcasecmp(a: &str, b: &str) -> Ordering {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.is_empty() || b.is_empty() {
        return a.len().cmp(&b.len());
    }
    let at = |s: &[u8], i: usize| s.get(i).copied().unwrap_or(0);
    let digit = |s: &[u8], i: usize| at(s, i).is_ascii_digit();
    let end = |ai: usize, bi: usize| match (ai >= a.len(), bi >= b.len()) {
        (true, true) => Some(Ordering::Equal),
        (true, false) => Some(Ordering::Less),
        (false, true) => Some(Ordering::Greater),
        _ => None,
    };
    let (mut ai, mut bi) = (0, 0);
    let mut leading = true;
    loop {
        if leading {
            while at(a, ai) == b'0' && digit(a, ai + 1) {
                ai += 1;
            }
            while at(b, bi) == b'0' && digit(b, bi + 1) {
                bi += 1;
            }
            leading = false;
        }
        while at(a, ai).is_ascii_whitespace() || at(a, ai) == 0x0b {
            ai += 1;
        }
        while at(b, bi).is_ascii_whitespace() || at(b, bi) == 0x0b {
            bi += 1;
        }
        if digit(a, ai) && digit(b, bi) {
            // fractional parts compare left-aligned, integers by magnitude first
            let fractional = at(a, ai) == b'0' || at(b, bi) == b'0';
            let mut bias = Ordering::Equal;
            let result = loop {
                match (digit(a, ai), digit(b, bi)) {
                    (false, false) => break bias,
                    (false, true) => break Ordering::Less,
                    (true, false) => break Ordering::Greater,
                    _ => {}
                }
                let ord = at(a, ai).cmp(&at(b, bi));
                if fractional && ord != Ordering::Equal {
                    break ord;
                }
                if bias == Ordering::Equal {
                    bias = ord;
                }
                ai += 1;
                bi += 1;
            };
            if result != Ordering::Equal {
                return result;
            }
            if let Some(ord) = end(ai, bi) {
                return ord;
            }
        }
        let ord = at(a, ai).to_ascii_uppercase().cmp(&at(b, bi).to_ascii_uppercase());
        if ord != Ordering::Equal {
            return ord;
        }
        ai += 1;
        bi += 1;
        if let Some(ord) = end(ai, bi) {
            return ord;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortest_paths() {
        assert_eq!(find_shortest_path_code("/a/vendor/composer", "/a/vendor", true, false), "dirname(__DIR__)");
        assert_eq!(find_shortest_path_code("/a/vendor", "/a", true, false), "dirname(__DIR__)");
        assert_eq!(find_shortest_path_code("/a/vendor", "/a/b", true, false), "dirname(__DIR__).'/b'");
        assert_eq!(find_shortest_path_code("/a/vendor/composer", "/a", true, true), "__DIR__ . '/../..'");
        assert_eq!(find_shortest_path_code("/vendor", "/app", true, false), "dirname(__DIR__).'/app'");
        assert_eq!(find_shortest_path_code("/vendor/composer", "/app", true, false), "'/app'");
        assert_eq!(find_shortest_path_code("/a", "/a/b/c", true, false), "__DIR__ . '/b/c'");
        assert_eq!(find_shortest_path("/a/b", "/a/b", true), "./");
        assert_eq!(find_shortest_path("/a/b", "/a/c/d", true), "../c/d");
        assert_eq!(find_shortest_path("/a/b", "/a/b/c", true), "./c");
        assert_eq!(normalize_path("/a/./b/../c//d/"), "/a/c/d");
        assert_eq!(normalize_path("../a/../../b"), "../../b");
        assert_eq!(normalize_path("c:\\foo\\..\\bar"), "C:/bar");
        assert_eq!(dirname("/a"), "/");
        assert_eq!(dirname("a"), ".");
        assert_eq!(dirname("/a/b//"), "/a");
    }

    #[test]
    fn natural_order() {
        assert_eq!(strnatcasecmp("a/b2", "a/b10"), Ordering::Less);
        assert_eq!(strnatcasecmp("A/B", "a/b"), Ordering::Equal);
        assert_eq!(strnatcasecmp("x01", "x1"), Ordering::Less);
        assert_eq!(strnatcasecmp("b/bar", "c/lorem"), Ordering::Less);
    }

    #[test]
    fn exports() {
        assert_eq!(export_str("A\\B'c"), "'A\\\\B\\'c'");
        assert_eq!(pcre_to_regex(&preg_quote("/a-b/c.d<e>")), r"/a\-b/c\.d<e>");
    }
}
