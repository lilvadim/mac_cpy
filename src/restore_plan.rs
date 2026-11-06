use crate::{Result, archive::archive_paths, check_cancel, error, process};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    fs, io,
    os::unix::{ffi::OsStringExt, fs::MetadataExt},
    path::{Path, PathBuf},
    process::Command,
};

pub(crate) struct Entry {
    directory: bool,
    link: Option<PathBuf>,
}

pub(crate) struct Index(pub BTreeMap<PathBuf, Entry>);

pub(crate) struct Plan {
    pub replaced: BTreeSet<PathBuf>,
    pub unchanged_links: BTreeSet<PathBuf>,
}

impl Index {
    pub fn read(archive: &Path) -> Result<Self> {
        // Validate original names first: format conversion can normalize unsafe paths.
        let listing = process::tar_progress(
            Command::new("/usr/bin/tar").arg("-tzf").arg(archive),
            "Validating archive paths",
            None,
            process::TarMode::List,
        )?;
        let paths = archive_paths(std::str::from_utf8(&listing)?)?;
        let mut operand = OsString::from("@");
        operand.push(archive);
        // Convert headers to escaped, machine-readable text; no file data is extracted.
        let data = process::tar_progress(
            Command::new("/usr/bin/tar")
                .args([
                    "-cf",
                    "-",
                    "--format=mtree",
                    "--options=!all,type,link,!use-set",
                ])
                .arg(operand),
            "Reading archive index",
            None,
            process::TarMode::List,
        )?;
        let index = Self::parse(&data)?;
        if !paths.iter().eq(index.0.keys()) {
            return Err(error(
                "Archive path and type indexes disagree; restore aborted before changes",
            ));
        }
        Ok(index)
    }

    fn parse(data: &[u8]) -> Result<Self> {
        let text = std::str::from_utf8(data)?;
        let mut entries = BTreeMap::new();
        for line in text.lines() {
            check_cancel()?;
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut fields = line.split_ascii_whitespace();
            let path = decode(fields.next().ok_or_else(|| error("Missing archive path"))?)?;
            let path = path
                .to_str()
                .ok_or_else(|| error("Non-UTF-8 archive path"))?;
            let path = archive_paths(path)?.remove(0);
            let (mut kind, mut link) = (None, None);
            for field in fields {
                if let Some(value) = field.strip_prefix("type=") {
                    kind = Some(value);
                } else if let Some(value) = field.strip_prefix("link=") {
                    link = Some(PathBuf::from(decode(value)?));
                } else {
                    return Err(error(format!("Unexpected archive index field: {field}")));
                }
            }
            let kind = kind.ok_or_else(|| error("Missing archive entry type"))?;
            if (kind == "link") != link.is_some() {
                return Err(error("Invalid symbolic link in archive index"));
            }
            let entry = Entry {
                directory: kind == "dir",
                link,
            };
            if entries.insert(path, entry).is_some() {
                return Err(error(
                    "Duplicate archive path; restore aborted before changes",
                ));
            }
        }
        if entries.is_empty() {
            return Err(error("Empty archive index"));
        }
        // Detect links/files used as directories inside the archive before any extraction.
        for path in entries.keys() {
            for parent in path.ancestors().skip(1) {
                if entries.get(parent).is_some_and(|entry| !entry.directory) {
                    return Err(error(format!(
                        "Archive uses a non-directory as a parent: {}",
                        parent.display()
                    )));
                }
            }
        }
        Ok(Self(entries))
    }

    pub fn plan(&self, home: &Path, sources: &[PathBuf]) -> Result<Plan> {
        let mut protected = Vec::new();
        for source in sources {
            // Protect the checksum link itself as well as its resolved target.
            for path in [source.clone(), fs::canonicalize(source)?] {
                let metadata = fs::symlink_metadata(&path)?;
                protected.push((path, metadata.dev(), metadata.ino()));
            }
        }
        let progress = crate::progress::Progress::new(
            "Checking destination",
            Some(self.0.len() as u64),
            crate::progress::Unit::Items,
        );
        let handle = progress.handle();
        let mut plan = Plan {
            replaced: BTreeSet::new(),
            unchanged_links: BTreeSet::new(),
        };
        for (path, entry) in &self.0 {
            check_cancel()?;
            for parent in path
                .ancestors()
                .skip(1)
                .filter(|p| !p.as_os_str().is_empty())
            {
                let target = home.join(parent);
                if let Some(metadata) = inspect(&target)? {
                    if metadata.file_type().is_symlink() {
                        return Err(error(format!(
                            "Parent directory is a symlink: {} -> {}. Restore aborted before changes",
                            target.display(),
                            fs::read_link(&target)?.display()
                        )));
                    }
                    if !metadata.is_dir() {
                        return Err(error(format!(
                            "Parent is not a directory: {}",
                            target.display()
                        )));
                    }
                }
            }
            let target = home.join(path);
            let metadata = inspect(&target)?;
            for (source, device, inode) in &protected {
                let same_file = metadata
                    .as_ref()
                    .is_some_and(|m| m.dev() == *device && m.ino() == *inode);
                let replaces_source_path =
                    source.starts_with(&target) && (source == &target || !entry.directory);
                // File identity also catches hard links and case-insensitive aliases.
                if same_file || replaces_source_path {
                    return Err(error(format!(
                        "Restore would overwrite its source archive or checksum: {} (archive entry: {}). Restore aborted before changes",
                        source.display(),
                        path.display()
                    )));
                }
            }
            if let Some(metadata) = metadata {
                let symlink = metadata.file_type().is_symlink();
                if entry.directory && symlink {
                    return Err(error(format!(
                        "Archive directory conflicts with a destination symlink: {} -> {}. Restore aborted before changes",
                        target.display(),
                        fs::read_link(&target)?.display()
                    )));
                }
                if metadata.is_dir() != entry.directory {
                    return Err(error(format!(
                        "File/directory type conflict: {}",
                        target.display()
                    )));
                }
                let same_link = if symlink {
                    let current = fs::read_link(&target)?;
                    entry
                        .link
                        .as_ref()
                        .is_some_and(|link| current.as_os_str() == link.as_os_str())
                } else {
                    false
                };
                if same_link {
                    plan.unchanged_links.insert(path.clone());
                } else if !metadata.is_dir() {
                    plan.replaced.insert(path.clone());
                }
            }
            handle.advance();
        }
        progress.finish(true);
        Ok(plan)
    }
}

fn inspect(path: &Path) -> io::Result<Option<fs::Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

fn decode(text: &str) -> Result<OsString> {
    let mut bytes = Vec::with_capacity(text.len());
    let mut input = text.bytes();
    while let Some(byte) = input.next() {
        let byte = if byte == b'\\' {
            let mut value = 0u16;
            for _ in 0..3 {
                let digit = input
                    .next()
                    .filter(|b| (b'0'..=b'7').contains(b))
                    .ok_or_else(|| error("Invalid mtree escape"))?;
                value = value * 8 + u16::from(digit - b'0');
            }
            u8::try_from(value).map_err(|_| error("Invalid mtree byte"))?
        } else {
            byte
        };
        if byte == 0 {
            return Err(error("NUL in archive index"));
        }
        bytes.push(byte);
    }
    Ok(OsString::from_vec(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_escaped_links_and_rejects_archive_parent_links() {
        let index =
            Index::parse(b"#mtree\n./a\\040link type=link link=../back\\134slash\\012next\n")
                .unwrap();
        assert_eq!(
            index.0[Path::new("a link")].link.as_deref(),
            Some(Path::new("../back\\slash\nnext"))
        );
        for data in [
            "./a type=link link=outside\n./a/file type=file\n",
            "./a type=file\n./a/file type=file\n",
            "./a type=file\n./a type=link link=outside\n",
            "../outside type=file\n",
        ] {
            assert!(Index::parse(data.as_bytes()).is_err(), "{data}");
        }
    }
}
