//! `PhpFileParser::findClasses()` and `PhpFileCleaner`, on top of an emulation of the
//! `php_strip_whitespace()` call they start with. PHP >= 8.1 is assumed (`enum` is a type,
//! `#[` starts an attribute) with `short_open_tag` enabled.

use anyhow::{Result, bail};
use regex::bytes::{Regex, RegexBuilder};
use std::sync::LazyLock;

fn bytes_regex(pattern: &str) -> Regex {
    RegexBuilder::new(pattern).unicode(false).case_insensitive(true).build().unwrap()
}

/// `PhpFileParser::findClasses()`.
pub fn find_classes(path: &str) -> Result<Vec<String>> {
    // the possessive quantifiers of the PHP pattern never give anything back here, so plain
    // greedy ones match the same; the `(?<![\\$:>])` lookbehind is checked by hand below
    static COUNT_SHARED: LazyLock<Regex> = LazyLock::new(|| bytes_regex(r"\b(?:class|interface|trait|enum)\s"));
    static DECL_SHARED: LazyLock<Regex> = LazyLock::new(|| {
        bytes_regex(
            r"\b(?:(?P<type>class|interface|trait|enum)\s+(?P<name>[a-zA-Z_\x7f-\xff:][a-zA-Z0-9_\x7f-\xff:\-]*)|(?P<ns>namespace)(?P<nsname>\s+[a-zA-Z_\x7f-\xff][a-zA-Z0-9_\x7f-\xff]*(?:\s*\\\s*[a-zA-Z_\x7f-\xff][a-zA-Z0-9_\x7f-\xff]*)*)?\s*[\{;])",
        )
    });
    // files are parsed on many threads: a clone per thread has its own match cache, so they don't contend for one
    thread_local! {
        static COUNT: Regex = COUNT_SHARED.clone();
        static DECL: Regex = DECL_SHARED.clone();
    }

    let raw = match std::fs::read(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            bail!("File at \"{path}\" does not exist, check your classmap definitions")
        }
        Err(_) => bail!("File at \"{path}\" is not readable, check its permissions"),
    };
    let contents = strip_whitespace(&raw);

    // return early if there is no chance of matching anything in this file
    let max_matches = COUNT.with(|re| re.find_iter(&contents).take(2).count());
    if max_matches == 0 {
        return Ok(vec![]);
    }
    let contents = clean(&contents, max_matches);

    let classes = DECL.with(|decl| {
        let mut classes = vec![];
        let mut namespace = String::new();
        let mut pos = 0;
        while let Some(caps) = decl.captures_at(&contents, pos) {
            let m = caps.get(0).unwrap();
            if m.start() > 0 && matches!(contents[m.start() - 1], b'\\' | b'$' | b':' | b'>') {
                pos = m.start() + 1;
                continue;
            }
            pos = m.end();
            if caps.name("ns").is_some() {
                let ns = caps.name("nsname").map_or(&b""[..], |m| m.as_bytes());
                let ns: Vec<u8> = ns.iter().copied().filter(|b| !matches!(b, b' ' | b'\t' | b'\r' | b'\n')).collect();
                namespace = format!("{}\\", String::from_utf8_lossy(&ns));
                continue;
            }
            let mut name = String::from_utf8_lossy(&caps["name"]).into_owned();
            // skip anon classes extending/implementing
            if name == "extends" || name == "implements" {
                continue;
            }
            if let Some(xhp) = name.strip_prefix(':') {
                // an XHP class, https://github.com/facebook/xhp
                name = format!("xhp_{}", xhp.replace('-', "_").replace(':', "__"));
            } else if caps["type"].eq_ignore_ascii_case(b"enum")
                && let Some(colon) = name.rfind(':')
            {
                // `enum Foo: int` captures the colon (and the backing type)
                name.truncate(colon);
            }
            classes.push(format!("{namespace}{name}").trim_start_matches('\\').to_string());
        }
        classes
    });
    Ok(classes)
}

fn is_label_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c >= 0x80
}

/// `strcspn($s, $reject, $from)`.
fn strcspn(s: &[u8], from: usize, reject: &[u8]) -> usize {
    s.get(from..).map_or(0, |rest| rest.iter().position(|c| reject.contains(c)).unwrap_or(rest.len()))
}

/// `<<<[ \t]*+(['"]?)(LABEL)\1(?:\r\n|\n|\r)` at `i`: (end of the match, label).
fn heredoc_start(s: &[u8], i: usize) -> Option<(usize, &[u8])> {
    let mut j = i + 3;
    if s.get(i..j)? != b"<<<" {
        return None;
    }
    while matches!(s.get(j), Some(b' ' | b'\t')) {
        j += 1;
    }
    let quote = s.get(j).copied().filter(|c| matches!(c, b'\'' | b'"'));
    j += quote.is_some() as usize;
    let label_start = j;
    if !s.get(j).is_some_and(|&c| is_label_char(c) && !c.is_ascii_digit()) {
        return None;
    }
    while s.get(j).is_some_and(|&c| is_label_char(c)) {
        j += 1;
    }
    let label = &s[label_start..j];
    if let Some(q) = quote {
        if s.get(j) != Some(&q) {
            return None;
        }
        j += 1;
    }
    match s.get(j..) {
        Some([b'\r', b'\n', ..]) => Some((j + 2, label)),
        Some([b'\n' | b'\r', ..]) => Some((j + 1, label)),
        _ => None,
    }
}

/// `PhpFileCleaner::skipHeredoc()`: index after the closing label.
fn heredoc_end(s: &[u8], mut i: usize, label: &[u8]) -> usize {
    while i < s.len() {
        match s[i] {
            b'\t' | b' ' => {
                i += 1;
                continue;
            }
            c if c == label[0] && s[i..].starts_with(label) && !s.get(i + label.len()).is_some_and(|&c| is_label_char(c)) => {
                return i + label.len();
            }
            _ => {}
        }
        i += strcspn(s, i, b"\r\n");
        i += s[i..].iter().take_while(|c| matches!(c, b'\r' | b'\n')).count();
    }
    i
}

/// End of a `'`, `"` or `` ` `` string starting at `i`, the way PHP's lexer scans it.
fn string_end(s: &[u8], i: usize) -> usize {
    let quote = s[i];
    let mut j = i + 1;
    while j < s.len() {
        match s[j] {
            b'\\' => j += 2,
            c if c == quote => return j + 1,
            _ => j += 1,
        }
    }
    s.len()
}

fn line_comment_end(s: &[u8], mut i: usize) -> usize {
    while i < s.len() && !matches!(s[i], b'\r' | b'\n') && !(s[i] == b'?' && s.get(i + 1) == Some(&b'>')) {
        i += 1;
    }
    i
}

/// `php_strip_whitespace()`: drops comments and collapses whitespace runs to one space,
/// keeping strings, heredocs and inline HTML verbatim.
fn strip_whitespace(s: &[u8]) -> Vec<u8> {
    let n = s.len();
    let mut out = Vec::with_capacity(n);
    let mut i = 0;
    while i < n {
        let Some(open) = s[i..].windows(2).position(|w| w == b"<?") else {
            out.extend_from_slice(&s[i..]);
            break;
        };
        out.extend_from_slice(&s[i..i + open + 2]);
        i += open + 2;
        // comments leave `prev_space` alone, every other token resets it
        let mut prev_space = false;
        while i < n {
            let next = s.get(i + 1).copied();
            match s[i] {
                b' ' | b'\t' | b'\n' | b'\r' => {
                    i += s[i..].iter().take_while(|c| matches!(c, b' ' | b'\t' | b'\n' | b'\r')).count();
                    if !prev_space {
                        out.push(b' ');
                        prev_space = true;
                    }
                }
                // line comments end before a newline or `?>`
                b'#' if next != Some(b'[') => i = line_comment_end(s, i),
                b'/' if next == Some(b'/') => i = line_comment_end(s, i),
                b'/' if next == Some(b'*') => i = s[i + 2..].windows(2).position(|w| w == b"*/").map_or(n, |p| i + 2 + p + 2),
                b'?' if next == Some(b'>') => {
                    let newline = match &s[i + 2..] {
                        [b'\r', b'\n', ..] => 2,
                        [b'\n' | b'\r', ..] => 1,
                        _ => 0,
                    };
                    out.extend_from_slice(&s[i..i + 2 + newline]);
                    i += 2 + newline;
                    break;
                }
                c => {
                    let end = match c {
                        b'\'' | b'"' | b'`' => string_end(s, i),
                        b'<' => heredoc_start(s, i).map_or(i + 1, |(body, label)| heredoc_end(s, body, label)),
                        _ => i + 1,
                    };
                    out.extend_from_slice(&s[i..end]);
                    i = end;
                    prev_space = false;
                }
            }
        }
    }
    out
}

/// `PhpFileCleaner::clean()`: strings and heredocs become `null`, comments and inline HTML go.
/// With a single possible match it stops right after the first type declaration.
fn clean(s: &[u8], max_matches: usize) -> Vec<u8> {
    const REJECT: &[u8] = b"?\"'</cite";
    let n = s.len();
    let peek = |i: usize, c: u8| i + 1 < n && s[i + 1] == c;
    let mut out = Vec::with_capacity(n);
    let mut i = 0;
    'php: while i < n {
        while i < n {
            if s[i] == b'<' && peek(i, b'?') {
                i += 2;
                break;
            }
            i += 1;
        }
        out.extend_from_slice(b"<?");
        while i < n {
            let c = s[i];
            if c == b'?' && peek(i, b'>') {
                out.extend_from_slice(b"?>");
                i += 2;
                continue 'php;
            }
            if c == b'"' || c == b'\'' {
                i = skip_string(s, i, c);
                out.extend_from_slice(b"null");
                continue;
            }
            if c == b'<'
                && peek(i, b'<')
                && let Some((body, label)) = heredoc_start(s, i)
            {
                i = heredoc_end(s, body, label);
                out.extend_from_slice(b"null");
                continue;
            }
            if c == b'/' && peek(i, b'/') {
                i += strcspn(s, i, b"\r\n");
                continue;
            }
            if c == b'/' && peek(i, b'*') {
                i += 2;
                while i < n {
                    i += strcspn(s, i, b"*");
                    if peek(i, b'/') {
                        i += 2;
                        break;
                    }
                    i += 1;
                }
                continue;
            }
            if max_matches == 1
                && let Some(end) = type_declaration(s, i)
            {
                out.extend_from_slice(&s[i - 1..end]);
                return out;
            }
            let skip = strcspn(s, i + 1, REJECT);
            out.extend_from_slice(&s[i..i + 1 + skip]);
            i += 1 + skip;
        }
    }
    out
}

/// `PhpFileCleaner::skipString()`.
fn skip_string(s: &[u8], mut i: usize, delimiter: u8) -> usize {
    let n = s.len();
    i += 1;
    while i < n {
        i += strcspn(s, i, &[b'\\', delimiter]);
        if i >= n {
            break;
        }
        if s[i] == b'\\' && i + 1 < n && (s[i + 1] == b'\\' || s[i + 1] == delimiter) {
            i += 2;
            continue;
        }
        i += 1;
        if s[i - 1] == delimiter {
            break;
        }
    }
    i
}

/// The cleaner's `{.\b(?<![\$:>])TYPE\s++NAME}Ais` type pattern, tried at `i - 1`: its end.
fn type_declaration(s: &[u8], i: usize) -> Option<usize> {
    let keyword: &[u8] = match s[i] {
        b'c' => b"class",
        b'i' => b"interface",
        b't' => b"trait",
        b'e' => b"enum",
        _ => return None,
    };
    let prev = s[i - 1];
    if !s[i..].starts_with(keyword) || prev.is_ascii_alphanumeric() || matches!(prev, b'_' | b'$' | b':' | b'>') {
        return None;
    }
    let mut j = i + keyword.len();
    let spaces = s[j..].iter().take_while(|c| matches!(c, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)).count();
    j += spaces;
    let name_start = |c: u8| c.is_ascii_alphabetic() || c == b'_' || c == b':' || c >= 0x7f;
    if spaces == 0 || !s.get(j).is_some_and(|&c| name_start(c)) {
        return None;
    }
    Some(j + 1 + s[j + 1..].iter().take_while(|&&c| name_start(c) || c.is_ascii_digit() || c == b'-').count())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classes(src: &str) -> Vec<String> {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let file = std::env::temp_dir().join(format!("komposer-autoload-parser-{}-{n}.php", std::process::id()));
        std::fs::write(&file, src).unwrap();
        let classes = find_classes(file.to_str().unwrap()).unwrap();
        std::fs::remove_file(file).unwrap();
        classes
    }

    #[test]
    fn strips_comments_and_whitespace() {
        assert_eq!(strip_whitespace(b"a <?php # c's\n$x = '#';  // y ?>\n<b>"), b"a <?php $x = '#'; ?>\n<b>");
        assert_eq!(strip_whitespace(b"<?php #[Attr] /* c */ class\n\nA {}"), b"<?php #[Attr] class A {}");
    }

    #[test]
    fn finds_declarations() {
        assert_eq!(classes("<?php # it's\nclass A {}"), ["A"]);
        assert_eq!(classes("<?php namespace X\\Y; $class = 1; Foo::class; class B extends C {}"), ["X\\Y\\B"]);
        assert_eq!(classes("<?php enum E: string {} interface I {} trait T {}"), ["E", "I", "T"]);
        assert_eq!(classes("<?php class :foo:bar-baz {}"), ["xhp_foo__bar_baz"]);
        assert_eq!(classes("<?php $a = <<<EOT\nclass X {}\nEOT;\nclass Y {}"), ["Y"]);
        assert_eq!(classes("<?php new class extends Foo {}; class Z {}"), ["Z"]);
    }
}
