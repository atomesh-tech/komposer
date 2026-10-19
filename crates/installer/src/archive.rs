//! The extraction half of `ArchiveDownloader::install()` and the `extract()` of
//! `ZipDownloader`, `TarDownloader`, `XzDownloader`, `GzipDownloader` and `PharDownloader`.
//! Runs on worker threads, so it prints nothing.

use crate::fs::{self, basename, normalize_path};
use anyhow::{Context, Result, anyhow, bail};
use std::io::{BufRead, BufReader, Read};
use std::path::Path;

/// One archive to unpack into an install path.
pub(crate) struct Extract {
    /// dist type: zip, tar, xz, gzip or phar
    pub kind: String,
    /// the downloaded archive
    pub file: String,
    /// `vendor/composer/<random>`
    pub temp: String,
    pub path: String,
    pub vendor_dir: String,
    /// the file name in the dist URL (gzip and phar dists)
    pub dist_name: String,
    /// `$package` (unique name) for messages
    pub package: String,
    pub name: String,
    /// for updates: the previous install, removed first
    pub remove_first: Option<String>,
}

impl Extract {
    pub fn run(&self) -> Result<()> {
        if let Some(dir) = &self.remove_first {
            remove_install(dir)?;
        }
        if !contains_vendor_dir(&self.path, &self.vendor_dir) {
            fs::empty_directory(&self.path, true)?;
        }
        fs::ensure_directory_exists(&self.temp)?;
        self.extract()?;
        self.move_into_place()
    }

    fn extract(&self) -> Result<()> {
        let (file, dir) = (self.file.as_str(), self.temp.as_str());
        match self.kind.as_str() {
            "zip" => extract_zip(file, dir, &self.name),
            "tar" => extract_tar(file, dir),
            "xz" => tar_xz(file, dir),
            "gzip" => {
                let name = self.dist_name.rsplit_once('.').map_or(self.dist_name.as_str(), |(stem, _)| stem);
                let mut target = std::fs::File::create(format!("{dir}/{name}"))?;
                std::io::copy(&mut flate2::read::GzDecoder::new(std::fs::File::open(file)?), &mut target)?;
                Ok(())
            }
            // komposer can not read phar archives, the phar itself is installed
            "phar" => {
                std::fs::copy(file, format!("{dir}/{}", self.dist_name))?;
                Ok(())
            }
            kind => bail!("{kind} archives are not supported by komposer"),
        }
    }

    /// The promise callback of `ArchiveDownloader::install()`: move the extracted files into the
    /// install path, flattening a single top-level directory.
    fn move_into_place(&self) -> Result<()> {
        if Path::new(&self.file).exists() {
            fs::unlink(&self.file)?;
        }
        let path = self.path.as_str();
        let rename_as_one = !Path::new(path).exists() || (fs::is_dir_empty(path) && std::fs::remove_dir(path).is_ok());
        let content = folder_content(&self.temp)?;
        let single_dir = (content.len() == 1 && Path::new(&content[0]).is_dir()).then(|| content[0].as_str());
        let from = single_dir.unwrap_or(&self.temp);
        if rename_as_one {
            // the target is clear, move the package in one go
            fs::rename(from, path)?;
        } else {
            self.rename_recursively(from, path)?;
        }
        fs::remove_directory(&self.temp)?;
        Ok(())
    }

    /// Merges `from` into `to`, as packages may share paths with custom installers.
    fn rename_recursively(&self, from: &str, to: &str) -> Result<()> {
        for file in folder_content(from)? {
            let target = format!("{to}/{}", basename(&file));
            if Path::new(&target).is_dir() {
                if !Path::new(&file).is_dir() {
                    bail!(
                        "Installing {} would lead to overwriting the {target} directory with a file from the package, invalid operation.",
                        self.package
                    );
                }
                self.rename_recursively(&file, &target)?;
            } else {
                fs::rename(&file, &target)?;
            }
        }
        Ok(())
    }
}

/// Installs clear their target first, unless it contains the vendor dir (create-project
/// into the cwd), as the download lives there.
pub(crate) fn contains_vendor_dir(path: &str, vendor_dir: &str) -> bool {
    normalize_path(vendor_dir).contains(&normalize_path(&format!("{path}/")))
}

/// `FileDownloader::remove()`'s directory removal.
pub(crate) fn remove_install(path: &str) -> Result<()> {
    if !fs::remove_directory(path)? || Path::new(path).is_dir() {
        bail!("Could not completely delete {path}, aborting.");
    }
    Ok(())
}

/// Direct children, including dotfiles and VCS dirs but not `.DS_Store`.
fn folder_content(dir: &str) -> Result<Vec<String>> {
    let mut content = vec![];
    for entry in std::fs::read_dir(dir)? {
        let name = entry?.file_name();
        if name != ".DS_Store" {
            content.push(format!("{dir}/{}", name.to_string_lossy()));
        }
    }
    Ok(content)
}

fn extract_zip(file: &str, dir: &str, name: &str) -> Result<()> {
    let size = std::fs::metadata(file).map(|m| m.len()).unwrap_or(0);
    if size == 0 {
        bail!("'{file}' is a corrupted zip archive (0 bytes), try again.");
    }
    let mut zip = zip::ZipArchive::new(std::fs::File::open(file)?).map_err(|e| anyhow!(e).context(format!("'{file}' is not a zip archive.")))?;
    // ZipDownloader's zip bomb heuristic, applied to every entry instead of a random sample
    let (mut total, mut suspicious) = (0u64, false);
    for i in 0..zip.len() {
        if let Ok(entry) = zip.by_index_raw(i) {
            total += entry.size();
            suspicious |= entry.size() > entry.compressed_size().saturating_mul(200);
        }
    }
    if suspicious && total > size * 100 && total > 50 * 1024 * 1024 {
        bail!("Invalid zip file for \"{name}\" with compression ratio >99% (possible zip bomb)");
    }
    zip.extract(dir).map_err(|e| {
        anyhow!(e).context(format!("There was an error extracting the ZIP file for \"{name}\", it is either corrupted or using an invalid format."))
    })
}

/// `XzDownloader::extract()`: the system tar unpacks these.
fn tar_xz(file: &str, dir: &str) -> Result<()> {
    match std::process::Command::new("tar").args(["-xJf", file, "-C", dir]).output() {
        Ok(out) if out.status.success() => Ok(()),
        out => {
            let stderr = out.map(|o| String::from_utf8_lossy(&o.stderr).into_owned()).unwrap_or_default();
            bail!("Failed to execute tar -xJf {file} -C {dir}\n\n{stderr}")
        }
    }
}

/// `PharData::extractTo()` for tar, tar.gz and tar.bz2.
fn extract_tar(file: &str, dir: &str) -> Result<()> {
    let mut reader = BufReader::new(std::fs::File::open(file)?);
    let magic = reader.fill_buf()?.to_vec();
    let reader: Box<dyn Read> = match magic.as_slice() {
        [0x1f, 0x8b, ..] => Box::new(flate2::read::GzDecoder::new(reader)),
        [b'B', b'Z', b'h', ..] => Box::new(bzip2::read::BzDecoder::new(reader)),
        _ => Box::new(reader),
    };
    tar::Archive::new(reader).unpack(dir).with_context(|| format!("Could not extract {file}"))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::Write;

    /// Builds a zip from `(path, contents, unix mode)`; paths ending in `/` are directories.
    pub fn zip(path: &str, entries: &[(&str, &str, u32)]) {
        let mut w = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        for &(name, contents, mode) in entries {
            let opts = zip::write::SimpleFileOptions::default().unix_permissions(mode);
            if name.ends_with('/') {
                w.add_directory(name, opts).unwrap();
            } else {
                w.start_file(name, opts).unwrap();
                w.write_all(contents.as_bytes()).unwrap();
            }
        }
        w.finish().unwrap();
    }

    pub fn tar_gz(path: &str, entries: &[(&str, &str, u32)]) {
        let gz = flate2::write::GzEncoder::new(std::fs::File::create(path).unwrap(), flate2::Compression::default());
        let mut b = tar::Builder::new(gz);
        for &(name, contents, mode) in entries {
            let mut h = tar::Header::new_gnu();
            h.set_size(contents.len() as u64);
            h.set_mode(mode);
            h.set_cksum();
            b.append_data(&mut h, name, contents.as_bytes()).unwrap();
        }
        b.into_inner().unwrap().finish().unwrap();
    }

    fn extract(kind: &str, file: &str, path: &str, vendor: &str) -> Extract {
        Extract {
            kind: kind.into(),
            file: file.into(),
            temp: format!("{vendor}/composer/0123abcd"),
            path: path.into(),
            vendor_dir: vendor.into(),
            dist_name: basename(file).to_string(),
            package: "foo/bar-1.0.0.0".into(),
            name: "foo/bar".into(),
            remove_first: None,
        }
    }

    #[cfg(unix)]
    fn mode(path: &str) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn flattens_a_single_top_level_dir_and_keeps_modes() {
        let dir = crate::tests::tmp_dir("archive-flatten");
        let file = format!("{dir}/a.zip");
        zip(&file, &[("foo-bar-abc123/", "", 0o755), ("foo-bar-abc123/bin/run", "#!/bin/sh\n", 0o755), ("foo-bar-abc123/src/A.php", "<?php", 0o644)]);
        let path = format!("{dir}/vendor/foo/bar");
        extract("zip", &file, &path, &format!("{dir}/vendor")).run().unwrap();
        assert!(Path::new(&format!("{path}/src/A.php")).is_file());
        assert!(!Path::new(&file).exists(), "archive is removed");
        assert!(!Path::new(&format!("{dir}/vendor/composer/0123abcd")).exists(), "temp dir is removed");
        #[cfg(unix)]
        {
            assert_eq!(mode(&format!("{path}/bin/run")), 0o755);
            assert_eq!(mode(&format!("{path}/src/A.php")), 0o644);
        }
    }

    #[test]
    fn xz_dists_go_through_the_system_tar() {
        let dir = crate::tests::tmp_dir("archive-xz");
        std::fs::create_dir_all(format!("{dir}/src/pkg")).unwrap();
        std::fs::write(format!("{dir}/src/pkg/A.php"), "<?php").unwrap();
        let file = format!("{dir}/a.tar.xz");
        let packed = std::process::Command::new("tar").args(["-cJf", &file, "-C", &format!("{dir}/src"), "pkg"]).status();
        if !packed.is_ok_and(|s| s.success()) {
            return; // no xz support in this tar
        }
        let path = format!("{dir}/vendor/foo/bar");
        extract("xz", &file, &path, &format!("{dir}/vendor")).run().unwrap();
        assert_eq!(std::fs::read_to_string(format!("{path}/A.php")).unwrap(), "<?php");

        std::fs::write(&file, "not xz").unwrap();
        let err = extract("xz", &file, &path, &format!("{dir}/vendor")).run().unwrap_err().to_string();
        assert!(err.starts_with(&format!("Failed to execute tar -xJf {file} -C {dir}/vendor/composer/0123abcd\n\n")), "{err}");
    }

    #[test]
    fn keeps_multiple_top_level_entries_and_merges_into_existing_dirs() {
        let dir = crate::tests::tmp_dir("archive-merge");
        let file = format!("{dir}/a.tar.gz");
        tar_gz(&file, &[("README", "hi", 0o644), ("src/B.php", "<?php", 0o644), (".DS_Store", "", 0o644)]);
        let path = format!("{dir}/vendor/foo/bar");
        std::fs::create_dir_all(format!("{path}/src")).unwrap();
        std::fs::write(format!("{path}/keep"), "").unwrap();
        let mut e = extract("tar", &file, &path, &format!("{dir}/vendor"));
        // not emptied when it contains the vendor dir, so the merge is visible
        e.vendor_dir = format!("{path}/vendor");
        e.run().unwrap();
        assert!(Path::new(&format!("{path}/README")).is_file());
        assert!(Path::new(&format!("{path}/src/B.php")).is_file());
        assert!(Path::new(&format!("{path}/keep")).is_file());
        assert!(!Path::new(&format!("{path}/.DS_Store")).exists());
    }

    #[test]
    fn rejects_path_traversal() {
        let dir = crate::tests::tmp_dir("archive-traversal");
        let file = format!("{dir}/evil.zip");
        zip(&file, &[("../../evil.txt", "x", 0o644), ("ok.txt", "x", 0o644)]);
        let path = format!("{dir}/vendor/foo/bar");
        let _ = extract("zip", &file, &path, &format!("{dir}/vendor")).run();
        assert!(!Path::new(&format!("{dir}/evil.txt")).exists());
        assert!(!Path::new(&format!("{dir}/vendor/evil.txt")).exists());
    }

    #[test]
    fn reports_broken_zips_like_composer() {
        let dir = crate::tests::tmp_dir("archive-broken");
        let empty = format!("{dir}/empty.zip");
        std::fs::write(&empty, "").unwrap();
        let err = extract("zip", &empty, &format!("{dir}/p"), &format!("{dir}/vendor")).run().unwrap_err();
        assert_eq!(err.to_string(), format!("'{empty}' is a corrupted zip archive (0 bytes), try again."));
        let junk = format!("{dir}/junk.zip");
        std::fs::write(&junk, "not a zip").unwrap();
        let err = extract("zip", &junk, &format!("{dir}/p"), &format!("{dir}/vendor")).run().unwrap_err();
        assert_eq!(err.to_string(), format!("'{junk}' is not a zip archive."));
    }

    #[test]
    fn gunzips_single_files() {
        let dir = crate::tests::tmp_dir("archive-gzip");
        let file = format!("{dir}/tool.phar.gz");
        let mut gz = flate2::write::GzEncoder::new(std::fs::File::create(&file).unwrap(), flate2::Compression::default());
        gz.write_all(b"payload").unwrap();
        gz.finish().unwrap();
        let path = format!("{dir}/vendor/foo/tool");
        extract("gzip", &file, &path, &format!("{dir}/vendor")).run().unwrap();
        assert_eq!(std::fs::read_to_string(format!("{path}/tool.phar")).unwrap(), "payload");
    }
}
