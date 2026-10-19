//! `VersionGuesser` (git only): the version of a checkout from its branch, tag or commit.

use crate::git;
use crate::util::{ProcessOutput, split_lines};
use komposer_package::io::Io;
use komposer_package::loader::GuessedVersion;
use regex::Regex;
use serde_json::{Map, Value};
use std::cmp::Ordering;
use std::path::Path;
use std::sync::LazyLock;

/// Result of `guessVersion()`; feature branches also report the branch's own version.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VersionData {
    pub version: String,
    pub pretty_version: String,
    pub commit: Option<String>,
    pub feature_version: Option<String>,
    pub feature_pretty_version: Option<String>,
}

/// Runs `git <args>` in the checkout being guessed.
type Exec<'a> = &'a (dyn Fn(&[&str]) -> ProcessOutput + Sync);

/// `VersionGuesser::guessVersion($packageConfig, $path)`.
pub fn guess_version(package_config: &Map<String, Value>, path: &Path, io: &Io) -> Option<VersionData> {
    let exec = |args: &[&str]| git::run(args, Some(path));
    let data = guess_git_version(package_config, &exec, io, path)?;
    Some(postprocess(data))
}

/// The root package version for `komposer_package::loader::load_root()`.
pub fn guess_root_version(package_config: &Map<String, Value>, path: &Path, io: &Io) -> Option<GuessedVersion> {
    guess_version(package_config, path, io).map(|v| GuessedVersion { pretty_version: v.pretty_version, version: v.version, commit: v.commit })
}

fn postprocess(mut data: VersionData) -> VersionData {
    static NINES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(\.9{7})+").unwrap());
    if data.feature_version.as_ref() == Some(&data.version) && data.feature_pretty_version.as_ref() == Some(&data.pretty_version) {
        data.feature_version = None;
        data.feature_pretty_version = None;
    }
    if data.version.ends_with("-dev") && NINES.is_match(&data.version) {
        data.pretty_version = NINES.replace_all(&data.version, ".x").into_owned();
    }
    if let Some(feature) = data.feature_version.as_ref().filter(|f| f.ends_with("-dev") && NINES.is_match(f)) {
        data.feature_pretty_version = Some(NINES.replace_all(feature, ".x").into_owned());
    }
    data
}

fn guess_git_version(config: &Map<String, Value>, exec: Exec, io: &Io, path: &Path) -> Option<VersionData> {
    static CURRENT: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^(?:\* ) *(\(no branch\)|\(detached from \S+\)|\(HEAD detached at \S+\)|\S+) *([a-f0-9]+) .*$").unwrap());
    static HEAD_REF: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^ *.+/HEAD ").unwrap());
    static BRANCH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(?:\* )? *((?:remotes/(?:origin|upstream)/)?[^\s/]+) *([a-f0-9]+) .*$").unwrap());
    let (mut version, mut pretty, mut commit) = (None, None, None);
    let mut feature: Option<(String, String)> = None;
    let mut detached = false;

    let out = exec(&["branch", "-a", "--no-color", "--no-abbrev", "-v"]);
    if out.ok() {
        let mut branches = vec![];
        let mut is_feature = false;
        for line in split_lines(&out.stdout) {
            if let Some(c) = CURRENT.captures(line) {
                let name = &c[1];
                if name == "(no branch)" || name.starts_with("(detached ") || name.starts_with("(HEAD detached at") {
                    version = Some(format!("dev-{}", &c[2]));
                    pretty = version.clone();
                    is_feature = true;
                    detached = true;
                } else {
                    version = Some(komposer_semver::normalize_branch(name));
                    pretty = Some(format!("dev-{name}"));
                    is_feature = is_feature_branch(config, name);
                }
                commit = Some(c[2].to_string());
            }
            if !HEAD_REF.is_match(line)
                && let Some(c) = BRANCH.captures(line)
            {
                branches.push(c[1].to_string());
            }
        }
        if is_feature && let (Some(v), Some(p)) = (&version, &pretty) {
            feature = Some((v.clone(), p.clone()));
            // try to find the best (nearest) version branch to assume this feature's version
            let (v, p) = guess_feature_version(config, v, branches, exec);
            (version, pretty) = (Some(v), Some(p));
        }
    }
    if let Err(e) = git::check_for_repo_ownership_error(&out.stderr, path) {
        io.warning(&e.to_string());
    }

    if version.is_none() || detached {
        let out = exec(&["describe", "--exact-match", "--tags"]);
        if out.ok()
            && let Ok(v) = komposer_semver::normalize(out.stdout.trim())
        {
            (version, pretty, feature) = (Some(v), Some(out.stdout.trim().to_string()), None);
        }
    }
    if commit.is_none() {
        let mut args = git::rev_list_command(&["--format=%H", "-n1", "HEAD"]);
        args.extend(git::no_show_signature_flags());
        let out = exec(&args);
        if out.ok() {
            commit = Some(git::parse_rev_list_output(&out.stdout).trim().to_string()).filter(|c| !c.is_empty());
        }
    }
    let (feature_version, feature_pretty_version) = feature.unzip();
    Some(VersionData { version: version?, pretty_version: pretty?, commit, feature_version, feature_pretty_version })
}

/// `guessFeatureVersion()`: the version branch with the fewest commits missing from this branch.
fn guess_feature_version(config: &Map<String, Value>, version: &str, mut branches: Vec<String>, exec: Exec) -> (String, String) {
    let mut result = (version.to_string(), version.to_string());
    let aliased = config.get("extra").and_then(|e| e.get("branch-alias")).and_then(|a| a.get(version)).is_some();
    // feature branches without a branch-alias, or using self.version, are versioned like the branch they came from
    if aliased && !serde_json::to_string(config).unwrap_or_default().contains("\"self.version\"") {
        return result;
    }
    let branch = version.strip_prefix("dev-").unwrap_or(version);
    if !is_feature_branch(config, branch) {
        return result;
    }
    // local branches first, then numeric ones below named ones in natural order
    branches.sort_by(|a, b| match (a.starts_with("remotes/"), b.starts_with("remotes/")) {
        (false, true) => Ordering::Less,
        (true, false) => Ordering::Greater,
        _ => natcasecmp(b, a),
    });
    static REMOTE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^remotes/\S+/").unwrap());
    let candidates: Vec<(usize, &String, String)> = branches
        .iter()
        .enumerate()
        .map(|(i, c)| (i, c, REMOTE.replace(c, "").into_owned()))
        .filter(|(_, c, v)| *c != branch && !is_feature_branch(config, v))
        .collect();
    let outputs: Vec<Option<usize>> = std::thread::scope(|s| {
        let handles: Vec<_> = candidates
            .chunks(30)
            .map(|chunk| {
                s.spawn(move || {
                    chunk
                        .iter()
                        .map(|(_, candidate, _)| {
                            let out = exec(&["rev-list", &format!("{candidate}..{branch}")]);
                            out.ok().then_some(out.stdout.len())
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
    });
    // the shortest diff wins; on equal diffs the later (older) branch
    let mut best: Option<(usize, usize)> = None;
    for ((index, _, candidate_version), len) in candidates.iter().zip(outputs) {
        let Some(len) = len else { continue };
        if best.is_none_or(|(best_len, best_index)| len < best_len || (len == best_len && best_index < *index)) {
            best = Some((len, *index));
            result = (komposer_semver::normalize_branch(candidate_version), format!("dev-{candidate_version}"));
        }
    }
    result
}

/// `isFeatureBranch()`: anything but `non-feature-branches` and the usual mainline names.
fn is_feature_branch(config: &Map<String, Value>, branch: &str) -> bool {
    let custom: Vec<&str> = config.get("non-feature-branches").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).collect();
    let pattern = format!(r"^({}|master|main|latest|next|current|support|tip|trunk|default|develop|\d+\..+)$", custom.join("|"));
    !Regex::new(&pattern).is_ok_and(|re| re.is_match(branch))
}

/// PHP `strnatcasecmp()`.
fn natcasecmp(a: &str, b: &str) -> Ordering {
    let (a, b) = (a.to_lowercase(), b.to_lowercase());
    let (mut x, mut y) = (a.as_bytes(), b.as_bytes());
    loop {
        match (x.first(), y.first()) {
            (None, None) => return Ordering::Equal,
            (None, _) => return Ordering::Less,
            (_, None) => return Ordering::Greater,
            (Some(c), Some(d)) if c.is_ascii_digit() && d.is_ascii_digit() => {
                let n = x.iter().take_while(|c| c.is_ascii_digit()).count();
                let m = y.iter().take_while(|c| c.is_ascii_digit()).count();
                let (p, q) = (
                    std::str::from_utf8(&x[..n]).unwrap_or("0").trim_start_matches('0'),
                    std::str::from_utf8(&y[..m]).unwrap_or("0").trim_start_matches('0'),
                );
                let ord = p.len().cmp(&q.len()).then_with(|| p.cmp(q));
                if ord != Ordering::Equal {
                    return ord;
                }
                (x, y) = (&x[n..], &y[m..]);
            }
            (Some(c), Some(d)) if c != d => return c.cmp(d),
            _ => (x, y) = (&x[1..], &y[1..]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{commit_file, git, git_repo};
    use serde_json::json;
    use std::collections::HashMap;

    const HASH: &str = "03a15d220da53c52eddd5f32ffca64a7b3801bea";
    const OTHER: &str = "13a15d220da53c52eddd5f32ffca64a7b3801bea";

    /// Answers git commands from a table, failing anything else.
    fn guess(config: Value, outputs: &[(&str, &str)]) -> VersionData {
        let table: HashMap<String, String> = outputs.iter().map(|(c, o)| (c.to_string(), o.to_string())).collect();
        let exec = move |args: &[&str]| {
            let key = args.join(" ");
            match table.get(&key) {
                Some(out) => ProcessOutput { code: 0, stdout: out.clone(), stderr: String::new() },
                None if key.starts_with("rev-list --") => ProcessOutput { code: 0, stdout: format!("{HASH}\n"), stderr: String::new() },
                None => ProcessOutput { code: 128, ..Default::default() },
            }
        };
        postprocess(guess_git_version(config.as_object().unwrap(), &exec, &Io::null(), Path::new(".")).unwrap())
    }

    const BRANCH: &str = "branch -a --no-color --no-abbrev -v";

    #[test]
    fn branch_and_commit() {
        let v = guess(json!({}), &[(BRANCH, &format!("* master {HASH} Commit message\n(no branch) {OTHER} Commit message\n"))]);
        assert_eq!((v.version.as_str(), v.pretty_version.as_str(), v.commit.as_deref()), ("dev-master", "dev-master", Some(HASH)));
        assert_eq!(v.feature_version, None);
        let v = guess(
            json!({"version": "self.version"}),
            &[(BRANCH, &format!("  arbitrary {HASH} Commit message\n* current {OTHER} Another message\n"))],
        );
        assert_eq!((v.version.as_str(), v.commit.as_deref()), ("dev-current", Some(OTHER)));
    }

    #[test]
    fn non_feature_branches_config() {
        let v = guess(
            json!({"version": "self.version", "non-feature-branches": ["arbitrary"]}),
            &[
                (BRANCH, &format!("  arbitrary {HASH} Commit message\n* feature {OTHER} Another message\n")),
                ("rev-list arbitrary..feature", &format!("{OTHER}\n")),
            ],
        );
        assert_eq!((v.version.as_str(), v.commit.as_deref()), ("dev-arbitrary", Some(OTHER)));
        assert_eq!((v.feature_version.as_deref(), v.feature_pretty_version.as_deref()), (Some("dev-feature"), Some("dev-feature")));

        let v = guess(
            json!({"version": "self.version", "non-feature-branches": ["latest-.*"]}),
            &[
                (BRANCH, &format!("  latest-testing {HASH} Commit message\n* feature {OTHER} Another message\n")),
                ("rev-list latest-testing..feature", &format!("{OTHER}\n")),
            ],
        );
        assert_eq!(v.version, "dev-latest-testing");
        assert_eq!(v.feature_version.as_deref(), Some("dev-feature"));

        let v = guess(
            json!({"version": "self.version", "non-feature-branches": ["latest-.*"]}),
            &[(BRANCH, &format!("* latest-testing {HASH} Commit message\n  current {OTHER} Another message\n  master {OTHER} Another message\n"))],
        );
        assert_eq!((v.version.as_str(), v.commit.as_deref(), v.feature_version), ("dev-latest-testing", Some(HASH), None));
    }

    #[test]
    fn detached_heads_and_tags() {
        for head in ["(no branch)", "(HEAD detached at FETCH_HEAD)", "(HEAD detached at 03a15d220)"] {
            assert_eq!(guess(json!({}), &[(BRANCH, &format!("* {head} {HASH} Commit message\n"))]).version, format!("dev-{HASH}"));
        }
        let v = guess(
            json!({}),
            &[
                (BRANCH, "* (HEAD detached at v2.0.5-alpha2) 433b98d4218c181bae01865901aac045585e8a1a Commit message\n"),
                ("describe --exact-match --tags", "v2.0.5-alpha2"),
            ],
        );
        assert_eq!(v.version, "2.0.5.0-alpha2");
        let v = guess(
            json!({}),
            &[
                (BRANCH, "* (HEAD detached at 1.0.0) c006f0c12bbbf197b5c071ffb1c0e9812bb14a4d Commit message\n"),
                ("describe --exact-match --tags", "1.0.0"),
            ],
        );
        assert_eq!((v.version.as_str(), v.pretty_version.as_str()), ("1.0.0.0", "1.0.0"));
        assert_eq!(guess(json!({}), &[(BRANCH, &format!("* foo {HASH} Commit message\n"))]).version, "dev-foo");
    }

    #[test]
    fn numeric_and_remote_branches() {
        let v = guess(json!({}), &[(BRANCH, &format!("* 1.5 {HASH} Commit message\n"))]);
        assert_eq!((v.version.as_str(), v.pretty_version.as_str()), ("1.5.9999999.9999999-dev", "1.5.x-dev"));
        let v = guess(
            json!({"version": "self.version"}),
            &[
                (BRANCH, &format!("* feature-branch {HASH} Commit message\nremotes/origin/1.5 {HASH} Commit message\n")),
                ("rev-list remotes/origin/1.5..feature-branch", "\n"),
            ],
        );
        assert_eq!((v.version.as_str(), v.pretty_version.as_str()), ("1.5.9999999.9999999-dev", "1.5.x-dev"));
    }

    #[test]
    fn real_repository() {
        let repo = git_repo("guesser");
        let first = commit_file(&repo, "composer.json", "{}", "init");
        let v = guess_version(&Map::new(), &repo, &Io::null()).unwrap();
        assert_eq!((v.version.as_str(), v.pretty_version.as_str(), v.commit.as_deref()), ("dev-main", "dev-main", Some(first.as_str())));

        git(&repo, &["checkout", "-q", "-b", "my-feature"]);
        commit_file(&repo, "a.txt", "a", "feature work");
        let v = guess_version(&Map::new(), &repo, &Io::null()).unwrap();
        assert_eq!((v.version.as_str(), v.feature_version.as_deref()), ("dev-main", Some("dev-my-feature")));

        git(&repo, &["tag", "v1.2.0"]);
        git(&repo, &["checkout", "-q", "--detach", "v1.2.0"]);
        let v = guess_version(&Map::new(), &repo, &Io::null()).unwrap();
        assert_eq!((v.version.as_str(), v.pretty_version.as_str()), ("1.2.0.0", "v1.2.0"));
        let root = guess_root_version(&Map::new(), &repo, &Io::null()).unwrap();
        assert_eq!(root.version, "1.2.0.0");
    }

    #[test]
    fn natural_sort() {
        let mut v = vec!["1.9", "1.10", "main", "2.0"];
        v.sort_by(|a, b| natcasecmp(b, a));
        assert_eq!(v, ["main", "2.0", "1.10", "1.9"]);
    }
}
