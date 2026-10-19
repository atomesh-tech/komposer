//! `Installer\BinaryInstaller`: `vendor/bin` proxies for package binaries.

use crate::Out;
use crate::fs::{self, basename, dirname, find_shortest_path, find_shortest_path_code, normalize_path, realpath};
use anyhow::Result;
use komposer_package::Package;
use regex::Regex;
use std::io::{BufRead, Read};
use std::path::Path;
use std::sync::LazyLock;

pub(crate) struct BinaryInstaller {
    pub bin_dir: String,
    /// `auto`, `proxy` or `full`
    pub bin_compat: String,
    /// The configured vendor dir.
    pub vendor_dir: String,
}

impl BinaryInstaller {
    /// `installBinaries()`.
    pub fn install(&self, out: &Out, package: &Package, install_path: &str, warn_on_overwrite: bool) -> Result<()> {
        let mut bin_dir = None;
        for bin in &package.bin {
            let skipped =
                |why: &str| out.write(&format!("    <warning>Skipped installation of bin {bin} for package {}: {why}</warning>", package.name));
            let mut bin_path = format!("{install_path}/{bin}");
            if !Path::new(&bin_path).exists() {
                skipped("file not found in package");
                continue;
            }
            if Path::new(&bin_path).is_dir() {
                skipped("found a directory at that path");
                continue;
            }
            // a bin symlinked (or `..`-pathed) out of the package would let it chmod/proxy any host file
            if !is_bin_path_inside_package(install_path, &bin_path) {
                skipped("the bin resolves to a path outside of the package directory");
                continue;
            }
            if !fs::is_absolute_path(&bin_path) {
                bin_path = realpath(&bin_path).unwrap_or(bin_path);
            }
            let dir = match &bin_dir {
                Some(d) => d,
                None => bin_dir.insert(self.initialize_bin_dir()?),
            };
            let link = format!("{dir}/{}", basename(bin));
            if Path::new(&link).exists() {
                if !fs::is_link(&link) {
                    if warn_on_overwrite {
                        out.write(&format!(
                            "    Skipped installation of bin {bin} for package {}: name conflicts with an existing file",
                            package.name
                        ));
                    }
                    continue;
                }
                if realpath(&link) == realpath(&bin_path) {
                    // a symlinked binary from a previous installation, replaced by a proxy
                    fs::unlink(&link)?;
                }
            }
            let full = self.bin_compat == "full" || (self.bin_compat == "auto" && (cfg!(windows) || fs::is_wsl()));
            if full {
                self.install_full(out, &bin_path, &link, bin, package)?;
            } else {
                self.write_proxy(&link, &self.unixy_proxy(&bin_path, &link)?)?;
            }
            fs::chmod_executable(&bin_path);
        }
        Ok(())
    }

    /// `removeBinaries()`. Like Composer, this creates the bin dir even when there is nothing to remove.
    pub fn remove(&self, package: &Package) -> Result<()> {
        let bin_dir = self.initialize_bin_dir()?;
        if package.bin.is_empty() {
            return Ok(());
        }
        for bin in &package.bin {
            let link = format!("{bin_dir}/{}", basename(bin));
            if fs::is_link(&link) || Path::new(&link).exists() {
                fs::unlink(&link)?;
            }
            if Path::new(&format!("{link}.bat")).is_file() {
                fs::unlink(&format!("{link}.bat"))?;
            }
        }
        if fs::is_dir_empty(&bin_dir) {
            let _ = std::fs::remove_dir(&bin_dir);
        }
        Ok(())
    }

    fn initialize_bin_dir(&self) -> Result<String> {
        fs::ensure_directory_exists(&self.bin_dir)?;
        Ok(realpath(&self.bin_dir).unwrap_or_else(|| self.bin_dir.clone()))
    }

    fn install_full(&self, out: &Out, bin_path: &str, link: &str, bin: &str, package: &Package) -> Result<()> {
        let mut link = link.to_string();
        // unixy support for cygwin and similar environments
        if !bin_path.ends_with(".bat") {
            self.write_proxy(&link, &self.unixy_proxy(bin_path, &link)?)?;
            link.push_str(".bat");
            if Path::new(&link).exists() {
                out.write(&format!(
                    "    Skipped installation of bin {bin}.bat proxy for package {}: a .bat proxy was already installed",
                    package.name
                ));
            }
        }
        if !Path::new(&link).exists() {
            self.write_proxy(&link, &windows_proxy(bin_path, &link)?)?;
        }
        Ok(())
    }

    fn write_proxy(&self, link: &str, code: &str) -> Result<()> {
        std::fs::write(link, code)?;
        fs::chmod_executable(link);
        Ok(())
    }

    /// `generateUnixyProxyCode()`: a PHP proxy for PHP binaries, a shell proxy otherwise.
    pub fn unixy_proxy(&self, bin: &str, link: &str) -> Result<String> {
        static PHP: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(#!.*\r?\n)?[\r\n\t ]*<\?php").unwrap());
        let bin_path = find_shortest_path(link, bin, false, false)?;
        let bin_dir = shell_escape(&dirname(&bin_path));
        let bin_file = basename(&bin_path);

        let mut head = Vec::with_capacity(500);
        std::fs::File::open(bin)?.take(500).read_to_end(&mut head)?;
        let head = String::from_utf8_lossy(&head);
        let Some(m) = PHP.captures(&head) else {
            return Ok(SH_PROXY.replace("{binDir}", &bin_dir).replace("{binFile}", bin_file));
        };

        // carry over the existing shebang if present, otherwise add our own
        let shebang = m.get(1).map_or("#!/usr/bin/env php", |s| s.as_str().trim());
        let exported = find_shortest_path_code(link, bin, false, true, false)?;
        // compare realpaths, as $link is realpath'd already
        let vendor_real = realpath(&self.vendor_dir).unwrap_or_else(|| self.vendor_dir.clone());
        let autoload = find_shortest_path_code(link, &format!("{vendor_real}/autoload.php"), false, true, false)?;
        let mut globals = format!("$GLOBALS['_composer_bin_dir'] = __DIR__;\n$GLOBALS['_composer_autoload_path'] = {autoload};\n");
        // PHPUnit process isolation workaround (the configured vendor dir is used as is, like Composer)
        let phpunit = normalize_path(bin) == normalize_path(&format!("{}/phpunit/phpunit/phpunit", self.vendor_dir));
        if phpunit {
            globals.push_str(&format!(
                "$GLOBALS['__PHPUNIT_ISOLATION_EXCLUDE_LIST'] = $GLOBALS['__PHPUNIT_ISOLATION_BLACKLIST'] = array(realpath({exported}));\n"
            ));
        }
        let (stream_hint, stream_proxy) = if m[0].trim() != "<?php" {
            let (hack1, hack2) = if phpunit { ("'phpvfscomposer://'.", PHPUNIT_STREAM_READ) } else { ("", "") };
            let code = STREAM_PROXY.replace("{hack1}", hack1).replace("{hack2}", hack2).replace("{binPathExported}", &exported);
            (" using a stream wrapper to prevent the shebang from being output on PHP<8\n *", code)
        } else {
            ("", String::new())
        };
        Ok(format!(
            "{shebang}\n<?php\n\n/**\n * Proxy PHP file generated by Composer\n *\n * This file includes the referenced bin path ({bin_path})\n *{stream_hint}\n * @generated\n */\n\nnamespace Composer;\n\n{globals}\n{stream_proxy}\nreturn include {exported};\n"
        ))
    }
}

/// `generateWindowsProxyCode()`.
fn windows_proxy(bin: &str, link: &str) -> Result<String> {
    let bin_path = find_shortest_path(link, bin, false, false)?;
    let caller = determine_binary_caller(bin);
    // php targets run through the unixy proxy so _composer_autoload_path gets defined
    let target = if caller == "php" {
        let name = basename(link);
        shell_escape(name.strip_suffix(".bat").unwrap_or(name))
    } else {
        shell_escape(&bin_path)
    };
    Ok(format!(
        "@ECHO OFF\r\nsetlocal DISABLEDELAYEDEXPANSION\r\nSET BIN_TARGET=%~dp0/{}\r\nSET COMPOSER_RUNTIME_BIN_DIR=%~dp0\r\n{caller} \"%BIN_TARGET%\" %*\r\n",
        target.trim_matches(['"', '\''])
    ))
}

/// `determineBinaryCaller()`.
pub(crate) fn determine_binary_caller(bin: &str) -> String {
    static SHEBANG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^#!/(?:usr/bin/env )?(?:[^/]+/)*(.+)$").unwrap());
    if bin.ends_with(".bat") || bin.ends_with(".exe") {
        return "call".into();
    }
    let mut line = String::new();
    if let Ok(f) = std::fs::File::open(bin) {
        let _ = std::io::BufReader::new(f).read_line(&mut line);
    }
    match SHEBANG.captures(&line) {
        Some(c) => c[1].trim().to_string(),
        None => "php".into(),
    }
}

/// `isBinPathInsidePackage()`.
pub(crate) fn is_bin_path_inside_package(install_path: &str, bin_path: &str) -> bool {
    match (realpath(bin_path), realpath(install_path)) {
        (Some(bin), Some(dir)) => bin.starts_with(&format!("{dir}/")),
        _ => false,
    }
}

/// `ProcessExecutor::escape()` on unix.
pub(crate) fn shell_escape(arg: &str) -> String {
    format!("'{}'", arg.replace('\'', "'\\''"))
}

const SH_PROXY: &str = r#"#!/usr/bin/env sh

# Support bash to support `source` with fallback on $0 if this does not run with bash
# https://stackoverflow.com/a/35006505/6512
selfArg="$BASH_SOURCE"
if [ -z "$selfArg" ]; then
    selfArg="$0"
fi

self=$(realpath "$selfArg" 2> /dev/null)
if [ -z "$self" ]; then
    self="$selfArg"
fi

dir=$(cd "${self%[/\\]*}" > /dev/null; cd {binDir} && pwd)

if [ -d /proc/cygdrive ]; then
    case $(which php) in
        $(readlink -n /proc/cygdrive)/*)
            # We are in Cygwin using Windows php, so the path must be translated
            dir=$(cygpath -m "$dir");
            ;;
    esac
fi

export COMPOSER_RUNTIME_BIN_DIR="$(cd "${self%[/\\]*}" > /dev/null; pwd)"

# If bash is sourcing this file, we have to source the target as well
bashSource="$BASH_SOURCE"
if [ -n "$bashSource" ]; then
    if [ "$bashSource" != "$0" ]; then
        source "${dir}/{binFile}" "$@"
        return
    fi
fi

exec "${dir}/{binFile}" "$@"
"#;

const PHPUNIT_STREAM_READ: &str = "
                $data = str_replace('__DIR__', var_export(dirname($this->realpath), true), $data);
                $data = str_replace('__FILE__', var_export($this->realpath, true), $data);";

const STREAM_PROXY: &str = r#"if (PHP_VERSION_ID < 80000) {
    if (!class_exists('Composer\BinProxyWrapper')) {
        /**
         * @internal
         */
        final class BinProxyWrapper
        {
            private $handle;
            private $position;
            private $realpath;

            public function stream_open($path, $mode, $options, &$opened_path)
            {
                // get rid of phpvfscomposer:// prefix for __FILE__ & __DIR__ resolution
                $opened_path = substr($path, 17);
                $this->realpath = realpath($opened_path) ?: $opened_path;
                $opened_path = {hack1}$this->realpath;
                $this->handle = fopen($this->realpath, $mode);
                $this->position = 0;

                return (bool) $this->handle;
            }

            public function stream_read($count)
            {
                $data = fread($this->handle, $count);

                if ($this->position === 0) {
                    $data = preg_replace('{^#!.*\r?\n}', '', $data);
                }{hack2}

                $this->position += strlen($data);

                return $data;
            }

            public function stream_cast($castAs)
            {
                return $this->handle;
            }

            public function stream_close()
            {
                fclose($this->handle);
            }

            public function stream_lock($operation)
            {
                return $operation ? flock($this->handle, $operation) : true;
            }

            public function stream_seek($offset, $whence)
            {
                if (0 === fseek($this->handle, $offset, $whence)) {
                    $this->position = ftell($this->handle);
                    return true;
                }

                return false;
            }

            public function stream_tell()
            {
                return $this->position;
            }

            public function stream_eof()
            {
                return feof($this->handle);
            }

            public function stream_stat()
            {
                return array();
            }

            public function stream_set_option($option, $arg1, $arg2)
            {
                return true;
            }

            public function url_stat($path, $flags)
            {
                $path = substr($path, 17);
                if (file_exists($path)) {
                    return stat($path);
                }

                return false;
            }
        }
    }

    if (
        (function_exists('stream_get_wrappers') && in_array('phpvfscomposer', stream_get_wrappers(), true))
        || (function_exists('stream_wrapper_register') && stream_wrapper_register('phpvfscomposer', 'Composer\BinProxyWrapper'))
    ) {
        return include("phpvfscomposer://" . {binPathExported});
    }
}
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{read, tmp_dir};
    use komposer_package::io::{Io, Verbosity};

    fn installer(root: &str, compat: &str) -> BinaryInstaller {
        BinaryInstaller { bin_dir: format!("{root}/vendor/bin"), bin_compat: compat.into(), vendor_dir: format!("{root}/vendor") }
    }

    fn package(name: &str, bins: &[&str]) -> Package {
        let mut p = Package::new(name, "1.0.0.0", "1.0.0");
        p.bin = bins.iter().map(|b| b.to_string()).collect();
        p
    }

    fn write(path: &str, contents: &str) {
        std::fs::create_dir_all(dirname(path)).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    #[test]
    fn proxies_match_composer_byte_for_byte() {
        let root = tmp_dir("bin-golden");
        let io = Io::new(Verbosity::Normal, Some(false), false);
        let out = Out::new(&io);
        let bins = installer(&root, "proxy");
        let cases = [
            ("laravel/pint", "builds/pint", "#!/usr/bin/env php\n<?php\n", include_str!("../tests/fixtures/bin/pint")),
            ("phpunit/phpunit", "phpunit", "#!/usr/bin/env php\n<?php declare(strict_types=1);\n", include_str!("../tests/fixtures/bin/phpunit")),
            ("laravel/sail", "bin/sail", "#!/usr/bin/env bash\n\nUNAMEOUT=\"$(uname -s)\"\n", include_str!("../tests/fixtures/bin/sail")),
        ];
        for (name, bin, contents, expected) in cases {
            write(&format!("{root}/vendor/{name}/{bin}"), contents);
            bins.install(&out, &package(name, &[bin]), &format!("{root}/vendor/{name}"), true).unwrap();
            assert_eq!(read(format!("{root}/vendor/bin/{}", basename(bin))), expected, "{name}");
        }
        assert!(out.take().is_empty());

        // without a shebang no stream wrapper is needed
        write(&format!("{root}/vendor/a/b/run"), "<?php\necho 1;");
        bins.install(&out, &package("a/b", &["run"]), &format!("{root}/vendor/a/b"), true).unwrap();
        assert_eq!(
            read(format!("{root}/vendor/bin/run")),
            "#!/usr/bin/env php\n<?php\n\n/**\n * Proxy PHP file generated by Composer\n *\n * This file includes the referenced bin path (../a/b/run)\n *\n * @generated\n */\n\nnamespace Composer;\n\n$GLOBALS['_composer_bin_dir'] = __DIR__;\n$GLOBALS['_composer_autoload_path'] = __DIR__ . '/..'.'/autoload.php';\n\n\nreturn include __DIR__ . '/..'.'/a/b/run';\n"
        );
    }

    #[test]
    fn full_compat_adds_bat_proxies() {
        let root = tmp_dir("bin-full");
        let io = Io::new(Verbosity::Normal, Some(false), false);
        let out = Out::new(&io);
        let bins = installer(&root, "full");
        write(&format!("{root}/vendor/laravel/pint/builds/pint"), "#!/usr/bin/env php\n<?php\n");
        write(&format!("{root}/vendor/laravel/sail/bin/sail"), "#!/usr/bin/env bash\n");
        bins.install(&out, &package("laravel/pint", &["builds/pint"]), &format!("{root}/vendor/laravel/pint"), true).unwrap();
        bins.install(&out, &package("laravel/sail", &["bin/sail"]), &format!("{root}/vendor/laravel/sail"), true).unwrap();
        assert_eq!(read(format!("{root}/vendor/bin/pint")), include_str!("../tests/fixtures/bin/pint"));
        assert_eq!(
            read(format!("{root}/vendor/bin/pint.bat")),
            "@ECHO OFF\r\nsetlocal DISABLEDELAYEDEXPANSION\r\nSET BIN_TARGET=%~dp0/pint\r\nSET COMPOSER_RUNTIME_BIN_DIR=%~dp0\r\nphp \"%BIN_TARGET%\" %*\r\n"
        );
        assert_eq!(
            read(format!("{root}/vendor/bin/sail.bat")),
            "@ECHO OFF\r\nsetlocal DISABLEDELAYEDEXPANSION\r\nSET BIN_TARGET=%~dp0/../laravel/sail/bin/sail\r\nSET COMPOSER_RUNTIME_BIN_DIR=%~dp0\r\nbash \"%BIN_TARGET%\" %*\r\n"
        );

        bins.remove(&package("laravel/pint", &["builds/pint"])).unwrap();
        assert!(!Path::new(&format!("{root}/vendor/bin/pint.bat")).exists());
        bins.remove(&package("laravel/sail", &["bin/sail"])).unwrap();
        assert!(!Path::new(&format!("{root}/vendor/bin")).exists(), "the empty bin dir is removed");
    }

    #[cfg(unix)]
    #[test]
    fn shell_proxies_run_their_target() {
        let root = tmp_dir("bin-exec");
        let io = Io::new(Verbosity::Normal, Some(false), false);
        let out = Out::new(&io);
        write(&format!("{root}/vendor/foo/bar/binary"), "#!/bin/sh\nprintf 'success %s' \"$1\"\n");
        installer(&root, "proxy").install(&out, &package("foo/bar", &["binary"]), &format!("{root}/vendor/foo/bar"), true).unwrap();
        let run = std::process::Command::new(format!("{root}/vendor/bin/binary")).arg("arg").current_dir(&*root).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&run.stderr), "");
        assert_eq!(String::from_utf8_lossy(&run.stdout), "success arg");
    }

    #[cfg(unix)]
    #[test]
    fn skips_bins_outside_the_package_and_conflicts() {
        use std::os::unix::fs::PermissionsExt;
        let root = tmp_dir("bin-escape");
        let io = Io::new(Verbosity::Normal, Some(false), false);
        let out = Out::new(&io);
        let bins = installer(&root, "full");
        let victim = format!("{root}/victim.sh");
        write(&victim, "#!/bin/sh\necho pwned\n");
        std::fs::set_permissions(&victim, std::fs::Permissions::from_mode(0o644)).unwrap();
        let install_path = format!("{root}/vendor/attacker/pkg");
        std::fs::create_dir_all(format!("{install_path}/bin")).unwrap();
        std::os::unix::fs::symlink("../../../../victim.sh", format!("{install_path}/bin/pwn")).unwrap();

        bins.install(&out, &package("attacker/pkg", &["bin/pwn", "../../../victim.sh", "missing", "bin"]), &install_path, true).unwrap();
        assert!(!Path::new(&format!("{root}/vendor/bin/pwn")).exists());
        assert!(!Path::new(&format!("{root}/vendor/bin/victim.sh")).exists());
        assert_eq!(std::fs::metadata(&victim).unwrap().permissions().mode() & 0o777, 0o644, "never chmod'd");
        assert_eq!(
            out.take(),
            [
                "    Skipped installation of bin bin/pwn for package attacker/pkg: the bin resolves to a path outside of the package directory",
                "    Skipped installation of bin ../../../victim.sh for package attacker/pkg: the bin resolves to a path outside of the package directory",
                "    Skipped installation of bin missing for package attacker/pkg: file not found in package",
                "    Skipped installation of bin bin for package attacker/pkg: found a directory at that path",
            ]
        );

        write(&format!("{root}/vendor/bin/tool"), "mine");
        write(&format!("{root}/vendor/foo/bar/tool"), "#!/bin/sh\n");
        bins.install(&out, &package("foo/bar", &["tool"]), &format!("{root}/vendor/foo/bar"), true).unwrap();
        assert_eq!(read(format!("{root}/vendor/bin/tool")), "mine");
        assert_eq!(out.take(), ["    Skipped installation of bin tool for package foo/bar: name conflicts with an existing file"]);
        bins.install(&out, &package("foo/bar", &["tool"]), &format!("{root}/vendor/foo/bar"), false).unwrap();
        assert!(out.take().is_empty(), "ensureBinariesPresence does not warn");
    }

    #[test]
    fn determines_binary_callers() {
        let root = tmp_dir("bin-caller");
        let cases = [
            ("#!/usr/bin/env php\n<?php", "php"),
            ("#!/bin/sh\n", "sh"),
            ("#!/bin/sh -e\n", "sh -e"),
            ("#!/usr/bin/env php -d memory_limit=-1\n", "php -d memory_limit=-1"),
            ("#!/usr/bin/php7.4\n", "php7.4"),
            ("#!/usr/bin/env php\r\n<?php", "php"),
            ("#!/usr/bin/env php  \n", "php"),
            ("  #!/bin/sh\n", "php"),
            ("<?php\n", "php"),
        ];
        for (contents, expected) in cases {
            let bin = format!("{root}/caller-bin");
            std::fs::write(&bin, contents).unwrap();
            assert_eq!(determine_binary_caller(&bin), expected, "{contents:?}");
        }
        assert_eq!(determine_binary_caller(&format!("{root}/does-not-exist.bat")), "call");
        assert_eq!(determine_binary_caller(&format!("{root}/does-not-exist.exe")), "call");
    }
}
