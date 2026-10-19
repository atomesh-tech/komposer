//! `Util\Filesystem` and the `Util\Platform` bits the installers need. Paths are
//! Composer-style strings (forward slashes); PHP's `dirname()`/`basename()` semantics
//! are reproduced because the shortest-path algorithms depend on them.

use anyhow::{Context, Result, bail};
use regex::Regex;
use std::fs;
use std::io::ErrorKind;
use std::path::Path;
use std::sync::LazyLock;

/// PHP `dirname()` (unix build).
pub fn dirname(path: &str) -> String {
    let b = path.as_bytes();
    if b.is_empty() {
        return String::new();
    }
    let mut end = b.len();
    while end > 0 && b[end - 1] == b'/' {
        end -= 1;
    }
    if end == 0 {
        return "/".into();
    }
    while end > 0 && b[end - 1] != b'/' {
        end -= 1;
    }
    if end == 0 {
        return ".".into();
    }
    while end > 0 && b[end - 1] == b'/' {
        end -= 1;
    }
    if end == 0 {
        return "/".into();
    }
    path[..end].to_string()
}

/// PHP `basename()`.
pub fn basename(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    trimmed.rsplit('/').next().unwrap_or("")
}

/// PHP `var_export()` of a string.
pub fn var_export(s: &str) -> String {
    let escaped = s.replace('\\', "\\\\").replace('\'', "\\'");
    format!("'{}'", escaped.replace('\0', "' . \"\\0\" . '"))
}

/// `Filesystem::isAbsolutePath()`.
pub fn is_absolute_path(path: &str) -> bool {
    path.starts_with('/') || path.as_bytes().get(1) == Some(&b':') || path.starts_with("\\\\")
}

/// `Filesystem::normalizePath()`: slashes, `.`/`..` and duplicate separators.
pub fn normalize_path(path: &str) -> String {
    static PREFIX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^([0-9a-z]{2,}:(?://(?:[a-z]:)?)?|[a-z]:)").unwrap());
    static DRIVE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)(^|://)[a-z]:$").unwrap());
    let mut path = path.replace('\\', "/");
    let mut absolute = "";
    // windows UNC paths, e.g. \\foo\bar
    if path.starts_with("//") && path.len() > 2 {
        absolute = "//";
        path = path[2..].to_string();
    }
    let mut prefix = String::new();
    if let Some(m) = PREFIX.find(&path) {
        prefix = m.as_str().to_string();
        path = path[prefix.len()..].to_string();
    }
    if let Some(rest) = path.strip_prefix('/') {
        absolute = "/";
        path = rest.to_string();
    }
    let mut parts: Vec<&str> = vec![];
    let mut up = false;
    for chunk in path.split('/') {
        if chunk == ".." && (!absolute.is_empty() || up) {
            parts.pop();
            up = !(parts.is_empty() || parts.last() == Some(&".."));
        } else if chunk != "." && !chunk.is_empty() {
            parts.push(chunk);
            up = chunk != "..";
        }
    }
    let prefix = DRIVE.replace(&prefix, |c: &regex::Captures| c[0].to_uppercase());
    format!("{prefix}{absolute}{}", parts.join("/"))
}

static DRIVE_ROOT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^[A-Z]:/?$").unwrap());

fn ensure_absolute(from: &str, to: &str) -> Result<()> {
    if !is_absolute_path(from) || !is_absolute_path(to) {
        bail!("$from ({from}) and $to ({to}) must be absolute paths.");
    }
    Ok(())
}

/// `Filesystem::findShortestPath()`.
pub fn find_shortest_path(from: &str, to: &str, directories: bool, prefer_relative: bool) -> Result<String> {
    ensure_absolute(from, to)?;
    let mut from = normalize_path(from);
    let to = normalize_path(to);
    if directories {
        from = format!("{}/dummy_file", from.trim_end_matches('/'));
    }
    if dirname(&from) == dirname(&to) {
        return Ok(format!("./{}", basename(&to)));
    }
    let mut common = to.clone();
    while !format!("{from}/").starts_with(&format!("{common}/")) && common != "/" && !DRIVE_ROOT.is_match(&common) {
        common = dirname(&common).replace('\\', "/");
    }
    // no commonality at all
    if !from.starts_with(&common) {
        return Ok(to);
    }
    let common = format!("{}/", common.trim_end_matches('/'));
    let depth = from.get(common.len()..).unwrap_or("").matches('/').count();
    // allow top level /foo & /bar dirs to be addressed relatively as this is common in Docker setups
    if !prefer_relative && common == "/" && depth > 1 {
        return Ok(to);
    }
    let result = "../".repeat(depth) + to.get(common.len()..).unwrap_or("");
    Ok(if result.is_empty() { "./".into() } else { result })
}

/// `Filesystem::findShortestPathCode()`: PHP code evaluating to `$to` from a file at `$from`.
pub fn find_shortest_path_code(from: &str, to: &str, directories: bool, static_code: bool, prefer_relative: bool) -> Result<String> {
    ensure_absolute(from, to)?;
    let from = normalize_path(from);
    let to = normalize_path(to);
    if from == to {
        return Ok(if directories { "__DIR__" } else { "__FILE__" }.into());
    }
    let mut common = to.clone();
    while !format!("{from}/").starts_with(&format!("{common}/")) && common != "/" && !DRIVE_ROOT.is_match(&common) && common != "." {
        common = dirname(&common).replace('\\', "/");
    }
    if !from.starts_with(&common) || common == "." {
        return Ok(var_export(&to));
    }
    let common = format!("{}/", common.trim_end_matches('/'));
    if let Some(rest) = to.strip_prefix(&format!("{from}/")) {
        return Ok(format!("__DIR__ . {}", var_export(&format!("/{rest}"))));
    }
    let depth = from.get(common.len()..).unwrap_or("").matches('/').count() + usize::from(directories);
    if !prefer_relative && common == "/" && depth > 1 {
        return Ok(var_export(&to));
    }
    let base = if static_code {
        format!("__DIR__ . '{}'", "/..".repeat(depth))
    } else {
        format!("{}__DIR__{}", "dirname(".repeat(depth), ")".repeat(depth))
    };
    let rel = to.get(common.len()..).unwrap_or("");
    Ok(if rel.is_empty() { base } else { format!("{base}.{}", var_export(&format!("/{rel}"))) })
}

/// `Filesystem::trimTrailingSlash()`.
pub fn trim_trailing_slash(path: &str) -> &str {
    if path.bytes().all(|b| b == b'/' || b == b'\\') {
        return path;
    }
    path.trim_end_matches(['/', '\\'])
}

/// `Filesystem::isLocalPath()` (non-Windows).
pub fn is_local_path(path: &str) -> bool {
    static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^(file://|/|/?[a-z]:[\\/]|\.\.[\\/]|[a-z0-9_.-]+[\\/])").unwrap());
    RE.is_match(path)
}

/// PHP `realpath()`.
pub fn realpath(path: &str) -> Option<String> {
    fs::canonicalize(if path.is_empty() { "." } else { path }).ok().map(|p| p.to_string_lossy().into_owned())
}

/// `Platform::getCwd()`.
pub fn cwd() -> Result<String> {
    let cwd = std::env::current_dir().context("Could not determine the current working directory")?;
    Ok(cwd.to_string_lossy().into_owned())
}

pub(crate) fn is_link(path: &str) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink())
}

/// `Filesystem::isReadable()`.
pub fn is_readable(path: &str) -> bool {
    match fs::metadata(path) {
        Ok(m) if m.is_dir() => fs::read_dir(path).is_ok(),
        Ok(_) => fs::File::open(path).is_ok(),
        Err(_) => false,
    }
}

/// `Filesystem::ensureDirectoryExists()`.
pub fn ensure_directory_exists(dir: &str) -> Result<()> {
    if Path::new(dir).is_dir() {
        return Ok(());
    }
    if Path::new(dir).exists() {
        bail!("{dir} exists and is not a directory.");
    }
    if is_link(dir) {
        fs::remove_file(dir).with_context(|| format!("Could not delete symbolic link {dir}"))?;
    }
    match fs::create_dir_all(dir) {
        Ok(()) => Ok(()),
        Err(_) if Path::new(dir).is_dir() => Ok(()),
        // paths like path/to/broken-symlink/../foo only work normalized
        Err(_) if normalize_path(dir) != dir && ensure_directory_exists(&normalize_path(dir)).is_ok() => Ok(()),
        Err(e) => Err(e).with_context(|| format!("{dir} does not exist and could not be created")),
    }
}

/// `Filesystem::isDirEmpty()` (dotfiles count).
pub fn is_dir_empty(dir: &str) -> bool {
    fs::read_dir(dir).is_ok_and(|mut d| d.next().is_none())
}

/// `Filesystem::emptyDirectory()`.
pub fn empty_directory(dir: &str, ensure_exists: bool) -> Result<()> {
    if is_link(dir) && Path::new(dir).exists() {
        unlink(dir)?;
    }
    if ensure_exists {
        ensure_directory_exists(dir)?;
    }
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            remove(&entry.path().to_string_lossy())?;
        }
    }
    Ok(())
}

/// `Filesystem::remove()`.
pub fn remove(path: &str) -> Result<bool> {
    if Path::new(path).is_dir() {
        return remove_directory(path);
    }
    if Path::new(path).exists() || is_link(path) {
        unlink(path)?;
        return Ok(true);
    }
    Ok(false)
}

/// `Filesystem::unlink()`.
pub fn unlink(path: &str) -> Result<()> {
    fs::remove_file(path).with_context(|| format!("Could not delete {path}"))
}

/// `Filesystem::removeDirectory()`: symlinks to directories are unlinked, never followed.
pub fn remove_directory(dir: &str) -> Result<bool> {
    let trimmed = dir.trim_end_matches('/');
    if is_link(trimmed) {
        unlink(trimmed)?;
        return Ok(true);
    }
    if !Path::new(dir).is_dir() {
        return Ok(true);
    }
    static ROOT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^(?:[a-z]:)?[/\\]+$").unwrap());
    if ROOT.is_match(dir) {
        bail!("Aborting an attempted deletion of {dir}, this was probably not intended, if it is a real use case please report it.");
    }
    if fs::remove_dir_all(dir).is_err() {
        // read-only directories (e.g. 0555 from an archive) block removing their contents
        make_writable(Path::new(dir));
        fs::remove_dir_all(dir).with_context(|| format!("Could not delete {dir}"))?;
    }
    Ok(true)
}

fn make_writable(dir: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
    }
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            make_writable(&entry.path());
        }
    }
}

/// `Filesystem::copy()`: a file, or a directory recursively (symlinks are followed).
pub fn copy(source: &str, target: &str) -> Result<()> {
    let target = normalize_path(target);
    if !Path::new(source).is_dir() {
        fs::copy(source, &target).with_context(|| format!("Could not copy {source} to {target}"))?;
        return Ok(());
    }
    ensure_directory_exists(&target)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        copy(&format!("{source}/{name}"), &format!("{target}/{name}"))?;
    }
    Ok(())
}

/// `Filesystem::copyThenRemove()`.
pub fn copy_then_remove(source: &str, target: &str) -> Result<()> {
    copy(source, target)?;
    if Path::new(source).is_dir() { remove_directory(source).map(drop) } else { unlink(source) }
}

/// `Filesystem::rename()`: falls back to copying across devices.
pub fn rename(source: &str, target: &str) -> Result<()> {
    match fs::rename(source, target) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == ErrorKind::NotFound => Err(e).with_context(|| format!("Could not rename {source} to {target}")),
        Err(_) => copy_then_remove(source, target),
    }
}

/// `Filesystem::relativeSymlink()`.
pub fn relative_symlink(target: &str, link: &str) -> Result<()> {
    let relative = find_shortest_path(link, target, false, false)?;
    symlink(&relative, link)
}

pub(crate) fn symlink(target: &str, link: &str) -> Result<()> {
    #[cfg(unix)]
    let res = std::os::unix::fs::symlink(target, link);
    #[cfg(windows)]
    let res = std::os::windows::fs::symlink_dir(target, link);
    res.with_context(|| format!("Could not create symlink {link} to {target}"))
}

/// `Filesystem::filePutContentsIfModified()`.
pub fn file_put_contents_if_modified(path: &str, content: &str) -> Result<()> {
    if fs::read(path).is_ok_and(|old| old == content.as_bytes()) {
        return Ok(());
    }
    fs::write(path, content).with_context(|| format!("Could not write {path}"))
}

/// The process umask (Composer chmods bins to `0777 & ~umask()`).
#[cfg(unix)]
pub fn umask() -> u32 {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    use std::sync::OnceLock;
    static UMASK: OnceLock<u32> = OnceLock::new();
    // the kernel applies the umask to the mode of a new file, which avoids the racy umask(2) dance
    *UMASK.get_or_init(|| {
        let probe = std::env::temp_dir().join(format!(".komposer-umask-{}", std::process::id()));
        let _ = fs::remove_file(&probe);
        let created = fs::OpenOptions::new().write(true).create_new(true).mode(0o777).open(&probe);
        let mode = created.and_then(|f| f.metadata()).map(|m| m.permissions().mode() & 0o777);
        let _ = fs::remove_file(&probe);
        mode.map(|m| 0o777 & !m).unwrap_or(0o022)
    })
}

/// `chmod($path, 0777 & ~umask())`, ignoring failures like Composer's `Silencer`.
pub(crate) fn chmod_executable(path: &str) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o777 & !umask()));
    }
}

/// `Platform::isWindowsSubsystemForLinux()`.
pub fn is_wsl() -> bool {
    static WSL: LazyLock<bool> = LazyLock::new(|| {
        cfg!(target_os = "linux") && fs::read_to_string("/proc/version").is_ok_and(|v| v.to_lowercase().contains("microsoft")) && !is_docker()
    });
    *WSL
}

/// `Platform::isDocker()`.
fn is_docker() -> bool {
    if ["/.dockerenv", "/run/.containerenv", "/var/run/.containerenv"].iter().any(|f| Path::new(f).exists()) {
        return true;
    }
    ["/proc/self/mountinfo", "/proc/1/cgroup"]
        .iter()
        .any(|f| fs::read_to_string(f).is_ok_and(|d| d.contains("/var/lib/docker/") || d.contains("/io.containerd.snapshotter")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortest_path_code() {
        let cases: &[(&str, &str, bool, &str, bool, bool)] = &[
            ("/foo/bar", "/foo/bar", false, "__FILE__", false, false),
            ("/foo/bar", "/foo/baz", false, "__DIR__.'/baz'", false, false),
            ("/foo/bin/run", "/foo/vendor/acme/bin/run", false, "dirname(__DIR__).'/vendor/acme/bin/run'", false, false),
            ("/foo/bin/run", "/bar/bin/run", false, "'/bar/bin/run'", false, false),
            ("c:/bin/run", "c:/vendor/acme/bin/run", false, "dirname(__DIR__).'/vendor/acme/bin/run'", false, false),
            ("c:\\bin\\run", "c:/vendor/acme/bin/run", false, "dirname(__DIR__).'/vendor/acme/bin/run'", false, false),
            ("c:/bin/run", "D:/vendor/acme/bin/run", false, "'D:/vendor/acme/bin/run'", false, false),
            ("c:\\bin\\run", "d:/vendor/acme/bin/run", false, "'D:/vendor/acme/bin/run'", false, false),
            ("/foo/bar", "/foo/bar", true, "__DIR__", false, false),
            ("/foo/bar/", "/foo/bar", true, "__DIR__", false, false),
            ("/foo", "/baz", true, "dirname(__DIR__).'/baz'", false, false),
            ("/foo/bar", "/foo/baz", true, "dirname(__DIR__).'/baz'", false, false),
            ("/foo/bin/run", "/foo/vendor/acme/bin/run", true, "dirname(dirname(__DIR__)).'/vendor/acme/bin/run'", false, false),
            ("/foo/bin/run", "/bar/bin/run", true, "'/bar/bin/run'", false, false),
            ("/app/vendor/foo/bar", "/lib", true, "dirname(dirname(dirname(dirname(__DIR__)))).'/lib'", false, true),
            ("/bin/run", "/bin/run", true, "__DIR__", false, false),
            ("c:/bin/run", "C:\\bin/run", true, "__DIR__", false, false),
            ("c:/bin/run", "c:/vendor/acme/bin/run", true, "dirname(dirname(__DIR__)).'/vendor/acme/bin/run'", false, false),
            ("c:\\bin\\run", "c:/vendor/acme/bin/run", true, "dirname(dirname(__DIR__)).'/vendor/acme/bin/run'", false, false),
            ("c:/bin/run", "d:/vendor/acme/bin/run", true, "'D:/vendor/acme/bin/run'", false, false),
            ("c:\\bin\\run", "d:/vendor/acme/bin/run", true, "'D:/vendor/acme/bin/run'", false, false),
            ("C:/Temp/test", "C:\\Temp", true, "dirname(__DIR__)", false, false),
            ("C:/Temp", "C:\\Temp\\test", true, "__DIR__ . '/test'", false, false),
            ("/tmp/test", "/tmp", true, "dirname(__DIR__)", false, false),
            ("/tmp", "/tmp/test", true, "__DIR__ . '/test'", false, false),
            ("C:/Temp", "c:\\Temp\\test", true, "__DIR__ . '/test'", false, false),
            ("/tmp/test/./", "/tmp/test/", true, "__DIR__", false, false),
            ("/tmp/test/../vendor", "/tmp/test", true, "dirname(__DIR__).'/test'", false, false),
            ("/tmp/test/.././vendor", "/tmp/test", true, "dirname(__DIR__).'/test'", false, false),
            ("C:/Temp", "c:\\Temp\\..\\..\\test", true, "dirname(__DIR__).'/test'", false, false),
            ("C:/Temp/../..", "d:\\Temp\\..\\..\\test", true, "'D:/test'", false, false),
            ("/foo/bar", "/foo/bar_vendor", true, "dirname(__DIR__).'/bar_vendor'", false, false),
            ("/foo/bar_vendor", "/foo/bar", true, "dirname(__DIR__).'/bar'", false, false),
            ("/foo/bar_vendor", "/foo/bar/src", true, "dirname(__DIR__).'/bar/src'", false, false),
            ("/foo/bar_vendor/src2", "/foo/bar/src/lib", true, "dirname(dirname(__DIR__)).'/bar/src/lib'", false, false),
            ("/tmp/test/../vendor", "/tmp/test", true, "__DIR__ . '/..'.'/test'", true, false),
            ("/tmp/test/.././vendor", "/tmp/test", true, "__DIR__ . '/..'.'/test'", true, false),
            ("C:/Temp", "c:\\Temp\\..\\..\\test", true, "__DIR__ . '/..'.'/test'", true, false),
            ("C:/Temp/../..", "d:\\Temp\\..\\..\\test", true, "'D:/test'", true, false),
            ("/foo/bar", "/foo/bar_vendor", true, "__DIR__ . '/..'.'/bar_vendor'", true, false),
            ("/foo/bar_vendor", "/foo/bar", true, "__DIR__ . '/..'.'/bar'", true, false),
            ("/foo/bar_vendor", "/foo/bar/src", true, "__DIR__ . '/..'.'/bar/src'", true, false),
            ("/foo/bar_vendor/src2", "/foo/bar/src/lib", true, "__DIR__ . '/../..'.'/bar/src/lib'", true, false),
        ];
        for &(a, b, dirs, expected, stat, rel) in cases {
            assert_eq!(find_shortest_path_code(a, b, dirs, stat, rel).unwrap(), expected, "{a} -> {b}");
        }
    }

    #[test]
    fn shortest_path() {
        let cases: &[(&str, &str, &str, bool, bool)] = &[
            ("/foo/bar", "/foo/bar", "./bar", false, false),
            ("/foo/bar", "/foo/baz", "./baz", false, false),
            ("/foo/bar/", "/foo/baz", "./baz", false, false),
            ("/foo/bar", "/foo/bar", "./", true, false),
            ("/foo/bar", "/foo/baz", "../baz", true, false),
            ("/foo/bar/", "/foo/baz", "../baz", true, false),
            ("C:/foo/bar/", "c:/foo/baz", "../baz", true, false),
            ("/foo/bin/run", "/foo/vendor/acme/bin/run", "../vendor/acme/bin/run", false, false),
            ("/foo/bin/run", "/bar/bin/run", "/bar/bin/run", false, false),
            ("/foo/bin/run", "/bar/bin/run", "/bar/bin/run", true, false),
            ("c:/foo/bin/run", "d:/bar/bin/run", "D:/bar/bin/run", true, false),
            ("c:/bin/run", "c:/vendor/acme/bin/run", "../vendor/acme/bin/run", false, false),
            ("c:\\bin\\run", "c:/vendor/acme/bin/run", "../vendor/acme/bin/run", false, false),
            ("c:/bin/run", "d:/vendor/acme/bin/run", "D:/vendor/acme/bin/run", false, false),
            ("c:\\bin\\run", "d:/vendor/acme/bin/run", "D:/vendor/acme/bin/run", false, false),
            ("C:/Temp/test", "C:\\Temp", "./", false, false),
            ("/tmp/test", "/tmp", "./", false, false),
            ("C:/Temp/test/sub", "C:\\Temp", "../", false, false),
            ("/tmp/test/sub", "/tmp", "../", false, false),
            ("/tmp/test/sub", "/tmp", "../../", true, false),
            ("c:/tmp/test/sub", "c:/tmp", "../../", true, false),
            ("/tmp", "/tmp/test", "test", false, false),
            ("C:/Temp", "C:\\Temp\\test", "test", false, false),
            ("C:/Temp", "c:\\Temp\\test", "test", false, false),
            ("/tmp/test/./", "/tmp/test", "./", true, false),
            ("/tmp/test/../vendor", "/tmp/test", "../test", true, false),
            ("/tmp/test/.././vendor", "/tmp/test", "../test", true, false),
            ("C:/Temp", "c:\\Temp\\..\\..\\test", "../test", true, false),
            ("C:/Temp/../..", "c:\\Temp\\..\\..\\test", "./test", true, false),
            ("C:/Temp/../..", "D:\\Temp\\..\\..\\test", "D:/test", true, false),
            ("/app/vendor/foo/bar", "/lib", "../../../../lib", true, true),
            ("/tmp", "/tmp/../../test", "../test", true, false),
            ("/tmp", "/test", "../test", true, false),
            ("/foo/bar", "/foo/bar_vendor", "../bar_vendor", true, false),
            ("/foo/bar_vendor", "/foo/bar", "../bar", true, false),
            ("/foo/bar_vendor", "/foo/bar/src", "../bar/src", true, false),
            ("/foo/bar_vendor/src2", "/foo/bar/src/lib", "../../bar/src/lib", true, false),
            ("C:/", "C:/foo/bar/", "foo/bar", true, false),
        ];
        for &(a, b, expected, dirs, rel) in cases {
            assert_eq!(find_shortest_path(a, b, dirs, rel).unwrap(), expected, "{a} -> {b}");
        }
        assert!(find_shortest_path("foo", "/bar", false, false).is_err());
    }

    #[test]
    fn normalizes_paths() {
        let cases = [
            ("../foo", "../foo"),
            ("C:/foo/bar", "c:/foo//bar"),
            ("C:/foo/bar", "C:/foo/./bar"),
            ("C:/foo/bar", "C://foo//bar"),
            ("C:/foo/bar", "C:///foo//bar"),
            ("C:/bar", "C:/foo/../bar"),
            ("/bar", "/foo/../bar/"),
            ("phar://C:/Foo", "phar://c:/Foo/Bar/.."),
            ("phar://C:/Foo", "phar://c:///Foo/Bar/.."),
            ("phar://C:/", "phar://c:/Foo/Bar/../../../.."),
            ("/", "/Foo/Bar/../../../.."),
            ("/", "/"),
            ("/", "//"),
            ("/", "///"),
            ("/Foo", "///Foo"),
            ("C:/", "c:\\"),
            ("../src", "Foo/Bar/../../../src"),
            ("C:../b", "c:.\\..\\a\\..\\b"),
            ("phar://C:../Foo", "phar://c:../Foo"),
            ("//foo/bar", "\\\\foo\\bar"),
        ];
        for (expected, path) in cases {
            assert_eq!(normalize_path(path), expected, "{path}");
        }
    }

    #[test]
    fn php_string_helpers() {
        assert_eq!(dirname("/foo/bar/"), "/foo");
        assert_eq!(dirname("foo"), ".");
        assert_eq!(dirname("/foo"), "/");
        assert_eq!(basename("/foo/bar/"), "bar");
        assert_eq!(var_export("it's a \\ \0"), "'it\\'s a \\\\ ' . \"\\0\" . ''");
        assert!(is_local_path("../foo") && is_local_path("file:///x") && !is_local_path("https://github.com/a/b"));
    }

    #[test]
    fn removes_read_only_trees_and_unlinks_symlinked_dirs() {
        let dir = crate::tests::tmp_dir("fs-remove");
        let target = format!("{dir}/target");
        fs::create_dir_all(format!("{target}/ro/sub")).unwrap();
        fs::write(format!("{target}/ro/sub/f"), "x").unwrap();
        symlink(&target, &format!("{dir}/link")).unwrap();
        assert!(remove_directory(&format!("{dir}/link/")).unwrap());
        assert!(Path::new(&format!("{target}/ro/sub/f")).exists(), "symlink target must survive");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(format!("{target}/ro"), fs::Permissions::from_mode(0o555)).unwrap();
        }
        assert!(remove_directory(&target).unwrap());
        assert!(!Path::new(&target).exists());
    }

    #[test]
    fn copy_then_remove_moves_trees() {
        let dir = crate::tests::tmp_dir("fs-copy");
        fs::create_dir_all(format!("{dir}/a/b")).unwrap();
        fs::write(format!("{dir}/a/b/f"), "x").unwrap();
        copy_then_remove(&format!("{dir}/a"), &format!("{dir}/c")).unwrap();
        assert_eq!(fs::read_to_string(format!("{dir}/c/b/f")).unwrap(), "x");
        assert!(!Path::new(&format!("{dir}/a")).exists());
    }
}
