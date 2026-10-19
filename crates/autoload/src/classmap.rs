//! `ClassMapGenerator` and `ClassMap` of composer/class-map-generator 1.7.3, including the
//! Symfony Finder walk it scans directories with.

use crate::parser::find_classes;
use crate::php::{is_absolute_path, normalize_path, substr};
use anyhow::{Context, Result, bail};
use indexmap::IndexMap;
use regex::Regex;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

/// `Composer\ClassMapGenerator\ClassMap`. The map is kept sorted (`ClassMap::sort()`).
#[derive(Debug, Default)]
pub struct ClassMap {
    map: BTreeMap<String, String>,
    ambiguous: IndexMap<String, Vec<String>>,
    /// path => warnings
    psr_violations: IndexMap<String, Vec<String>>,
}

impl ClassMap {
    /// `getMap()`: class => path, sorted by class name.
    pub fn map(&self) -> &BTreeMap<String, String> {
        &self.map
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// `getClassPath()`.
    pub fn class_path(&self, class: &str) -> Option<&str> {
        self.map.get(class).map(String::as_str)
    }

    /// `addClass()`.
    pub fn add_class(&mut self, class: &str, path: &str) {
        self.psr_violations.shift_remove(&path.replace('\\', "/"));
        self.map.insert(class.into(), path.into());
    }

    /// `getPsrViolations()`.
    pub fn psr_violations(&self) -> Vec<&str> {
        self.psr_violations.values().flatten().map(String::as_str).collect()
    }

    /// `getAmbiguousClasses()`: class => the paths not used for it. `filter_duplicates` leaves out
    /// paths in test, fixture, example or stub directories.
    pub fn ambiguous_classes(&self, filter_duplicates: bool) -> IndexMap<&str, Vec<&str>> {
        static DUPLICATES_FILTER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)/(test|fixture|example|stub)s?/").unwrap());
        self.ambiguous
            .iter()
            .map(|(class, paths)| {
                let paths = paths.iter().map(String::as_str).filter(|p| !filter_duplicates || !DUPLICATES_FILTER.is_match(&p.replace('\\', "/")));
                (class.as_str(), paths.collect::<Vec<_>>())
            })
            .filter(|(_, paths)| !paths.is_empty())
            .collect()
    }

    /// `clearPsrViolationsByPath()`.
    pub fn clear_psr_violations_by_path(&mut self, prefix: &str) {
        let prefix = prefix.replace('\\', "/");
        let prefix = prefix.trim_end_matches('/');
        self.psr_violations.retain(|path, _| path != prefix && !path.starts_with(&format!("{prefix}/")));
    }

    fn add_psr_violation(&mut self, warning: String, path: &str) {
        let path = path.replace('\\', "/").trim_end_matches('/').to_string();
        self.psr_violations.entry(path).or_default().push(warning);
    }
}

/// The autoload rule a directory is scanned for; PSR rules carry their namespace prefix.
#[derive(Clone, Copy, Debug)]
pub enum Scan<'a> {
    Classmap,
    Psr0(&'a str),
    Psr4(&'a str),
}

/// `Composer\ClassMapGenerator\ClassMapGenerator`. Relative paths resolve against `cwd`,
/// which stands in for PHP's working directory.
pub struct ClassMapGenerator {
    extensions: Vec<String>,
    scanned_files: Option<HashSet<String>>,
    class_map: ClassMap,
    cwd: String,
    /// realpath of directories, like PHP's realpath cache
    real_dirs: HashMap<String, String>,
}

impl ClassMapGenerator {
    pub fn new(extensions: &[&str], cwd: &Path) -> Result<ClassMapGenerator> {
        let cwd = cwd.canonicalize().with_context(|| format!("Could not resolve {}", cwd.display()))?;
        Ok(ClassMapGenerator {
            extensions: extensions.iter().map(|e| e.to_string()).collect(),
            scanned_files: None,
            class_map: ClassMap::default(),
            cwd: cwd.to_string_lossy().into_owned(),
            real_dirs: HashMap::new(),
        })
    }

    /// `realpath()`: a file that is not a symlink resolves through its (cached) directory.
    fn realpath(&mut self, file_path: &str) -> Option<String> {
        let resolve = |p: &str| std::fs::canonicalize(p).ok().map(|p| p.to_string_lossy().into_owned());
        let meta = std::fs::symlink_metadata(file_path).ok()?;
        let (dir, name) = file_path.rsplit_once('/').filter(|_| !meta.file_type().is_symlink())?;
        let dir = if dir.is_empty() { "/" } else { dir };
        if !self.real_dirs.contains_key(dir) {
            let real = resolve(dir)?;
            self.real_dirs.insert(dir.to_string(), real);
        }
        Some(format!("{}/{name}", self.real_dirs[dir].trim_end_matches('/')))
    }

    /// `avoidDuplicateScans()`: never scan the same file twice across `scan_paths()` calls.
    pub fn avoid_duplicate_scans(&mut self) {
        self.scanned_files = Some(HashSet::new());
    }

    pub fn class_map(&self) -> &ClassMap {
        &self.class_map
    }

    pub fn into_class_map(self) -> ClassMap {
        self.class_map
    }

    fn resolve(&self, path: &str) -> PathBuf {
        if is_absolute_path(path) { PathBuf::from(path) } else { Path::new(&self.cwd).join(path) }
    }

    /// `scanPaths()` for a file or directory path (`*` wildcards allowed in directories).
    /// `excluded` matches file paths to skip; `excluded_dirs` are directory names to skip.
    pub fn scan_paths(&mut self, path: &str, excluded: Option<&Regex>, scan: Scan, excluded_dirs: &[&str]) -> Result<()> {
        let files = if !path.is_empty() && self.resolve(path).is_file() {
            vec![path.to_string()]
        } else if (!path.is_empty() && self.resolve(path).is_dir()) || path.contains('*') {
            self.find_files(path, excluded_dirs)?
        } else {
            bail!("Could not scan for classes inside \"{path}\" which does not appear to be a file nor a folder");
        };

        let mut candidates = vec![];
        for file in files {
            let extension = file.rsplit('/').next().and_then(|name| name.rsplit_once('.')).map_or("", |(_, ext)| ext);
            if !self.extensions.iter().any(|e| e == extension) {
                continue;
            }
            let file_path = if is_absolute_path(&file) { collapse_separators(&file) } else { normalize_path(&format!("{}/{file}", self.cwd)) };
            let real_path = match self.realpath(&file_path) {
                Some(p) => p,
                None => std::fs::canonicalize(&file_path)
                    .map(|p| p.to_string_lossy().into_owned())
                    .map_err(|_| anyhow::anyhow!("realpath of {file_path} failed to resolve, got false"))?,
            };
            if self.scanned_files.as_ref().is_some_and(|s| s.contains(&real_path)) {
                continue;
            }
            // the excluded paths are realpath'd, but the file may sit below a symlink in the project
            if excluded.is_some_and(|re| re.is_match(&real_path.replace('\\', "/")) || re.is_match(&file_path.replace('\\', "/"))) {
                continue;
            }
            candidates.push((file_path, real_path));
        }

        let parsed = parse_all(&candidates);
        for ((file_path, real_path), classes) in candidates.iter().zip(parsed) {
            // a symlink may have led to the same file earlier in this scan
            if self.scanned_files.as_ref().is_some_and(|s| s.contains(real_path)) {
                continue;
            }
            let mut classes = classes?;
            if let Scan::Psr0(namespace) | Scan::Psr4(namespace) = scan {
                classes = self.filter_by_namespace(classes, file_path, namespace, scan, path);
            }
            // without a valid PSR class the file may still be matched by a later rule
            if (matches!(scan, Scan::Classmap) || !classes.is_empty())
                && let Some(scanned) = &mut self.scanned_files
            {
                scanned.insert(real_path.clone());
            }
            for class in classes {
                match self.class_map.class_path(&class) {
                    None => self.class_map.add_class(&class, file_path),
                    Some(existing) if existing != file_path => {
                        self.class_map.ambiguous.entry(class).or_default().push(file_path.clone());
                    }
                    Some(_) => {}
                }
            }
        }
        Ok(())
    }

    /// `filterByNamespace()`: drops classes PSR-0/4 autoloading could not find in that file.
    fn filter_by_namespace(&mut self, classes: Vec<String>, file_path: &str, base_namespace: &str, scan: Scan, base_path: &str) -> Vec<String> {
        let sub_path = substr(file_path, base_path.len() + 1);
        let real_sub_path = sub_path.rfind('.').map_or(sub_path, |dot| &sub_path[..dot]);
        let (valid, rejected): (Vec<String>, Vec<String>) = classes.into_iter().partition(|class| match scan {
            Scan::Psr0(_) => {
                if !base_namespace.is_empty() && !class.starts_with(base_namespace) {
                    return false;
                }
                let sub_path = match class.rfind('\\') {
                    Some(i) => format!("{}{}", class[..=i].replace('\\', "/"), class[i + 1..].replace('_', "/")),
                    None => class.replace('_', "/"),
                };
                sub_path == real_sub_path
            }
            _ => {
                let sub_namespace = if base_namespace.is_empty() { class.as_str() } else { substr(class, base_namespace.len()) };
                sub_namespace.replace('\\', "/") == real_sub_path
            }
        });
        // warn only if no valid classes, else silently skip invalid
        if !valid.is_empty() {
            return valid;
        }
        let kind = if matches!(scan, Scan::Psr0(_)) { "psr-0" } else { "psr-4" };
        let cwd = normalize_path(&self.cwd);
        let short = |p: &str| {
            let p = normalize_path(p);
            p.strip_prefix(&cwd).map_or(p.clone(), |rest| format!(".{rest}"))
        };
        let (short_path, short_base_path) = (short(file_path), short(base_path));
        for class in rejected {
            let warning = format!(
                "Class {class} located in {short_path} does not comply with {kind} autoloading standard (rule: {base_namespace} => {short_base_path}). Skipping."
            );
            self.class_map.add_psr_violation(warning, file_path);
        }
        vec![]
    }

    /// `Finder::create()->files()->followLinks()->name(extensions)->in($path)->exclude($excludedDirs)`,
    /// with Finder's default VCS and dot file exclusions. Unsorted, like Finder: entries come
    /// in directory order, depth first.
    fn find_files(&self, path: &str, excluded_dirs: &[&str]) -> Result<Vec<String>> {
        const VCS: [&str; 9] = [".svn", "_svn", "CVS", "_darcs", ".arch-params", ".monotone", ".bzr", ".git", ".hg"];
        let roots = if self.resolve(path).is_dir() {
            vec![normalize_dir(path).to_string()]
        } else {
            let dirs = self.glob_dirs(path);
            if dirs.is_empty() {
                bail!("The \"{path}\" directory does not exist.");
            }
            dirs.iter().map(|d| normalize_dir(d).to_string()).collect()
        };
        let mut files = vec![];
        let mut stack: Vec<(String, std::vec::IntoIter<std::fs::DirEntry>)> = vec![];
        let read = |dir: &str| -> Result<std::vec::IntoIter<std::fs::DirEntry>> {
            let entries = std::fs::read_dir(self.resolve(dir))
                .and_then(|rd| rd.collect::<std::io::Result<Vec<_>>>())
                .with_context(|| format!("Could not read directory \"{dir}\""))?;
            Ok(entries.into_iter())
        };
        for root in roots {
            stack.push((root.clone(), read(&root)?));
            while let Some((dir, entries)) = stack.last_mut() {
                let Some(entry) = entries.next() else {
                    stack.pop();
                    continue;
                };
                let name = entry.file_name().to_string_lossy().into_owned();
                let pathname = format!("{dir}/{name}");
                let file_type = entry.file_type()?;
                let (is_dir, is_file) = match file_type.is_symlink() {
                    true => std::fs::metadata(self.resolve(&pathname)).map_or((false, false), |m| (m.is_dir(), m.is_file())),
                    false => (file_type.is_dir(), file_type.is_file()),
                };
                // dot files and dirs are ignored, all files below a dot dir included
                let hidden = name.starts_with('.') && name.len() > 1;
                if is_dir && !hidden && !VCS.contains(&name.as_str()) && !excluded_dirs.contains(&name.as_str()) {
                    let children = read(&pathname)?;
                    stack.push((pathname, children));
                } else if is_file && !hidden && self.extensions.iter().any(|e| name.ends_with(&format!(".{e}"))) {
                    files.push(pathname);
                }
            }
        }
        Ok(files)
    }

    /// `glob($pattern, GLOB_ONLYDIR)`, sorted, for `*` and `?` wildcards.
    fn glob_dirs(&self, pattern: &str) -> Vec<String> {
        let mut found = vec![String::new()];
        for (i, part) in pattern.split('/').enumerate() {
            let mut next = vec![];
            for base in &found {
                let prefix = if i == 0 { String::new() } else { format!("{base}/") };
                if !part.contains(['*', '?']) {
                    next.push(format!("{prefix}{part}"));
                    continue;
                }
                let dir = match (base.as_str(), i) {
                    ("", 0) => ".",
                    ("", _) => "/",
                    (base, _) => base,
                };
                let Ok(entries) = std::fs::read_dir(self.resolve(dir)) else { continue };
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if wildcard_match(part.as_bytes(), name.as_bytes()) && (!name.starts_with('.') || part.starts_with('.')) {
                        next.push(format!("{prefix}{name}"));
                    }
                }
            }
            found = next;
        }
        let mut dirs: Vec<String> = found.into_iter().filter(|d| self.resolve(d).is_dir()).collect();
        dirs.sort();
        dirs
    }
}

/// Parses files in parallel, results in input order. Small scans stay on the calling thread: most
/// autoload paths hold a handful of files and spawning threads for them costs more than it saves.
fn parse_all(candidates: &[(String, String)]) -> Vec<Result<Vec<String>>> {
    const FILES_PER_THREAD: usize = 128;
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    let threads = cores.min(candidates.len() / FILES_PER_THREAD);
    if threads <= 1 {
        return candidates.iter().map(|(path, _)| find_classes(path)).collect();
    }
    let chunk = candidates.len().div_ceil(threads);
    std::thread::scope(|s| {
        let handles: Vec<_> =
            candidates.chunks(chunk).map(|c| s.spawn(move || c.iter().map(|(path, _)| find_classes(path)).collect::<Vec<_>>())).collect();
        handles.into_iter().flat_map(|h| h.join().unwrap()).collect()
    })
}

/// `Finder::normalizeDir()`.
fn normalize_dir(dir: &str) -> &str {
    if dir == "/" { dir } else { dir.trim_end_matches('/') }
}

/// `preg_replace('{(?<!:)[\\\\/]{2,}}', '/', $path)`.
fn collapse_separators(path: &str) -> String {
    let b = path.as_bytes();
    let mut out = String::with_capacity(path.len());
    let mut i = 0;
    while i < b.len() {
        let run = b[i..].iter().take_while(|c| matches!(c, b'/' | b'\\')).count();
        if run == 0 {
            let c = path[i..].chars().next().unwrap();
            out.push(c);
            i += c.len_utf8();
            continue;
        }
        // a run right after `:` keeps its first separator
        let keep = (i > 0 && b[i - 1] == b':') as usize;
        out.push_str(&path[i..i + keep]);
        if run - keep >= 2 {
            out.push('/');
        } else {
            out.push_str(&path[i + keep..i + run]);
        }
        i += run;
    }
    out
}

fn wildcard_match(pattern: &[u8], name: &[u8]) -> bool {
    match (pattern.first(), name.first()) {
        (None, None) => true,
        (Some(b'*'), _) => wildcard_match(&pattern[1..], name) || (!name.is_empty() && wildcard_match(pattern, &name[1..])),
        (Some(b'?'), Some(_)) => wildcard_match(&pattern[1..], &name[1..]),
        (Some(p), Some(n)) if p == n => wildcard_match(&pattern[1..], &name[1..]),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn psr_violations_by_path() {
        let mut map = ClassMap::default();
        map.add_psr_violation("vendor".into(), "/p/vendor/a/A.php");
        map.add_psr_violation("vendored".into(), "/p/vendored/B.php");
        map.add_psr_violation("root".into(), "/p/src/C.php");
        map.clear_psr_violations_by_path("/p/vendor/");
        assert_eq!(map.psr_violations(), ["vendored", "root"]);
        // a class found later for that file clears its violations
        map.add_class("C", "/p/src/C.php");
        assert_eq!(map.psr_violations(), ["vendored"]);
    }

    #[test]
    fn separators() {
        assert_eq!(collapse_separators("/a//b\\\\c/d"), "/a/b/c/d");
        assert_eq!(collapse_separators("phar://x"), "phar://x");
        assert_eq!(collapse_separators("a:///b"), "a://b");
        assert!(wildcard_match(b"a*c?", b"abbcd") && !wildcard_match(b"a*c", b"abd"));
    }
}
