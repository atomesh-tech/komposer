//! `Util\Url` and `Util\ComposerMirror`.

use komposer_package::Package;
use komposer_package::config::Config;
use md5::Digest;
use regex::{Captures, Regex};
use serde_json::Value;
use std::sync::LazyLock;

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub(crate) fn md5_hex(data: &str) -> String {
    hex(&md5::Md5::digest(data.as_bytes()))
}

pub(crate) fn sha1_hex(data: &[u8]) -> String {
    hex(&sha1::Sha1::digest(data))
}

fn regex(re: &str) -> Regex {
    Regex::new(re).unwrap()
}

/// `Url::sanitize()`: hides passwords and tokens in URLs embedded in messages.
pub fn sanitize(url: &str) -> String {
    static TOKEN: LazyLock<Regex> = LazyLock::new(|| regex(r"([&?]access_token=)[^&]+"));
    static AUTH: LazyLock<Regex> =
        LazyLock::new(|| regex(r"(?i)(?:(?P<prefix>[a-z0-9][a-z0-9+.-]*://)|\A)(?P<user>[^:/\s?#]*)(?::(?P<password>[^\s/?#]+))?@"));
    static GITHUB_TOKEN: LazyLock<Regex> = LazyLock::new(|| regex(r"^([a-f0-9]{12,}|gh[a-z]_[a-zA-Z0-9_.-]+|github_pat_[a-zA-Z0-9_]+)$"));
    const NON_SECRET: [&str; 5] = ["private-token", "x-token-auth", "oauth2", "gitlab-ci-token", "x-oauth-basic"];
    let url = TOKEN.replace_all(url, "${1}***");
    AUTH.replace_all(&url, |c: &Captures| {
        let prefix = c.name("prefix").map_or("", |m| m.as_str());
        let user = &c["user"];
        let user = if !NON_SECRET.contains(&user) && (GITHUB_TOKEN.is_match(user) || user.len() >= 12) {
            format!("{}***", &user[..3.min(user.len())])
        } else {
            user.to_string()
        };
        match c.name("password") {
            Some(_) => format!("{prefix}{user}:***@"),
            None => format!("{prefix}{user}@"),
        }
    })
    .into_owned()
}

/// `Url::stripCredentials()`.
pub fn strip_credentials(url: &str) -> String {
    static RE: LazyLock<Regex> = LazyLock::new(|| regex(r"://[^/\s?#]+@"));
    RE.replace_all(url, "://").into_owned()
}

/// PHP `rawurlencode()`.
pub(crate) fn raw_url_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// `parse_url($url, PHP_URL_HOST)` for the URLs Composer deals with.
pub(crate) fn host(url: &str) -> Option<&str> {
    let rest = &url[url.find("://")? + 3..];
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = authority.rsplit('@').next()?;
    Some(host.split(':').next().unwrap_or(host))
}

/// `parse_url($url, PHP_URL_PATH)`.
pub(crate) fn path(url: &str) -> &str {
    let url = url.split(['?', '#']).next().unwrap_or(url);
    match url.find("://") {
        Some(i) => url[i + 3..].find('/').map_or("", |p| &url[i + 3 + p..]),
        None => url,
    }
}

pub(crate) fn string_list(config: &Config, key: &str) -> Vec<String> {
    match config.get(key) {
        Some(Value::Array(a)) => a.iter().filter_map(|v| v.as_str().map(String::from)).collect(),
        _ => vec![],
    }
}

/// `Url::updateDistReference()`: point GitHub/GitLab/Bitbucket archive URLs at `$ref`.
pub fn update_dist_reference(config: &Config, url: &str, reference: &str) -> String {
    static GH_LEGACY: LazyLock<Regex> = LazyLock::new(|| regex(r"(?i)^https?://(?:www\.)?github\.com/([^/]+)/([^/]+)/(zip|tar)ball/(.+)$"));
    static GH_ARCHIVE: LazyLock<Regex> =
        LazyLock::new(|| regex(r"(?i)^https?://(?:www\.)?github\.com/([^/]+)/([^/]+)/archive/.+\.(zip|tar)(?:\.gz)?$"));
    static GH_API: LazyLock<Regex> = LazyLock::new(|| regex(r"(?i)^https?://api\.github\.com/repos/([^/]+)/([^/]+)/(zip|tar)ball(?:/.+)?$"));
    static BITBUCKET: LazyLock<Regex> =
        LazyLock::new(|| regex(r"(?i)^https?://(?:www\.)?bitbucket\.org/([^/]+)/([^/]+)/get/(.+)\.(zip|tar\.gz|tar\.bz2)$"));
    static GITLAB: LazyLock<Regex> = LazyLock::new(|| {
        regex(r"(?i)^https?://(?:www\.)?gitlab\.com/api/v[34]/projects/([^/]+)/repository/archive\.(zip|tar\.gz|tar\.bz2|tar)\?sha=.+$")
    });
    static GH_ENTERPRISE: LazyLock<Regex> = LazyLock::new(|| regex(r"(?i)(/repos/[^/]+/[^/]+/(zip|tar)ball)(?:/.+)?$"));
    static GL_SELF_HOSTED: LazyLock<Regex> =
        LazyLock::new(|| regex(r"(?i)(/api/v[34]/projects/[^/]+/repository/archive\.(?:zip|tar\.gz|tar\.bz2|tar)\?sha=).+$"));

    let host = host(url).unwrap_or("");
    let github = |c: Captures| format!("https://api.github.com/repos/{}/{}/{}ball/{reference}", &c[1], &c[2], &c[3]);
    match host {
        "api.github.com" | "github.com" | "www.github.com" => {
            match GH_LEGACY.captures(url).or_else(|| GH_ARCHIVE.captures(url)).or_else(|| GH_API.captures(url)) {
                Some(c) => github(c),
                None => url.to_string(),
            }
        }
        "bitbucket.org" | "www.bitbucket.org" => match BITBUCKET.captures(url) {
            Some(c) => format!("https://bitbucket.org/{}/{}/get/{reference}.{}", &c[1], &c[2], &c[4]),
            None => url.to_string(),
        },
        "gitlab.com" | "www.gitlab.com" => match GITLAB.captures(url) {
            Some(c) => format!("https://gitlab.com/api/v4/projects/{}/repository/archive.{}?sha={reference}", &c[1], &c[2]),
            None => url.to_string(),
        },
        h if string_list(config, "github-domains").iter().any(|d| d == h) => {
            GH_ENTERPRISE.replace(url, |c: &Captures| format!("{}/{reference}", &c[1])).into_owned()
        }
        h if string_list(config, "gitlab-domains").iter().any(|d| d == h) => {
            GL_SELF_HOSTED.replace(url, |c: &Captures| format!("{}{reference}", &c[1])).into_owned()
        }
        _ => url.to_string(),
    }
}

/// `ComposerMirror::processUrl()`.
pub(crate) fn mirror_url(mirror: &str, p: &Package, reference: Option<&str>, kind: &str) -> String {
    static REF: LazyLock<Regex> = LazyLock::new(|| regex(r"^([a-f0-9]*|%reference%)$"));
    let reference = reference.filter(|r| !r.is_empty()).map(|r| if REF.is_match(r) { r.to_string() } else { md5_hex(r) });
    let version = if p.version.contains('/') { md5_hex(&p.version) } else { p.version.clone() };
    mirror
        .replace("%package%", &p.name)
        .replace("%version%", &version)
        .replace("%reference%", reference.as_deref().unwrap_or(""))
        .replace("%type%", kind)
        .replace("%prettyVersion%", &p.pretty_version)
}

/// `ComposerMirror::processGitUrl()`.
pub(crate) fn mirror_git_url(mirror: &str, p: &Package, url: &str, kind: &str) -> String {
    static GH: LazyLock<Regex> = LazyLock::new(|| regex(r"^(?:(?:https?|git)://github\.com/|git@github\.com:)([^/]+)/(.+?)(?:\.git)?$"));
    static BB: LazyLock<Regex> = LazyLock::new(|| regex(r"^https://bitbucket\.org/([^/]+)/(.+?)(?:\.git)?/?$"));
    static OTHER: LazyLock<Regex> = LazyLock::new(|| regex(r"(?i)[^a-z0-9_.-]"));
    let normalized = if let Some(c) = GH.captures(url) {
        format!("gh-{}/{}", &c[1], &c[2])
    } else if let Some(c) = BB.captures(url) {
        format!("bb-{}/{}", &c[1], &c[2])
    } else {
        OTHER.replace_all(url.trim_matches('/'), "-").into_owned()
    };
    mirror.replace("%package%", &p.name).replace("%normalizedUrl%", &normalized).replace("%type%", kind)
}

/// `Package::getUrls()`: the URL plus its mirrors, preferred mirrors first.
pub(crate) fn package_urls(p: &Package, dist: bool) -> Vec<String> {
    let Some(loc) = (if dist { &p.dist } else { &p.source }) else { return vec![] };
    if loc.url.is_empty() {
        return vec![];
    }
    let reference = loc.reference.as_deref();
    let url = if dist && loc.url.contains('%') { mirror_url(&loc.url, p, reference, &loc.kind) } else { loc.url.clone() };
    let mut urls = vec![url.clone()];
    for mirror in loc.mirrors.as_ref().and_then(Value::as_array).into_iter().flatten() {
        let Some(m) = mirror.get("url").and_then(Value::as_str) else { continue };
        let mirror_url = match (dist, loc.kind.as_str()) {
            (true, _) => mirror_url(m, p, reference, &loc.kind),
            (false, "git" | "hg") => mirror_git_url(m, p, &url, &loc.kind),
            _ => continue,
        };
        if !urls.contains(&mirror_url) {
            if mirror.get("preferred").and_then(Value::as_bool).unwrap_or(false) {
                urls.insert(0, mirror_url);
            } else {
                urls.push(mirror_url);
            }
        }
    }
    urls
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ArchiveDownloaderTest::testProcessUrl*`.
    #[test]
    fn rewrites_dist_references() {
        let config = Config::new(false, None);
        for url in [
            "https://api.github.com/repos/composer/composer/zipball/master",
            "https://api.github.com/repos/composer/composer/tarball/master",
            "https://github.com/composer/composer/zipball/master",
            "https://www.github.com/composer/composer/tarball/master",
            "https://github.com/composer/composer/archive/master.zip",
            "https://github.com/composer/composer/archive/master.tar.gz",
        ] {
            let kind = if url.contains("tar") { "tar" } else { "zip" };
            assert_eq!(update_dist_reference(&config, url, "ref"), format!("https://api.github.com/repos/composer/composer/{kind}ball/ref"));
        }
        for (url, ext) in [
            ("https://bitbucket.org/davereid/drush-virtualhost/get/77ca490c26ac818e024d1138aa8bd3677d1ef21f", "zip"),
            ("https://bitbucket.org/davereid/drush-virtualhost/get/master", "tar.gz"),
            ("https://bitbucket.org/davereid/drush-virtualhost/get/v1.0", "tar.bz2"),
        ] {
            assert_eq!(
                update_dist_reference(&config, &format!("{url}.{ext}"), "ref"),
                format!("https://bitbucket.org/davereid/drush-virtualhost/get/ref.{ext}")
            );
        }
    }

    #[test]
    fn sanitizes_credentials() {
        assert_eq!(sanitize("https://user:secret@example.org/x"), "https://user:***@example.org/x");
        assert_eq!(sanitize("https://ghp_abcdefghijkl@github.com/x"), "https://ghp***@github.com/x");
        assert_eq!(sanitize("https://x/y?access_token=abc&b=1"), "https://x/y?access_token=***&b=1");
        assert_eq!(strip_credentials("https://user:secret@example.org/x"), "https://example.org/x");
        assert_eq!(path("https://example.com/script.js?x=1"), "/script.js");
        assert_eq!(host("https://u:p@api.github.com:443/x"), Some("api.github.com"));
    }

    #[test]
    fn mirrors() {
        let mut p = Package::new("foo/bar", "1.0.0.0", "1.0.0");
        p.dist = Some(komposer_package::Location {
            kind: "zip".into(),
            url: "https://example.org/dist.zip".into(),
            reference: Some("abc123".into()),
            mirrors: Some(serde_json::json!([
                {"url": "https://mirror/%package%/%version%/%reference%.%type%", "preferred": true},
                {"url": "https://other/%package%/%prettyVersion%", "preferred": false},
            ])),
            ..Default::default()
        });
        assert_eq!(
            package_urls(&p, true),
            ["https://mirror/foo/bar/1.0.0.0/abc123.zip", "https://example.org/dist.zip", "https://other/foo/bar/1.0.0"]
        );
        assert_eq!(mirror_git_url("https://m/%normalizedUrl%.%type%", &p, "https://github.com/foo/bar.git", "git"), "https://m/gh-foo/bar.git");
    }
}
