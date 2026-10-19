//! `search`: `SearchCommand`.

use crate::cli::SearchArgs;
use crate::composer::Composer;
use anyhow::{Result, bail};
use komposer_package::io::Io;
use komposer_package::json::{self, Flags};
use komposer_package::repository::{Repository, SearchMode};
use komposer_repository::platform::{PlatformRepository, detected_runtime};
use serde_json::{Map, Value, json};
use std::path::PathBuf;
use std::sync::Arc;

/// `SearchCommand::execute()`.
pub fn search(io: &Arc<Io>, args: &SearchArgs, run_scripts: bool) -> Result<i32> {
    let _ = run_scripts;
    if !["text", "json"].contains(&args.format.as_str()) {
        io.write_error(&format!("Unsupported format \"{}\". See help for supported formats.", args.format));
        return Ok(1);
    }
    // without a (valid) project, only the global config applies
    let c = match Composer::load(io.clone()) {
        Ok(c) => c,
        Err(_) => Composer::from_parts(io.clone(), std::env::current_dir()?, PathBuf::from("./composer.json"), "{}".into(), Map::new())?,
    };
    let mode = match (args.only_name, args.only_vendor) {
        (true, true) => bail!("--only-name and --only-vendor cannot be used together"),
        (true, false) => SearchMode::Name,
        (false, true) => SearchMode::Vendor,
        (false, false) => SearchMode::Fulltext,
    };
    let mut query = args.tokens.join(" ");
    if mode != SearchMode::Fulltext {
        query = preg_quote(&query);
    }
    let kind = args.package_type.as_deref();

    let mut repos: Vec<Arc<dyn Repository>> = vec![Arc::new(c.installed()?), Arc::new(PlatformRepository::new(&json!([]), detected_runtime())?)];
    repos.extend(c.repositories()?);
    let mut results: Vec<Value> = vec![];
    for repo in repos {
        results.extend(repo.search(&query, mode, kind)?);
    }

    if args.format == "json" {
        io.write(&json::encode(&Value::Array(results), Flags::FILE));
        return Ok(0);
    }
    let width = terminal_width();
    let str_of = |r: &Value, k: &str| r.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let name_length = results.iter().map(|r| str_of(r, "name").len()).max().unwrap_or(0) + 1;
    for r in &results {
        let name = str_of(r, "name");
        let mut description = str_of(r, "description");
        let abandoned = r.get("abandoned").is_some_and(komposer_package::loader::php_truthy);
        let warning = if abandoned { "<warning>! Abandoned !</warning> " } else { "" };
        // like PHP, the budget counts the warning's tags and bytes rather than characters
        let remaining = width as i64 - name_length as i64 - warning.len() as i64 - 2;
        if description.len() as i64 > remaining {
            description = format!("{}...", truncate(&description, (remaining - 3).max(0) as usize));
        }
        let padding = " ".repeat(name_length - name.len());
        match r.get("url").and_then(Value::as_str) {
            Some(url) => io.write(&format!("<href={}>{name}</>{padding}{warning}{description}", url.replace('<', "\\<"))),
            None => io.write(&format!("{name}{padding}{warning}{description}")),
        }
    }
    Ok(0)
}

/// `substr($s, 0, $len)`, kept on a character boundary.
fn truncate(s: &str, len: usize) -> &str {
    let mut end = len.min(s.len());
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// PHP `preg_quote()`.
fn preg_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if r".\+*?[^]$(){}=!<>|:-#/".contains(c) {
            out.push('\\');
        }
        if c == '\0' {
            out.push_str("\\000");
        } else {
            out.push(c);
        }
    }
    out
}

/// `BaseCommand::getTerminalWidth()`: `COLUMNS`, else `stty`, else 80; at least 80.
fn terminal_width() -> usize {
    let detected = std::env::var("COLUMNS").ok().map(|c| c.trim().parse::<i64>().unwrap_or(0)).unwrap_or_else(|| {
        let out = std::process::Command::new("stty").arg("-a").stdin(std::process::Stdio::inherit()).stderr(std::process::Stdio::null()).output();
        let text = out.map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
        let re = |p: &str| regex::Regex::new(p).unwrap().captures(&text).and_then(|c| c[1].parse::<i64>().ok());
        re(r"(?i)rows.\d+;.columns.(\d+);").or_else(|| re(r"(?i);.\d+.rows;.(\d+).columns")).unwrap_or(0)
    });
    let width = if detected == 0 { 80 } else { detected };
    width.max(80) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting_and_truncation() {
        assert_eq!(preg_quote("foo/bar.*"), r"foo\/bar\.\*");
        assert_eq!(truncate("héllo", 2), "h");
    }
}
