//! `Composer\Util\Url`.

use regex::{Captures, Regex};
use std::sync::LazyLock;

/// `Url::NON_SECRET_CREDENTIALS`.
pub const NON_SECRET_CREDENTIALS: [&str; 5] = ["private-token", "x-token-auth", "oauth2", "gitlab-ci-token", "x-oauth-basic"];

/// `GitHub::GITHUB_TOKEN_REGEX`.
pub(crate) static GITHUB_TOKEN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:[a-f0-9]{12,}|gh[a-z]_[a-zA-Z0-9_.-]+|github_pat_[a-zA-Z0-9_]+)$").unwrap());

/// The parts of `parse_url()` Composer looks at.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct UrlParts<'a> {
    pub scheme: Option<&'a str>,
    pub host: Option<&'a str>,
    pub port: Option<u16>,
    pub path: &'a str,
}

pub(crate) fn parse(url: &str) -> UrlParts<'_> {
    static RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^(?:([a-zA-Z][a-zA-Z0-9+.-]*):)?(?://(?:[^@/?#]*@)?(\[[^\]]*\]|[^:/?#]*)(?::(\d*))?)?([^?#]*)").unwrap());
    let Some(c) = RE.captures(url) else { return UrlParts { path: url, ..Default::default() } };
    UrlParts {
        scheme: c.get(1).map(|m| m.as_str()),
        host: c.get(2).map(|m| m.as_str()).filter(|h| !h.is_empty()),
        port: c.get(3).and_then(|m| m.as_str().parse().ok()),
        path: c.get(4).map_or("", |m| m.as_str()),
    }
}

/// `Url::getOrigin()`: the host (with port) used to look up credentials.
pub fn origin(gitlab_domains: &[String], url: &str) -> String {
    if url.starts_with("file://") {
        return url.to_string();
    }
    let parts = parse(url);
    let mut origin = parts.host.unwrap_or("").to_string();
    if let Some(port) = parts.port.filter(|p| *p != 0) {
        origin = format!("{origin}:{port}");
    }
    if origin.ends_with(".github.com") && origin != "codeload.github.com" {
        return "github.com".into();
    }
    if origin == "repo.packagist.org" {
        return "packagist.org".into();
    }
    if origin.is_empty() {
        origin = url.to_string();
    }
    // GitLab can be installed in a non-root context (i.e. gitlab.com/foo), find the matching configured domain
    if !origin.contains('/') && !gitlab_domains.contains(&origin) {
        static PORT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^([^/]+):\d+").unwrap());
        for domain in gitlab_domains {
            // configured domains may spell out a port the URL omits
            let bc = PORT.replace(domain, "$1");
            if !domain.is_empty() && (bc == origin || bc.starts_with(&format!("{origin}/"))) {
                return domain.clone();
            }
        }
    }
    origin
}

/// `Url::isAllowedRedirect()`: only http(s) redirects are followed.
pub fn is_allowed_redirect(url: &str) -> bool {
    parse(url).scheme.is_some_and(|s| s.eq_ignore_ascii_case("http") || s.eq_ignore_ascii_case("https"))
}

/// `Url::sanitize()`: masks passwords, tokens and `access_token` query values, anywhere in the string.
pub fn sanitize(url: &str) -> String {
    static TOKEN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"([&?]access_token=)[^&]+").unwrap());
    static CREDS: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)(?:(?P<prefix>[a-z0-9][a-z0-9+.-]*://)|\A)(?P<user>[^:/\s?#]*)(?::(?P<password>[^\s/?#]*))?@").unwrap());
    let url = TOKEN.replace_all(url, "${1}***");
    CREDS
        .replace_all(&url, |c: &Captures| {
            let prefix = c.name("prefix").map_or("", |m| m.as_str());
            let user = sanitize_username(&c["user"]);
            match c.name("password").filter(|p| !p.as_str().is_empty()) {
                Some(_) => format!("{prefix}{user}:***@"),
                None => format!("{prefix}{user}@"),
            }
        })
        .into_owned()
}

/// `Url::stripCredentials()` as of Composer 2.10: removes everything between `://` and `@`.
pub fn strip_credentials(url: &str) -> String {
    static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"://[^/\s?#]+@").unwrap());
    RE.replace_all(url, "://").into_owned()
}

/// `Url::sanitizeUsername()`.
pub fn sanitize_username(user: &str) -> String {
    if NON_SECRET_CREDENTIALS.contains(&user) {
        return user.to_string();
    }
    if GITHUB_TOKEN.is_match(user) || user.len() >= 12 {
        return format!("{}***", user.get(..3).unwrap_or(user));
    }
    user.to_string()
}

/// `Url::updateDistReference()`: points known archive URLs at another commit.
pub fn update_dist_reference(github_domains: &[String], gitlab_domains: &[String], url: &str, reference: &str) -> String {
    static GH_LEGACY: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)^https?://(?:www\.)?github\.com/([^/]+)/([^/]+)/(zip|tar)ball/(.+)$").unwrap());
    static GH_ARCHIVE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)^https?://(?:www\.)?github\.com/([^/]+)/([^/]+)/archive/.+\.(zip|tar)(?:\.gz)?$").unwrap());
    static GH_API: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)^https?://api\.github\.com/repos/([^/]+)/([^/]+)/(zip|tar)ball(?:/.+)?$").unwrap());
    static BB: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)^https?://(?:www\.)?bitbucket\.org/([^/]+)/([^/]+)/get/(.+)\.(zip|tar\.gz|tar\.bz2)$").unwrap());
    static GL: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)^https?://(?:www\.)?gitlab\.com/api/v[34]/projects/([^/]+)/repository/archive\.(zip|tar\.gz|tar\.bz2|tar)\?sha=.+$").unwrap()
    });
    static GH_ENTERPRISE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)(/repos/[^/]+/[^/]+/(zip|tar)ball)(?:/.+)?$").unwrap());
    static GL_ENTERPRISE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)(/api/v[34]/projects/[^/]+/repository/archive\.(?:zip|tar\.gz|tar\.bz2|tar)\?sha=).+$").unwrap());

    let host = parse(url).host.unwrap_or("").to_string();
    let api = |c: Captures| format!("https://api.github.com/repos/{}/{}/{}ball/{reference}", &c[1], &c[2], &c[3]);
    match host.as_str() {
        "api.github.com" | "github.com" | "www.github.com" => {
            if let Some(c) = GH_LEGACY.captures(url).or_else(|| GH_ARCHIVE.captures(url)).or_else(|| GH_API.captures(url)) {
                return api(c);
            }
        }
        "bitbucket.org" | "www.bitbucket.org" => {
            if let Some(c) = BB.captures(url) {
                return format!("https://bitbucket.org/{}/{}/get/{reference}.{}", &c[1], &c[2], &c[4]);
            }
        }
        "gitlab.com" | "www.gitlab.com" => {
            if let Some(c) = GL.captures(url) {
                return format!("https://gitlab.com/api/v4/projects/{}/repository/archive.{}?sha={reference}", &c[1], &c[2]);
            }
        }
        h if github_domains.iter().any(|d| d == h) => {
            return GH_ENTERPRISE.replace(url, |c: &Captures| format!("{}/{reference}", &c[1])).into_owned();
        }
        h if gitlab_domains.iter().any(|d| d == h) => {
            return GL_ENTERPRISE.replace(url, |c: &Captures| format!("{}{reference}", &c[1])).into_owned();
        }
        _ => {}
    }
    url.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::type_complexity)]
    fn updates_dist_reference() {
        let gh = vec!["github.com".to_string()];
        let gl = vec!["gitlab.com".to_string()];
        let cases: [(&str, &str, Option<(&str, bool)>, &str); 17] = [
            ("https://github.com/foo/bar/zipball/abcd", "https://api.github.com/repos/foo/bar/zipball/newref", None, "newref"),
            ("https://www.github.com/foo/bar/zipball/abcd", "https://api.github.com/repos/foo/bar/zipball/newref", None, "newref"),
            ("https://github.com/foo/bar/archive/abcd.zip", "https://api.github.com/repos/foo/bar/zipball/newref", None, "newref"),
            ("https://github.com/foo/bar/archive/abcd.tar.gz", "https://api.github.com/repos/foo/bar/tarball/newref", None, "newref"),
            ("https://api.github.com/repos/foo/bar/tarball", "https://api.github.com/repos/foo/bar/tarball/newref", None, "newref"),
            ("https://api.github.com/repos/foo/bar/tarball/abcd", "https://api.github.com/repos/foo/bar/tarball/newref", None, "newref"),
            (
                "https://mygithub.com/api/v3/repos/foo/bar/tarball/abcd",
                "https://mygithub.com/api/v3/repos/foo/bar/tarball/newref",
                Some(("mygithub.com", true)),
                "newref",
            ),
            ("https://bitbucket.org/foo/bar/get/abcd.zip", "https://bitbucket.org/foo/bar/get/newref.zip", None, "newref"),
            ("https://www.bitbucket.org/foo/bar/get/abcd.tar.bz2", "https://bitbucket.org/foo/bar/get/newref.tar.bz2", None, "newref"),
            (
                "https://gitlab.com/api/v4/projects/foo%2Fbar/repository/archive.zip?sha=abcd",
                "https://gitlab.com/api/v4/projects/foo%2Fbar/repository/archive.zip?sha=newref",
                None,
                "newref",
            ),
            (
                "https://www.gitlab.com/api/v4/projects/foo%2Fbar/repository/archive.zip?sha=abcd",
                "https://gitlab.com/api/v4/projects/foo%2Fbar/repository/archive.zip?sha=newref",
                None,
                "newref",
            ),
            (
                "https://gitlab.com/api/v3/projects/foo%2Fbar/repository/archive.tar.gz?sha=abcd",
                "https://gitlab.com/api/v4/projects/foo%2Fbar/repository/archive.tar.gz?sha=newref",
                None,
                "newref",
            ),
            (
                "https://mygitlab.com/api/v4/projects/foo%2Fbar/repository/archive.tar.gz?sha=abcd",
                "https://mygitlab.com/api/v4/projects/foo%2Fbar/repository/archive.tar.gz?sha=newref",
                Some(("mygitlab.com", false)),
                "newref",
            ),
            (
                "https://mygitlab.com/api/v3/projects/foo%2Fbar/repository/archive.tar.bz2?sha=abcd",
                "https://mygitlab.com/api/v3/projects/foo%2Fbar/repository/archive.tar.bz2?sha=newref",
                Some(("mygitlab.com", false)),
                "newref",
            ),
            (
                "https://mygitlab.com/api/v3/projects/foo%2Fbar/repository/archive.tar.bz2?sha=abcd",
                "https://mygitlab.com/api/v3/projects/foo%2Fbar/repository/archive.tar.bz2?sha=65",
                Some(("mygitlab.com", false)),
                "65",
            ),
            ("https://example.org/a.zip", "https://example.org/a.zip", None, "newref"),
            ("https://mygithub.com/a.zip", "https://mygithub.com/a.zip", None, "newref"),
        ];
        for (url, expected, extra, reference) in cases {
            let (mut gh, mut gl) = (gh.clone(), gl.clone());
            match extra {
                Some((d, true)) => gh.push(d.into()),
                Some((d, false)) => gl.push(d.into()),
                None => {}
            }
            assert_eq!(update_dist_reference(&gh, &gl, url, reference), expected, "{url}");
        }
    }

    #[test]
    fn origins() {
        let cases = [
            ("gitlab.example.co", "https://gitlab.example.co/foo/bar/repository/archive.zip", "gitlab.example.com"),
            ("gitlab.example.co", "https://gitlab.example.co/foo/bar/repository/archive.zip", "gitlab.example.co.uk/gitlab"),
            ("gitlab.example.com", "https://gitlab.example.com/foo/bar/repository/archive.zip", "gitlab.example.com"),
            ("gitlab.example.com/gitlab", "https://gitlab.example.com/foo/bar/repository/archive.zip", "gitlab.example.com/gitlab"),
            ("gitlab.example.com:443", "https://gitlab.example.com/foo/bar/repository/archive.zip", "gitlab.example.com:443"),
            ("gitlab.example.com:443/gitlab", "https://gitlab.example.com/foo/bar/repository/archive.zip", "gitlab.example.com:443/gitlab"),
        ];
        for (expected, url, domain) in cases {
            assert_eq!(origin(&["gitlab.com".into(), domain.into()], url), expected);
        }
        assert_eq!(origin(&[], "https://api.github.com/repos/a/b"), "github.com");
        assert_eq!(origin(&[], "https://codeload.github.com/a/b"), "codeload.github.com");
        assert_eq!(origin(&[], "https://repo.packagist.org/p2/a/b.json"), "packagist.org");
        assert_eq!(origin(&[], "http://localhost:8080/x"), "localhost:8080");
    }

    #[test]
    fn allowed_redirects() {
        for (expected, url) in [
            (true, "http://example.org/foo"),
            (true, "https://example.org/foo"),
            (true, "HTTPS://example.org/foo"),
            (false, "file://localhost/etc/passwd"),
            (false, "file:///etc/passwd"),
            (false, "phar://archive.phar/file"),
            (false, "data://text/plain;base64,Zm9v"),
            (false, "ftp://example.org/foo"),
            (false, "/foo/bar"),
            (false, "example.org/foo"),
        ] {
            assert_eq!(is_allowed_redirect(url), expected, "{url}");
        }
    }

    #[test]
    fn sanitizes() {
        let cases = [
            ("", ""),
            ("https://foo:***@example.org/", "https://foo:bar@example.org/"),
            ("https://foo@example.org/", "https://foo@example.org/"),
            ("https://example.org/", "https://example.org/"),
            ("http://10a***:***@example.org", "http://10a8f08e8d7b7b9:foo@example.org"),
            ("https://foo:***@example.org:123/", "https://foo:bar@example.org:123/"),
            ("https://example.org/foo/bar?access_token=***", "https://example.org/foo/bar?access_token=abcdef"),
            ("https://example.org/foo/bar?foo=bar&access_token=***", "https://example.org/foo/bar?foo=bar&access_token=abcdef"),
            ("https://ghp***:***@github.com/acme/repo", "https://ghp_1234567890abcdefghijklmnopqrstuvwxyzAB:x-oauth-basic@github.com/acme/repo"),
            (
                "https://git***:***@github.com/acme/repo",
                "https://github_pat_1234567890abcdefghijkl_1234567890abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVW:x-oauth-basic@github.com/acme/repo",
            ),
            ("http://abc***:***@example.org:123/", "http://abcdefghijkl:bar@example.org:123/"),
            ("https://ghp***@github.com/acme/repo", "https://ghp_1234567890abcdefghijklmnopqrstuvwxyzAB@github.com/acme/repo"),
            ("http://10a***@example.org", "http://10a8f08e8d7b7b9@example.org"),
            ("https://x-token-auth:***@bitbucket.org/acme/repo", "https://x-token-auth:secret@bitbucket.org/acme/repo"),
            ("https://gitlab-ci-token:***@gitlab.example.org/", "https://gitlab-ci-token:realtoken@gitlab.example.org/"),
            ("foo:***@example.org/", "foo:bar@example.org/"),
            ("foo@example.org/", "foo@example.org/"),
            ("example.org/", "example.org/"),
            ("example.org/foo/bar?access_token=***", "example.org/foo/bar?access_token=abcdef"),
            ("abc***:***@example.org:123/", "abcdefghijklmnop:bar@example.org:123/"),
            (
                "Failed to execute git clone --mirror -- https://foo:***@example.org/private/repo.git /cache/repo",
                "Failed to execute git clone --mirror -- https://foo:bar@example.org/private/repo.git /cache/repo",
            ),
            (
                "tried https://foo:***@example.org/a and https://baz:***@example.com/b",
                "tried https://foo:bar@example.org/a and https://baz:qux@example.com/b",
            ),
            ("https://foo:***@example.org/repo.git", "https://foo:bar@baz@example.org/repo.git"),
            ("https://use***:***@example.org/repo.git", "https://user@corp.example:realtoken@example.org/repo.git"),
            ("https://:***@example.org/", "https://:realtoken@example.org/"),
            ("https://ghp***@github.com/acme/repo", "https://ghp_1234567890abcdefghijklmnopqrstuvwxyzAB:@github.com/acme/repo"),
            ("git+ssh://foo:***@example.org/repo.git", "git+ssh://foo:bar@example.org/repo.git"),
            (
                "fatal: unable to access 'https://example.org/private/repo.git/': Could not resolve host",
                "fatal: unable to access 'https://example.org/private/repo.git/': Could not resolve host",
            ),
            ("https://example.org/foo/bar@2x.png", "https://example.org/foo/bar@2x.png"),
        ];
        for (expected, url) in cases {
            assert_eq!(sanitize(url), expected, "{url}");
            assert_eq!(sanitize(&sanitize(url)), expected, "idempotent {url}");
        }
    }

    #[test]
    fn strips_credentials() {
        for (expected, url) in [
            ("https://example.org/repo.git", "https://user:pass@example.org/repo.git"),
            ("https://github.com/acme/repo.git", "https://ghp_1234567890abcdefghijklmnopqrstuvwxyzAB@github.com/acme/repo.git"),
            ("https://example.org:8080/repo.git", "https://user:pass@example.org:8080/repo.git"),
            ("https://example.org/repo.git", "https://example.org/repo.git"),
            ("https://example.org/@scope/repo.git", "https://example.org/@scope/repo.git"),
            ("git@example.org:acme/repo.git", "git@example.org:acme/repo.git"),
            // Composer 2.10 also drops ssh login names
            ("ssh://example.org/repo.git", "ssh://git@example.org/repo.git"),
        ] {
            assert_eq!(strip_credentials(url), expected, "{url}");
        }
    }
}
