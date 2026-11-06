use crate::{
    Result, check_cancel,
    config::Config,
    error, estimate, exists,
    layout::{INFO_DIR, OVERWRITTEN_DIR},
    metadata, policy, process,
    progress::{Progress, Unit},
    scan::Scan,
};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::{ffi::OsStrExt, fs::OpenOptionsExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

pub(crate) fn backup_path(destination: &Path) -> Result<PathBuf> {
    Ok(destination.join(format!("backup-{}.tar.gz", stamp()?)))
}

pub(crate) fn output_paths(archive: &Path) -> [PathBuf; 3] {
    [
        archive.into(),
        suffix(archive, ".partial"),
        suffix(archive, ".sha256"),
    ]
}

pub(crate) fn backup(
    home: &Path,
    archive: &Path,
    selected: &[PathBuf],
    config: &Config,
    scan: &Scan,
    metadata_only: bool,
) -> Result<()> {
    let selected: Vec<_> = selected
        .iter()
        .filter(|p| !scan.skips(p))
        .cloned()
        .collect();
    let included: Vec<_> = scan
        .included
        .iter()
        .filter(|p| !scan.skips(p))
        .cloned()
        .collect();
    crate::checks::before_backup(&included)?;
    println!("[2/4] Collecting metadata...");
    let stage = tempfile::Builder::new().prefix("mac-cpy-").tempdir()?;
    let metadata_progress = Progress::new("Collecting metadata", None, Unit::Unknown);
    metadata::collect(home, stage.path(), &selected, config, scan)?;
    if metadata_only {
        let info = stage.path().join(INFO_DIR);
        fs::write(info.join("MODE"), "metadata-only\n")?;
        fs::write(
            info.join("README.txt"),
            "Metadata-only backup: no home files or application data included.\nRestore with mac_cpy -r ARCHIVE --home HOME.\nInstall apps separately using metadata/Brewfile; see metadata/Install-commands.txt. Review hosts manually.\nIncludes mac_cpy executable and configuration snapshots.\nArchives are not encrypted.\n",
        )?;
    }
    metadata_progress.finish(true);
    let partial = suffix(archive, ".partial");
    private_file(&partial)?;
    println!("[3/4] Writing archive: {}", archive.display());
    let mut command = tar();
    command
        .args([
            "--create",
            "--gzip",
            "--format=pax",
            "--mac-metadata",
            "--xattrs",
            "--verbose",
            "--file",
        ])
        .arg(&partial);
    let mut source_list = tempfile::NamedTempFile::new()?;
    for path in &scan.included {
        if !scan.skips(path) {
            source_list.write_all(b"./")?;
            source_list.write_all(path.as_os_str().as_bytes())?;
            source_list.write_all(b"\n")?;
        }
    }
    // Archive exactly the scanned selection; tar must not reapply lower-priority excludes.
    command
        .current_dir(home)
        .arg("--no-recursion")
        .arg("-T")
        .arg(source_list.path());
    // All data paths reject line breaks and backslashes. Native -T directives
    // switch to the private metadata directory without reordering the source list.
    source_list.write_all(b"-C\n")?;
    source_list.write_all(stage.path().as_os_str().as_bytes())?;
    source_list.write_all(b"\n")?;
    let (mut metadata_items, mut metadata_bytes) = (0u64, 0u64);
    for entry in walkdir::WalkDir::new(stage.path().join(INFO_DIR))
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|entry| {
            entry
                .path()
                .strip_prefix(stage.path())
                .is_ok_and(|path| !config.exclude_rules.matches(path))
        })
    {
        check_cancel()?;
        let entry = entry?;
        let relative = entry.path().strip_prefix(stage.path())?;
        metadata_items += 1;
        if entry.file_type().is_file() {
            metadata_bytes = metadata_bytes.saturating_add(entry.metadata()?.len());
        }
        source_list.write_all(b"./")?;
        source_list.write_all(relative.as_os_str().as_bytes())?;
        source_list.write_all(b"\n")?;
    }
    if let Err(e) = process::tar_progress_estimated(&mut command, "Creating archive", || {
        estimate::background_totals(scan, metadata_items, metadata_bytes)
    }) {
        return Err(error(format!(
            "Backup failed: {e}. Incomplete file: {}",
            partial.display()
        )));
    }
    println!("[4/4] Verifying archive and calculating SHA-256...");
    let item_total = scan.entries().saturating_add(metadata_items);
    check_archive(&partial, Some(item_total))?;
    complete_archive(&partial, archive)?;
    println!(
        "{}: {}\nSHA-256: {}\nWARNING: Not encrypted. Keep both files together.",
        if scan.problems.is_empty() {
            "DONE"
        } else {
            "DONE WITH SKIPS"
        },
        archive.display(),
        suffix(archive, ".sha256").display()
    );
    Ok(())
}

pub(crate) fn finalize(source: &Path, dry: bool) -> Result<()> {
    if fs::symlink_metadata(source)?.file_type().is_symlink() {
        return Err(error(
            "Specify a regular .tar.gz.partial file, not a symlink",
        ));
    }
    let partial = fs::canonicalize(source)?;
    let name = partial.file_name().unwrap().to_string_lossy();
    let Some(name) = name.strip_suffix(".tar.gz.partial") else {
        return Err(error("--finalize requires a .tar.gz.partial file"));
    };
    if !partial.is_file() {
        return Err(error("--finalize requires a regular file"));
    }
    let archive = partial.with_file_name(format!("{name}.tar.gz"));
    if exists(&archive) || exists(&suffix(&archive, ".sha256")) {
        return Err(error(
            "Final archive or checksum already exists; nothing changed",
        ));
    }
    println!("[1/2] Verifying existing archive: {}", partial.display());
    check_archive(&partial, None)?;
    if dry {
        println!("DRY RUN complete. Archive readable; no files changed.");
        return Ok(());
    }
    check_cancel()?;
    println!("[2/2] Calculating SHA-256 and completing archive...");
    complete_archive(&partial, &archive)?;
    println!(
        "DONE: {}\nSHA-256: {}\nWARNING: Review the archived metadata/skipped-paths.txt report before removing source data. Keep both files together.",
        archive.display(),
        suffix(&archive, ".sha256").display()
    );
    Ok(())
}

fn check_archive(partial: &Path, total: Option<u64>) -> Result<()> {
    let listing = String::from_utf8(process::tar_progress(
        tar().arg("-tzf").arg(partial),
        "Verifying archive",
        total,
        process::TarMode::List,
    )?)?;
    let archived = archive_paths(&listing)?;
    for name in ["FORMAT", "mac_cpy"]
        .into_iter()
        .chain(crate::config::NAMES)
    {
        let required = Path::new(INFO_DIR).join(name);
        if !archived.contains(&required) {
            return Err(error(format!(
                "Configuration excluded required archive metadata: {}. Incomplete file: {}",
                required.display(),
                partial.display()
            )));
        }
    }
    Ok(())
}

fn complete_archive(partial: &Path, archive: &Path) -> Result<()> {
    let flush_progress = Progress::new("Flushing archive", None, Unit::Unknown);
    // A writable descriptor is needed for fsync on some network filesystems.
    // Opening an existing file this way never truncates its contents.
    let file = OpenOptions::new().read(true).write(true).open(partial)?;
    crate::storage::sync(&file, partial)?;
    flush_progress.finish(true);
    let digest = checksum(partial)?;
    check_cancel()?;
    let sidecar = suffix(archive, ".sha256");
    if exists(archive) || exists(&sidecar) {
        return Err(error("Archive name collision; incomplete file retained"));
    }
    // Prepare the checksum before renaming the archive so write errors retain .partial.
    let mut sidecar_file = private_file(&sidecar)?;
    let sidecar_cleanup = tempfile::TempPath::try_from_path(&sidecar)?;
    writeln!(
        sidecar_file,
        "{digest}  {}",
        archive.file_name().unwrap().to_string_lossy()
    )?;
    crate::storage::sync(&sidecar_file, &sidecar)?;
    check_cancel()?;
    fs::rename(partial, archive)?;
    sidecar_cleanup.keep()?;
    Ok(())
}

pub(crate) fn restore(source: &Path, home: &Path, dry: bool) -> Result<()> {
    let archive = fs::canonicalize(source)?;
    if !archive.is_file() {
        return Err(error("Archive must be a regular file"));
    }
    println!("[1/3] Verifying: {}", archive.display());
    verify(&archive)?;
    let metadata_progress = Progress::new("Reading archive metadata", None, Unit::Unknown);
    let format = process::output(
        tar()
            .args(["-xzOf"])
            .arg(&archive)
            .arg(Path::new(INFO_DIR).join("FORMAT")),
    )?;
    metadata_progress.finish(true);
    if format != b"macos-copy-archive-v2\n" {
        return Err(error("Unsupported backup format"));
    }
    let index = crate::restore_plan::Index::read(&archive)?;
    println!("[2/3] Checking destination: {}", home.display());
    let plan = index.plan(home, &[archive.clone(), suffix(&archive, ".sha256")])?;
    let replaced = &plan.replaced;
    println!("Unchanged symlinks: {}", plan.unchanged_links.len());
    if dry {
        println!("\nARCHIVE CONTENTS");
        for path in index.0.keys() {
            println!("{}", path.display());
        }
        println!("\nREPLACED PATHS ({})", replaced.len());
        for path in replaced {
            println!("{}", path.display());
        }
        println!("DRY RUN complete. Checksum OK; destination unchanged.");
        return Ok(());
    }
    let safe_writes = tar_supports("--safe-writes")?;
    if !safe_writes {
        crate::progress::log(
            "WARNING: System tar does not support --safe-writes; individual file replacements are not atomic on this macOS version.",
        );
    }
    if !replaced.is_empty() {
        let folder = home.join(OVERWRITTEN_DIR);
        if fs::symlink_metadata(&folder).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(error("Rollback directory cannot be a symlink"));
        }
        fs::create_dir_all(&folder)?;
        let rollback = folder.join(format!("before-{}.tar.gz", stamp()?));
        let partial = suffix(&rollback, ".partial");
        private_file(&partial)?;
        println!("Saving replaced files: {}", rollback.display());
        let list = tempfile::NamedTempFile::new()?;
        let mut output = list.reopen()?;
        for path in replaced {
            output.write_all(b"./")?;
            output.write_all(path.as_os_str().as_encoded_bytes())?;
            output.write_all(b"\0")?;
        }
        drop(output);
        process::tar_progress(
            tar()
                .args(["-czpvf"])
                .arg(&partial)
                .args([
                    "--format=pax",
                    "--mac-metadata",
                    "--no-recursion",
                    "--null",
                    "-C",
                ])
                .arg(home)
                .arg("-T")
                .arg(list.path()),
            "Saving replaced files",
            Some(replaced.len() as u64),
            process::TarMode::Create,
        )?;
        process::tar_progress(
            tar().arg("-tzf").arg(&partial),
            "Verifying replaced files",
            Some(replaced.len() as u64),
            process::TarMode::List,
        )?;
        if exists(&rollback) {
            return Err(error("Rollback name collision"));
        }
        fs::rename(partial, rollback)?;
    }
    println!("[3/3] Restoring files...");
    let mut extract = tar();
    extract
        .arg("-xzpvf")
        .arg(&archive)
        .args(["--mac-metadata", "--xattrs", "--no-same-owner", "-C"])
        .arg(home);
    if safe_writes {
        extract.arg("--safe-writes");
    }
    for path in &plan.unchanged_links {
        extract.arg(policy::tar_exclusion(path));
    }
    process::tar_progress(
        &mut extract,
        "Restoring files",
        Some((index.0.len() - plan.unchanged_links.len()) as u64),
        process::TarMode::Extract,
    )?;
    println!(
        "DONE. Metadata: {}\nApps and system settings were not installed or changed.",
        home.join(INFO_DIR).join("metadata").display()
    );
    Ok(())
}

fn tar() -> Command {
    Command::new("/usr/bin/tar")
}

fn tar_supports(option: &str) -> Result<bool> {
    check_cancel()?;
    let status = tar()
        .args([option, "--help"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    match status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(error(format!(
            "Cannot inspect system tar capabilities: {status}"
        ))),
    }
}

fn private_file(path: &Path) -> Result<File> {
    Ok(OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?)
}

fn suffix(path: &Path, text: &str) -> PathBuf {
    let mut name: OsString = path.as_os_str().to_owned();
    name.push(text);
    name.into()
}

fn stamp() -> Result<String> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?;
    let date = String::from_utf8(process::output(
        Command::new("/bin/date").arg("+%Y-%m-%d_%H-%M-%S"),
    )?)?;
    Ok(format!(
        "{}-{}-{}",
        date.trim(),
        std::process::id(),
        now.subsec_nanos()
    ))
}

fn checksum(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let progress = Progress::new("SHA-256", Some(file.metadata()?.len()), Unit::Bytes);
    let mut read = 0;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 1024 * 1024];
    loop {
        check_cancel()?;
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
        read += n as u64;
        progress.set(read);
    }
    progress.finish(true);
    Ok(format!("{:x}", hash.finalize()))
}

fn verify(archive: &Path) -> Result<()> {
    let sidecar = suffix(archive, ".sha256");
    let mut text = String::new();
    File::open(sidecar)?.take(4096).read_to_string(&mut text)?;
    let expected = text
        .split_whitespace()
        .next()
        .ok_or_else(|| error("Empty checksum file"))?;
    if expected.len() != 64 || !expected.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(error("Invalid checksum format"));
    }
    if checksum(archive)? != expected.to_ascii_lowercase() {
        return Err(error("SHA-256 mismatch. Restore aborted"));
    }
    Ok(())
}

pub(crate) fn archive_paths(listing: &str) -> Result<Vec<PathBuf>> {
    let mut paths = BTreeSet::new();
    for line in listing.lines() {
        let path = Path::new(line.trim_end_matches('/'));
        policy::validate(path)?;
        let normalized: PathBuf = path
            .components()
            .filter(|p| !matches!(p, std::path::Component::CurDir))
            .collect();
        if normalized.as_os_str().is_empty() || normalized.starts_with(OVERWRITTEN_DIR) {
            return Err(error(format!("Unsafe archive entry: {line:?}")));
        }
        if !paths.insert(normalized) {
            return Err(error(format!("Duplicate archive entry: {line:?}")));
        }
    }
    if paths.is_empty() {
        return Err(error("Empty archive"));
    }
    Ok(paths.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[cfg(target_os = "macos")]
    fn native_tar_capability_probe_distinguishes_supported_and_unknown_options() {
        assert!(tar_supports("--no-recursion").unwrap());
        assert!(!tar_supports("--mac-cpy-nonexistent-option").unwrap());
    }
    #[test]
    fn parses_safe_entries() {
        let paths =
            archive_paths("./Documents/\n./Documents/a\n.mac_cpy_restore/FORMAT\n").unwrap();
        assert!(paths.contains(&PathBuf::from("Documents/a")));
        for s in [
            "/etc/hosts\n",
            "../outside\n",
            "./a/../../b\n",
            "./Documents/a\nDocuments/a\n",
            ".mac_cpy_overwritten/old\n",
            "",
        ] {
            assert!(archive_paths(s).is_err());
        }
    }
    #[test]
    fn sha256_known_value_and_mismatch() {
        let t = tempfile::tempdir().unwrap();
        let file = t.path().join("test");
        fs::write(&file, b"abc").unwrap();
        let digest = checksum(&file).unwrap();
        assert_eq!(
            digest,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        fs::write(
            suffix(&file, ".sha256"),
            format!("{digest}  ignored-name\n"),
        )
        .unwrap();
        verify(&file).unwrap();
        fs::write(&file, b"abcd").unwrap();
        assert!(verify(&file).is_err());
    }
}
