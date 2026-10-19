//! `Composer\Semver\VersionParser` plus the additions of Composer's own subclass.
//!
//! The PCRE patterns are translated for the `regex` crate in byte mode with Unicode off (`(?-u)`),
//! which matches PCRE's non-UTF semantics: ASCII-only `\d`, `\s` and `/i`. Possessive quantifiers
//! became greedy ones (none of them can give back anything the rest of its pattern could use), and
//! `$` became `\n?\z` because PCRE's `$` also matches before a final newline.

use crate::php;
use crate::{Constraint, Op, sort};
use regex::bytes::{Captures, Regex};
use std::collections::HashMap;
use std::fmt;
use std::sync::{LazyLock, Mutex};

/// `VersionParser::DEFAULT_BRANCH_ALIAS`.
pub const DEFAULT_BRANCH_ALIAS: &str = "9999999-dev";

/// `BasePackage::STABILITIES`. The discriminant is Composer's numeric priority.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Stability {
    Stable = 0,
    RC = 5,
    Beta = 10,
    Alpha = 15,
    Dev = 20,
}

impl Stability {
    pub fn priority(self) -> u8 {
        self as u8
    }

    /// `stable`, `RC`, `beta`, `alpha`, `dev`.
    pub fn as_str(self) -> &'static str {
        match self {
            Stability::Stable => "stable",
            Stability::RC => "RC",
            Stability::Beta => "beta",
            Stability::Alpha => "alpha",
            Stability::Dev => "dev",
        }
    }

    /// Case-insensitive parse of a stability name.
    pub fn parse(s: &str) -> Option<Stability> {
        match s.to_ascii_lowercase().as_str() {
            "stable" => Some(Stability::Stable),
            "rc" => Some(Stability::RC),
            "beta" => Some(Stability::Beta),
            "alpha" => Some(Stability::Alpha),
            "dev" => Some(Stability::Dev),
            _ => None,
        }
    }
}

impl fmt::Display for Stability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The `\UnexpectedValueException` thrown by the parser; the message matches Composer's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError(pub String);

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ParseError {}

/// A lazily compiled byte regex with PCRE's non-UTF semantics.
macro_rules! regex {
    ($($part:expr),+ $(,)?) => {{
        static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(concat!("(?-u)", $($part),+)).unwrap());
        &*RE
    }};
}

/// `$modifierRegex`: groups are stability, stability number, dev suffix.
macro_rules! modifier {
    () => {
        r"[._-]?(?:(stable|beta|b|RC|alpha|a|patch|pl|p)((?:[.-]?\d+)*)?)?([.-]?dev)?"
    };
}

/// The `$versionRegex` of `parseConstraint()`: groups 1-4 numbers, 5-7 modifier, 8 `.x-dev` wildcard.
macro_rules! version_regex {
    () => {
        concat!(r"v?(\d+)(?:\.(\d+))?(?:\.(\d+))?(?:\.(\d+))?(?:", modifier!(), r"|\.([xX*][.-]?dev))(?:\+[^\s]+)?")
    };
}

/// Capture group `i` of a match on `s`, `""` when unmatched like `preg_match()` fills it.
/// Groups always start and end next to ASCII bytes, so the slice is on char boundaries.
fn group<'a>(s: &'a str, caps: &Captures, i: usize) -> &'a str {
    caps.get(i).map_or("", |m| &s[m.range()])
}

/// PHP's `trim()`, which strips `" \t\n\r\0\x0B"` (no form feed, no Unicode spaces).
fn trim(s: &str) -> &str {
    s.trim_matches([' ', '\t', '\n', '\r', '\0', '\x0B'])
}

/// `VersionParser::parseStability()`.
pub fn parse_stability(version: &str) -> Stability {
    let stripped;
    let version = match regex!(r"(#.+)\n?\z").captures(version.as_bytes()).and_then(|c| c.get(1)) {
        Some(m) => {
            stripped = [&version[..m.start()], &version[m.end()..]].concat();
            &stripped
        }
        None => version,
    };
    if version.starts_with("dev-") || version.ends_with("-dev") {
        return Stability::Dev;
    }
    let lower = version.to_ascii_lowercase();
    // all optional, so this always matches (possibly empty, at the end)
    let Some(m) = regex!(r"(?i)", modifier!(), r"(?:\+.*)?\n?\z").captures(lower.as_bytes()) else {
        return Stability::Stable;
    };
    if !group(&lower, &m, 3).is_empty() {
        return Stability::Dev;
    }
    match group(&lower, &m, 1) {
        "beta" | "b" => Stability::Beta,
        "alpha" | "a" => Stability::Alpha,
        "rc" => Stability::RC,
        _ => Stability::Stable,
    }
}

/// `VersionParser::normalizeStability()`.
pub fn normalize_stability(stability: &str) -> Result<Stability, ParseError> {
    Stability::parse(stability).ok_or_else(|| {
        ParseError(format!(r#"Invalid stability string "{}", expected one of stable, RC, beta, alpha or dev"#, stability.to_ascii_lowercase()))
    })
}

/// `VersionParser::isValid()`.
pub fn is_valid(version: &str) -> bool {
    normalize(version).is_ok()
}

/// `VersionParser::normalize($version)`.
pub fn normalize(version: &str) -> Result<String, ParseError> {
    normalize_full(version, None)
}

/// `VersionParser::normalize($version, $fullVersion)`.
pub fn normalize_full(version: &str, full_version: Option<&str>) -> Result<String, ParseError> {
    let orig = trim(version);
    let full = full_version.unwrap_or(orig);
    let mut version = orig;

    // strip off aliasing
    if let Some(m) = regex!(r"^([^,\s]+) +as +([^,\s]+)\n?\z").captures(version.as_bytes()) {
        version = group(version, &m, 1);
    }
    // strip off stability flag
    if let Some(m) = regex!(r"(?i)@(?:stable|RC|beta|alpha|dev)\n?\z").find(version.as_bytes()) {
        version = &version[..m.start()];
    }
    // normalize master/trunk/default branches to dev-name for BC with 1.x as these used to be valid constraints
    if matches!(version, "master" | "trunk" | "default") {
        return Ok(format!("dev-{version}"));
    }
    // if requirement is branch-like, use full name
    if version.as_bytes().get(..4).is_some_and(|p| p.eq_ignore_ascii_case(b"dev-")) {
        return Ok(format!("dev-{}", &version[4..]));
    }
    // strip off build metadata
    if let Some(m) = regex!(r"^([^,\s+]+)\+[^\s]+\n?\z").captures(version.as_bytes()) {
        version = group(version, &m, 1);
    }

    let classical = regex!(r"(?i)^v?(\d{1,5})(\.\d+)?(\.\d+)?(\.\d+)?", modifier!(), r"\n?\z");
    let date = regex!(r"(?i)^v?(\d{4}(?:[.:-]?\d{2}){1,6}(?:[.:-]?\d{1,3}){0,2})", modifier!(), r"\n?\z");
    let matched = if let Some(m) = classical.captures(version.as_bytes()) {
        let part = |i| match group(version, &m, i) {
            "" => ".0",
            p => p,
        };
        Some((format!("{}{}{}{}", group(version, &m, 1), part(2), part(3), part(4)), m, 5))
    } else if let Some(m) = date.captures(version.as_bytes()) {
        let digits = group(version, &m, 1).chars().map(|c| if c.is_ascii_digit() { c } else { '.' }).collect();
        Some((digits, m, 2))
    } else {
        None
    };

    // add version modifiers if a version was matched
    if let Some((mut normalized, m, index)) = matched {
        let stability = group(version, &m, index);
        if !stability.is_empty() {
            // a case-sensitive check in PHP: `-STABLE` becomes `-stable`
            if stability == "stable" {
                return Ok(normalized);
            }
            normalized.push('-');
            normalized.push_str(match stability.to_ascii_lowercase().as_str() {
                "a" => "alpha",
                "b" => "beta",
                "p" | "pl" => "patch",
                "rc" => "RC",
                s => s,
            });
            normalized.push_str(group(version, &m, index + 1).trim_start_matches(['.', '-']));
        }
        if !group(version, &m, index + 2).is_empty() {
            normalized.push_str("-dev");
        }
        return Ok(normalized);
    }

    // match dev branches
    if let Some(m) = regex!(r"(?i)(.*?)[.-]?dev\n?\z").captures(version.as_bytes()) {
        let normalized = normalize_branch(group(version, &m, 1));
        // a branch ending with -dev is only valid if it is numeric
        // if it gets prefixed with dev- it means the branch name should
        // have had a dev- prefix already when passed to normalize
        if !normalized.contains("dev-") {
            return Ok(normalized);
        }
    }

    let quoted = regex::escape(version);
    let stability = "(?:@(?:stable|RC|beta|alpha|dev))?";
    let full_matches = |pattern: String| Regex::new(&pattern).is_ok_and(|re| re.is_match(full.as_bytes()));
    let extra = if full_matches(format!(r"(?-u) +as +{quoted}{stability}\n?\z")) {
        format!(r#" in "{full}", the alias must be an exact version"#)
    } else if full_matches(format!(r"(?-u)^{quoted}{stability} +as +")) {
        format!(r#" in "{full}", the alias source must be an exact version, if it is a branch name you should prefix it with dev-"#)
    } else {
        String::new()
    };
    Err(ParseError(format!(r#"Invalid version string "{orig}"{extra}"#)))
}

/// `VersionParser::parseNumericAliasPrefix()`.
pub fn parse_numeric_alias_prefix(branch: &str) -> Option<String> {
    let m = regex!(r"(?i)^((\d+\.)*\d+)(?:\.x)?-dev\n?\z").captures(branch.as_bytes())?;
    Some(format!("{}.", group(branch, &m, 1)))
}

/// `VersionParser::normalizeBranch()`.
pub fn normalize_branch(name: &str) -> String {
    let name = trim(name);
    let Some(m) = regex!(r"(?i)^v?(\d+)(\.(?:\d+|[xX*]))?(\.(?:\d+|[xX*]))?(\.(?:\d+|[xX*]))?\n?\z").captures(name.as_bytes()) else {
        return format!("dev-{name}");
    };
    let version: String = (1..5).map(|i| m.get(i).map_or(".x", |g| &name[g.range()])).collect();
    version.replace(['*', 'X'], "x").replace('x', "9999999") + "-dev"
}

/// `VersionParser::normalizeDefaultBranch()`.
pub fn normalize_default_branch(name: &str) -> String {
    match name {
        "dev-master" | "dev-default" | "dev-trunk" => DEFAULT_BRANCH_ALIAS.into(),
        _ => name.into(),
    }
}

/// `VersionParser::parseConstraints()` (cached, like Composer's subclass).
pub fn parse_constraints(constraints: &str) -> Result<Constraint, ParseError> {
    // ponytail: one global lock; shard it or go thread-local if parsing becomes contended
    static CACHE: LazyLock<Mutex<HashMap<String, Constraint>>> = LazyLock::new(Default::default);
    if let Some(parsed) = CACHE.lock().unwrap().get(constraints) {
        return Ok(parsed.clone());
    }
    let parsed = parse_constraints_uncached(constraints)?;
    CACHE.lock().unwrap().insert(constraints.to_owned(), parsed.clone());
    Ok(parsed)
}

/// The parent `Composer\Semver\VersionParser::parseConstraints()`.
fn parse_constraints_uncached(constraints: &str) -> Result<Constraint, ParseError> {
    let trimmed = trim(constraints);
    let mut or_groups = vec![];
    // split points are ASCII, so the pieces are valid UTF-8
    for or_constraint in regex!(r"\s*\|\|?\s*").split(trimmed.as_bytes()) {
        let or_constraint = std::str::from_utf8(or_constraint).expect("split at ASCII");
        let mut and_constraints = vec![];
        for and_constraint in split_and(or_constraint) {
            and_constraints.extend(parse_constraint(and_constraint)?);
        }
        or_groups.push(if and_constraints.len() == 1 {
            and_constraints.remove(0)
        } else {
            Constraint::Multi { constraints: and_constraints, conjunctive: true }
        });
    }
    Ok(Constraint::multi(or_groups, false))
}

/// `preg_split('{(?<!^|as|[=>< ,]) *(?<!-)[, ](?!-) *(?!,|as|$)}', $s)`, hand-written for the lookarounds.
fn split_and(s: &str) -> Vec<&str> {
    let b = s.as_bytes();
    let spaces_from = |i: usize| b[i..].iter().take_while(|&&c| c == b' ').count();
    // the match starting at `start`, trying the greedy ` *` lengths from longest to shortest
    let match_at = |start: usize| -> Option<usize> {
        if start == 0 || b[..start].ends_with(b"as") || b"=>< ,".contains(&b[start - 1]) {
            return None;
        }
        for t in (start..=start + spaces_from(start)).rev() {
            if b[t - 1] == b'-' || !matches!(b.get(t), Some(b',' | b' ')) || b.get(t + 1) == Some(&b'-') {
                continue;
            }
            for u in (t + 1..=t + 1 + spaces_from(t + 1)).rev() {
                let at_end = u == b.len() || (u + 1 == b.len() && b[u] == b'\n');
                if !(at_end || b[u] == b',' || b[u..].starts_with(b"as")) {
                    return Some(u);
                }
            }
        }
        None
    };
    let (mut pieces, mut last, mut pos) = (vec![], 0, 0);
    while pos < b.len() {
        match match_at(pos) {
            Some(end) => {
                pieces.push(&s[last..pos]);
                (last, pos) = (end, end);
            }
            None => pos += 1,
        }
    }
    pieces.push(&s[last..]);
    pieces
}

/// `VersionParser::parseConstraint()`: one or two constraints.
fn parse_constraint(mut constraint: &str) -> Result<Vec<Constraint>, ParseError> {
    use Op::*;
    // strip off aliasing
    if let Some(m) = regex!(r"^([^,\s]+) +as +([^,\s]+)\n?\z").captures(constraint.as_bytes()) {
        constraint = group(constraint, &m, 1);
    }
    // strip @stability flags, and keep it for later use
    let mut stability_modifier = None;
    if let Some(m) = regex!(r"(?i)^([^,\s]*?)@(stable|RC|beta|alpha|dev)\n?\z").captures(constraint.as_bytes()) {
        let stability = group(constraint, &m, 2);
        if stability != "stable" {
            stability_modifier = Some(stability);
        }
        constraint = match group(constraint, &m, 1) {
            "" => "*",
            c => c,
        };
    }
    // get rid of #refs as those are used by composer only
    if let Some(m) = regex!(r"(?i)^(dev-[^,\s@]+?|[^,\s@]+?\.x-dev)#.+\n?\z").captures(constraint.as_bytes()) {
        constraint = group(constraint, &m, 1);
    }

    if let Some(m) = regex!(r"(?i)^(v)?[xX*](\.[xX*])*\n?\z").captures(constraint.as_bytes()) {
        if m.get(1).is_some() || m.get(2).is_some() {
            return Ok(vec![Constraint::single(Ge, "0.0.0.0-dev")]);
        }
        return Ok(vec![Constraint::Any]);
    }

    let parts = |m: &Captures, from: usize| [0, 1, 2, 3].map(|i| group(constraint, m, from + i));
    // no `-dev` suffix on the lower bound when the constraint has a stability or is a `.x-dev` wildcard
    let low_suffix = |m: &Captures, from: usize| {
        let g = |i| group(constraint, m, from + i);
        if g(4).is_empty() && g(6).is_empty() && g(7).is_empty() { "-dev" } else { "" }
    };

    // Tilde Range
    //
    // Like wildcard constraints, unsuffixed tilde constraints say that they must be greater than the previous
    // version, to ensure that unstable instances of the current version are allowed. However, if a stability
    // suffix is added to the constraint, then a >= match on the current version is used instead.
    if let Some(m) = regex!(r"(?i)^~>?", version_regex!(), r"\n?\z").captures(constraint.as_bytes()) {
        if constraint.starts_with("~>") {
            return Err(ParseError(format!(
                r#"Could not parse version constraint {constraint}: Invalid operator "~>", you probably meant to use the "~" operator"#
            )));
        }
        // Work out which position in the version we are operating at
        let mut position = (2..=4).rev().find(|&i| !group(constraint, &m, i).is_empty()).unwrap_or(1);
        // when matching 2.x-dev or 3.0.x-dev we have to shift the second or third number, despite no second/third number matching above
        if !group(constraint, &m, 8).is_empty() {
            position += 1;
        }
        let low = normalize(&format!("{}{}", &constraint[1..], low_suffix(&m, 1)))?;
        // For upper bound, we increment the position of one more significance,
        // but highPosition = 0 would be illegal
        let high = manipulate_version_string(parts(&m, 1), (position - 1).max(1), 1).unwrap_or_default();
        return Ok(vec![Constraint::single(Ge, low), Constraint::single(Lt, high + "-dev")]);
    }

    // Caret Range
    //
    // Allows changes that do not modify the left-most non-zero digit in the [major, minor, patch] tuple.
    // In other words, this allows patch and minor updates for versions 1.0.0 and above, patch updates for
    // versions 0.X >=0.1.0, and no updates for versions 0.0.X
    if let Some(m) = regex!(r"(?i)^\^", version_regex!(), r"()\n?\z").captures(constraint.as_bytes()) {
        let g = |i| group(constraint, &m, i);
        // Work out which position in the version we are operating at
        let position = if g(1) != "0" || g(2).is_empty() {
            1
        } else if g(2) != "0" || g(3).is_empty() {
            2
        } else {
            3
        };
        let low = normalize(&format!("{}{}", &constraint[1..], low_suffix(&m, 1)))?;
        let high = manipulate_version_string(parts(&m, 1), position, 1).unwrap_or_default();
        return Ok(vec![Constraint::single(Ge, low), Constraint::single(Lt, high + "-dev")]);
    }

    // X Range
    //
    // Any of X, x, or * may be used to "stand in" for one of the numeric values in the [major, minor, patch] tuple.
    // A partial version range is treated as an X-Range, so the special character is in fact optional.
    if let Some(m) = regex!(r"^v?(\d+)(?:\.(\d+))?(?:\.(\d+))?(?:\.[xX*])+\n?\z").captures(constraint.as_bytes()) {
        let position = (2..=3).rev().find(|&i| !group(constraint, &m, i).is_empty()).unwrap_or(1);
        let low = manipulate_version_string(parts(&m, 1), position, 0).unwrap_or_default() + "-dev";
        let high = manipulate_version_string(parts(&m, 1), position, 1).unwrap_or_default() + "-dev";
        if low == "0.0.0.0-dev" {
            return Ok(vec![Constraint::single(Lt, high)]);
        }
        return Ok(vec![Constraint::single(Ge, low), Constraint::single(Lt, high)]);
    }

    // Hyphen Range
    //
    // Specifies an inclusive set. If a partial version is provided as the first version in the inclusive range,
    // then the missing pieces are replaced with zeroes. If a partial version is provided as the second version in
    // the inclusive range, then all versions that start with the supplied parts of the tuple are accepted, but
    // nothing that would be greater than the provided tuple parts.
    if let Some(m) = regex!(r"(?i)^(", version_regex!(), r") +- +(", version_regex!(), r")()\n?\z").captures(constraint.as_bytes()) {
        // groups: 1 = from (2-9 its parts), 10 = to (11-18 its parts); PHP's `$empty()` counts "0" as set
        let g = |i| group(constraint, &m, i);
        let low = normalize(g(1))? + low_suffix(&m, 2);
        let upper = if (!g(12).is_empty() && !g(13).is_empty()) || !g(15).is_empty() || !g(17).is_empty() || !g(18).is_empty() {
            Constraint::single(Le, normalize(g(10))?)
        } else {
            // validate to version
            normalize(g(10))?;
            let position = if g(12).is_empty() { 1 } else { 2 };
            let high = manipulate_version_string(parts(&m, 11), position, 1).unwrap_or_default();
            Constraint::single(Lt, high + "-dev")
        };
        return Ok(vec![Constraint::single(Ge, low), upper]);
    }

    // Basic Comparators
    let m = regex!(r"^(<>|!=|>=?|<=?|==?)?\s*(.*)").captures(constraint.as_bytes()).expect("always matches");
    let (op, version) = (Op::parse(group(constraint, &m, 1)).unwrap_or(Eq), group(constraint, &m, 2));
    let parsed = normalize(version)
        .or_else(|e| {
            // recover from an invalid constraint like foobar-dev which should be dev-foobar
            // except if the constraint uses a known operator, in which case it must be a parse error
            match version.strip_suffix("-dev") {
                Some(name) if version.bytes().all(|c| c.is_ascii_alphanumeric() || b"-./".contains(&c)) => normalize(&format!("dev-{name}")),
                _ => Err(e),
            }
        })
        .map(|mut normalized| {
            if let Some(modifier) = stability_modifier.filter(|_| op != Eq && parse_stability(&normalized) == Stability::Stable) {
                normalized = format!("{normalized}-{modifier}");
            } else if matches!(op, Lt | Ge) && !regex!(r"(?i)-", modifier!(), r"\n?\z").is_match(version.as_bytes()) && !version.starts_with("dev-") {
                normalized.push_str("-dev");
            }
            vec![Constraint::single(op, normalized)]
        });
    parsed.map_err(|e| ParseError(format!("Could not parse version constraint {constraint}: {e}")))
}

/// `VersionParser::manipulateVersionString()`: increment, decrement, or simply pad a version number.
/// `parts` are segments 1-4 and `position` is 1-based; `None` on a carry overflow.
pub(crate) fn manipulate_version_string(parts: [&str; 4], mut position: usize, increment: i64) -> Option<String> {
    let mut parts = parts.map(String::from);
    for i in (1..=4).rev() {
        if i > position {
            parts[i - 1] = "0".into();
        } else if i == position && increment != 0 {
            parts[i - 1] = php::add(&parts[i - 1], increment);
            // If $matches[$i] was 0, carry the decrement
            if parts[i - 1].starts_with('-') {
                parts[i - 1] = "0".into();
                position -= 1;
                // Return null on a carry overflow
                if i == 1 {
                    return None;
                }
            }
        }
    }
    Some(parts.join("."))
}

/// Composer's `VersionParser::parseNameVersionPairs()`: `(name, Option<version>)`.
/// `is_platform` is `PlatformRepository::isPlatformPackage`, injected to avoid a dependency.
pub fn parse_name_version_pairs(pairs: &[String], is_platform: impl Fn(&str) -> bool) -> Vec<(String, Option<String>)> {
    // `(?<=[a-z0-9_/-])\*|\*(?=[a-z0-9_/-])` with /i
    let has_glued_star = |s: &str| {
        let b = s.as_bytes();
        let name_char = |c: Option<&u8>| c.is_some_and(|&c| c.is_ascii_alphanumeric() || b"_/-".contains(&c));
        (0..b.len()).any(|i| b[i] == b'*' && (name_char(i.checked_sub(1).map(|j| &b[j])) || name_char(b.get(i + 1))))
    };
    let mut result = vec![];
    let mut i = 0;
    while i < pairs.len() {
        let trimmed = trim(&pairs[i]);
        let mut pair = match regex!(r"^([^=: ]+)[=: ](.*)\n?\z").captures(trimmed.as_bytes()) {
            Some(m) => format!("{} {}", group(trimmed, &m, 1), group(trimmed, &m, 2)),
            None => trimmed.to_string(),
        };
        if !pair.contains(' ')
            && let Some(next) = pairs.get(i + 1)
            && !next.contains('/')
            && !has_glued_star(next)
            && !is_platform(next)
        {
            pair = format!("{pair} {next}");
            i += 1;
        }
        // `if (strpos($pair, ' '))`: a space at offset 0 does not count
        result.push(match pair.find(' ') {
            Some(at) if at > 0 => (pair[..at].to_string(), Some(pair[at + 1..].to_string())),
            _ => (pair, None),
        });
        i += 1;
    }
    result
}

/// Composer's `VersionParser::isUpgrade()`.
pub fn is_upgrade(normalized_from: &str, normalized_to: &str) -> bool {
    if normalized_from == normalized_to {
        return true;
    }
    let (from, to) = (normalize_default_branch(normalized_from), normalize_default_branch(normalized_to));
    if from.starts_with("dev-") || to.starts_with("dev-") {
        return true;
    }
    let mut sorted = [to, from.clone()];
    sort(&mut sorted);
    sorted[0] == from
}
