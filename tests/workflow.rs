#![cfg(target_os = "macos")]
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::{Command, Output},
};

#[test]
fn finalize_verifies_preserves_bytes_and_refuses_collisions_or_corruption() {
    let t = tempfile::tempdir().unwrap();
    let stage = t.path().join("stage");
    write(
        stage.join(".mac_cpy_restore/FORMAT"),
        b"macos-copy-archive-v2\n",
    );
    for name in [
        "mac_cpy",
        "exclude.mac_cpy",
        "root_exclude.mac_cpy",
        "include.mac_cpy",
    ] {
        write(stage.join(".mac_cpy_restore").join(name), b"fixture");
    }
    write(stage.join("Documents/test.txt"), b"original backup data");
    let partial = t.path().join("backup.tar.gz.partial");
    assert!(
        Command::new("/usr/bin/tar")
            .arg("-czf")
            .arg(&partial)
            .arg("-C")
            .arg(&stage)
            .args([".mac_cpy_restore", "Documents"])
            .status()
            .unwrap()
            .success()
    );
    let original = fs::read(&partial).unwrap();
    let archive = t.path().join("backup.tar.gz");
    let sidecar = t.path().join("backup.tar.gz.sha256");
    assert_ok(&invoke(
        &[
            "--finalize".as_ref(),
            partial.as_os_str(),
            "--dry-run".as_ref(),
        ],
        None,
    ));
    assert_eq!(fs::read(&partial).unwrap(), original);
    assert!(!archive.exists() && !sidecar.exists());
    fs::write(&sidecar, b"existing checksum").unwrap();
    assert!(
        !invoke(&["--finalize".as_ref(), partial.as_os_str()], None)
            .status
            .success()
    );
    assert_eq!(fs::read(&sidecar).unwrap(), b"existing checksum");
    fs::remove_file(&sidecar).unwrap();
    let bad = t.path().join("corrupt.tar.gz.partial");
    fs::write(&bad, &original[..original.len() / 2]).unwrap();
    assert!(
        !invoke(&["--finalize".as_ref(), bad.as_os_str()], None)
            .status
            .success()
    );
    assert!(bad.exists());
    assert!(!t.path().join("corrupt.tar.gz.sha256").exists());
    assert_ok(&invoke(&["--finalize".as_ref(), partial.as_os_str()], None));
    assert!(!partial.exists());
    assert_eq!(fs::read(&archive).unwrap(), original);
    assert!(sidecar.is_file());
    let home = t.path().join("restore-home");
    fs::create_dir(&home).unwrap();
    assert_ok(&invoke(
        &[
            "-r".as_ref(),
            archive.as_os_str(),
            "--home".as_ref(),
            home.as_os_str(),
        ],
        None,
    ));
    assert_eq!(
        fs::read(home.join("Documents/test.txt")).unwrap(),
        b"original backup data"
    );
}

#[test]
fn metadata_only_archives_manifests_without_scanning_or_copying_home_files() {
    let t = tempfile::tempdir().unwrap();
    let home = t.path().join("home");
    let destination = t.path().join("destination");
    let bin = t.path().join("commands");
    fs::create_dir(&destination).unwrap();
    write(
        home.join("Documents/private.txt"),
        b"must never enter metadata archive",
    );
    // A normal home scan rejects this filename; metadata-only must not scan it.
    write(home.join("bad\nfilename"), b"not selected");
    write(
        home.join("Applications/Manual.app/Contents/example"),
        b"app binary not selected",
    );
    write(bin.join("brew"), br#"#!/bin/sh
case "$1" in
bundle) for a in "$@"; do case "$a" in --file=*) printf 'brew "git"\n' > "${a#--file=}";; esac; done;;
list) printf 'git 2.0\n';;
info) printf '{"casks":[]}\n';;
*) exit 2;;
esac
"#);
    fs::set_permissions(bin.join("brew"), fs::Permissions::from_mode(0o700)).unwrap();
    let before = tree_snapshot(&home);
    let args = [
        destination.as_os_str(),
        "--home".as_ref(),
        home.as_os_str(),
        "--metadata-only".as_ref(),
    ];
    let mut dry_args = args.to_vec();
    dry_args.push("--dry-run".as_ref());
    let preview = invoke(&dry_args, Some(&bin));
    assert_ok(&preview);
    assert!(String::from_utf8_lossy(&preview.stdout).contains("METADATA ONLY"));
    assert!(files(&destination).is_empty());
    assert_ok(&invoke(&args, Some(&bin)));
    assert_eq!(tree_snapshot(&home), before);
    let archive = files(&destination)
        .into_iter()
        .find(|p| p.extension().is_some_and(|v| v == "gz"))
        .unwrap();
    let result = Command::new("/usr/bin/tar")
        .arg("-tzf")
        .arg(&archive)
        .output()
        .unwrap();
    assert_ok(&result);
    let listing = String::from_utf8(result.stdout).unwrap();
    assert!(
        listing
            .lines()
            .all(|p| p.trim_start_matches("./").starts_with(".mac_cpy_restore")),
        "{listing}"
    );
    for name in [
        "MODE",
        "mac_cpy",
        "exclude.mac_cpy",
        "metadata/Brewfile",
        "metadata/Homebrew-versions.txt",
        "metadata/Install-commands.txt",
        "metadata/Applications-not-in-Brew.txt",
        "metadata/hosts",
        "metadata/source.txt",
        "metadata/macos.txt",
    ] {
        assert!(
            listing.contains(&format!(".mac_cpy_restore/{name}")),
            "{name}"
        );
    }
    let restored = t.path().join("restored");
    fs::create_dir(&restored).unwrap();
    assert_ok(&invoke(
        &[
            "-r".as_ref(),
            archive.as_os_str(),
            "--home".as_ref(),
            restored.as_os_str(),
        ],
        None,
    ));
    assert_eq!(files(&restored), vec![restored.join(".mac_cpy_restore")]);
    assert_eq!(
        fs::read(restored.join(".mac_cpy_restore/MODE")).unwrap(),
        b"metadata-only\n"
    );
    assert_eq!(
        fs::read(restored.join(".mac_cpy_restore/metadata/Brewfile")).unwrap(),
        b"brew \"git\"\n"
    );
    let instructions =
        fs::read_to_string(restored.join(".mac_cpy_restore/metadata/Install-commands.txt"))
            .unwrap();
    assert!(instructions.contains("https://brew.sh/"));
    assert!(!instructions.contains("https://sh.rustup.rs"));
}

#[test]
fn gitignore_directive_is_scoped_and_respected_by_scan_estimates_and_tar() {
    fn data_bytes(output: &str) -> u64 {
        output
            .lines()
            .find(|line| line.starts_with("  Data:"))
            .unwrap()
            .rsplit("; ")
            .next()
            .unwrap()
            .trim_end_matches(" bytes)")
            .parse()
            .unwrap()
    }
    let t = tempfile::tempdir().unwrap();
    let home = t.path().join("home");
    let destination = t.path().join("destination");
    let install = t.path().join("install");
    fs::create_dir(&destination).unwrap();
    fs::create_dir(&install).unwrap();
    let exe = install.join("mac_cpy");
    fs::copy(env!("CARGO_BIN_EXE_mac_cpy"), &exe).unwrap();
    write(
        install.join("exclude.mac_cpy"),
        b"@GITIGNORE\nproject/manual\nproject/forced\n**/*.secret\nnode_modules\n",
    );
    write(install.join("root_exclude.mac_cpy"), b"Library\nPrivate\n");
    write(
        install.join("include.mac_cpy"),
        b"project/blocked/keep.txt\nPrivate/*.json\nproject/manual/result.txt\nproject/forced\nproject/*.secret\n",
    );
    write(
        home.join("project/.gitignore"),
        b"target/\n*.log\n/build/\nblocked/\n!blocked/keep.txt\nodd\\[1\\].tmp\n",
    );
    write(home.join("project/sub/.gitignore"), b"!keep.log\n/cache/\n");
    let excluded = [
        "project/target/classes/A.class",
        "project/sub/target/A.class",
        "project/drop.log",
        "project/build/output",
        "project/blocked/drop.txt",
        "project/sub/cache/output",
        "project/odd[1].tmp",
        "Private/drop.txt",
        "project/manual/drop.txt",
        "other/private.secret",
        "node_modules/drop.txt",
    ];
    for name in excluded {
        write(home.join(name), vec![b'x'; 1024 * 1024]);
    }
    let included = [
        "other/target/important.txt",
        "other/keep.log",
        "project/sub/keep.log",
        "project/sub/build/keep",
        "project/ordinary/target",
        "project/odd1.tmp",
        "project/blocked/keep.txt",
        "Private/settings.json",
        "project/manual/result.txt",
        "project/forced/cache/result.bin",
        "project/important.secret",
        "other/node_modules/keep.txt",
        "project/.gitignore",
        "project/sub/.gitignore",
    ];
    for name in &included[..12] {
        write(home.join(name), b"keep");
    }
    write(home.join("project/forced/.gitignore"), b"*\n");
    write(t.path().join("external-ignore"), b"target/\n");
    write(home.join("linked-ignore/target/keep"), b"keep");
    symlink(
        t.path().join("external-ignore"),
        home.join("linked-ignore/.gitignore"),
    )
    .unwrap();
    write(install.join("brew"), br#"#!/bin/sh
case "$1" in
bundle) for a in "$@"; do case "$a" in --file=*) printf 'brew "git"\n' > "${a#--file=}";; esac; done;;
list) printf 'git 2.0\n';;
info) printf '{"casks":[]}\n';;
*) exit 2;;
esac
"#);
    fs::set_permissions(install.join("brew"), fs::Permissions::from_mode(0o700)).unwrap();
    let args = [destination.as_os_str(), "--home".as_ref(), home.as_os_str()];
    for estimate in [false, true] {
        let mut dry = args.to_vec();
        dry.push("--dry-run".as_ref());
        if estimate {
            dry.push("--estimate".as_ref());
        }
        let result = invoke_at(&exe, &dry, Some(&install));
        assert_ok(&result);
        assert!(String::from_utf8_lossy(&result.stdout).contains("GITIGNORE EXCLUSIONS"));
        if estimate {
            assert!(data_bytes(&String::from_utf8_lossy(&result.stdout)) < 1024 * 1024);
        }
        assert!(files(&destination).is_empty());
    }
    let before = tree_snapshot(&home);
    assert_ok(&invoke_at(&exe, &args, Some(&install)));
    assert_eq!(tree_snapshot(&home), before);
    let archive = files(&destination)
        .into_iter()
        .find(|p| p.extension().is_some_and(|s| s == "gz"))
        .unwrap();
    let listing = Command::new("/usr/bin/tar")
        .arg("-tzf")
        .arg(&archive)
        .output()
        .unwrap();
    assert_ok(&listing);
    let text = String::from_utf8(listing.stdout).unwrap();
    let paths: Vec<_> = text.lines().map(|p| p.trim_start_matches("./")).collect();
    for name in included {
        assert!(paths.contains(&name), "missing {name}: {text}");
    }
    assert!(paths.contains(&"linked-ignore/target/keep"));
    for name in excluded {
        assert!(!paths.contains(&name), "included {name}: {text}");
    }
    assert!(text.contains("metadata/gitignored-paths.txt"));
    // Removing the directive disables .gitignore filtering without rebuilding.
    write(install.join("exclude.mac_cpy"), b"");
    let mut dry = args.to_vec();
    dry.push("--dry-run".as_ref());
    dry.push("--estimate".as_ref());
    let result = invoke_at(&exe, &dry, Some(&install));
    assert_ok(&result);
    let output = String::from_utf8_lossy(&result.stdout);
    assert!(!output.contains("GITIGNORE EXCLUSIONS"));
    assert!(output.contains("ESTIMATED SIZE"));
    assert!(data_bytes(&output) >= 7 * 1024 * 1024);
}

#[test]
fn interrupted_estimate_removes_its_destination_file() {
    use std::{
        thread,
        time::{Duration, Instant},
    };
    let t = tempfile::tempdir().unwrap();
    let home = t.path().join("home");
    let dst = t.path().join("destination");
    fs::create_dir(&home).unwrap();
    fs::create_dir(&dst).unwrap();
    let data = vec![b'x'; 8 * 1024 * 1024];
    for i in 0..8 {
        write(home.join(format!("sample-{i}.bin")), &data);
    }
    let mut child = Command::new(env!("CARGO_BIN_EXE_mac_cpy"))
        .env("XDG_CONFIG_HOME", t.path().join("user-config"))
        .arg(&dst)
        .args(["--home"])
        .arg(&home)
        .args(["--dry-run", "--estimate"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut observed = false;
    while Instant::now() < deadline {
        if files(&dst).iter().any(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".mac-cpy-speed-")
        }) {
            observed = true;
            // Signal only this test child, never a user process.
            assert_eq!(
                unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) },
                0
            );
            break;
        }
        if child.try_wait().unwrap().is_some() {
            break;
        }
        thread::sleep(Duration::from_millis(1));
    }
    if !observed {
        let _ = child.kill();
        let _ = child.wait();
        panic!("Benchmark temp file not observed");
    }
    assert_eq!(child.wait().unwrap().code(), Some(130));
    assert!(files(&dst).is_empty());
    assert!(home.join("sample-0.bin").is_file());
}

fn invoke(args: &[&std::ffi::OsStr], mock_bin: Option<&Path>) -> Output {
    invoke_at(Path::new(env!("CARGO_BIN_EXE_mac_cpy")), args, mock_bin)
}
fn invoke_at(executable: &Path, args: &[&std::ffi::OsStr], mock_bin: Option<&Path>) -> Output {
    let mut c = Command::new(executable);
    c.env(
        "XDG_CONFIG_HOME",
        executable.parent().unwrap().join("unused-test-user-config"),
    );
    c.args(args);
    if let Some(bin) = mock_bin {
        let mut path = bin.as_os_str().to_os_string();
        path.push(":");
        path.push(std::env::var_os("PATH").unwrap_or_default());
        c.env("PATH", path);
    }
    c.output().unwrap()
}
fn assert_ok(out: &Output) {
    assert!(
        out.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
fn write(path: impl AsRef<Path>, bytes: impl AsRef<[u8]>) {
    let path = path.as_ref();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}
fn files(path: &Path) -> Vec<PathBuf> {
    fs::read_dir(path)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect()
}

fn small_archive(stage: &Path, destination: &Path) -> PathBuf {
    write(
        stage.join(".mac_cpy_restore/FORMAT"),
        b"macos-copy-archive-v2\n",
    );
    for name in [
        "mac_cpy",
        "exclude.mac_cpy",
        "root_exclude.mac_cpy",
        "include.mac_cpy",
    ] {
        write(stage.join(".mac_cpy_restore").join(name), b"fixture");
    }
    let partial = destination.join("backup.tar.gz.partial");
    assert!(
        Command::new("/usr/bin/tar")
            .arg("-czf")
            .arg(&partial)
            .arg("-C")
            .arg(stage)
            .args([".mac_cpy_restore", "Documents"])
            .status()
            .unwrap()
            .success()
    );
    assert_ok(&invoke(&["--finalize".as_ref(), partial.as_os_str()], None));
    destination.join("backup.tar.gz")
}

fn tree_snapshot(root: &Path) -> std::collections::BTreeMap<PathBuf, (u32, Vec<u8>)> {
    fn visit(
        root: &Path,
        path: &Path,
        out: &mut std::collections::BTreeMap<PathBuf, (u32, Vec<u8>)>,
    ) {
        let metadata = fs::symlink_metadata(path).unwrap();
        let data = if metadata.file_type().is_symlink() {
            fs::read_link(path)
                .unwrap()
                .as_os_str()
                .as_encoded_bytes()
                .to_vec()
        } else if metadata.is_file() {
            fs::read(path).unwrap()
        } else {
            Vec::new()
        };
        out.insert(
            path.strip_prefix(root).unwrap().into(),
            (metadata.permissions().mode(), data),
        );
        if metadata.is_dir() {
            for child in fs::read_dir(path).unwrap() {
                visit(root, &child.unwrap().path(), out);
            }
        }
    }
    let mut result = std::collections::BTreeMap::new();
    visit(root, root, &mut result);
    result
}

#[test]
fn restore_from_home_preserves_its_archive_and_checksum() {
    let t = tempfile::tempdir().unwrap();
    let stage = t.path().join("stage");
    let home = t.path().join("home");
    write(stage.join("Documents/sample.txt"), b"restored data");
    write(
        stage.join("Documents/Backups/other.txt"),
        b"other restored file",
    );
    write(home.join("Documents/sample.txt"), b"previous data");
    let destination = home.join("Documents/Backups");
    fs::create_dir(&destination).unwrap();
    let archive = small_archive(&stage, &destination);
    let checksum = destination.join("backup.tar.gz.sha256");
    let bytes = fs::read(&archive).unwrap();
    let checksum_bytes = fs::read(&checksum).unwrap();
    let before = tree_snapshot(&home);
    assert_ok(&invoke(
        &[
            "-r".as_ref(),
            archive.as_os_str(),
            "--home".as_ref(),
            home.as_os_str(),
            "--dry-run".as_ref(),
        ],
        None,
    ));
    assert_eq!(tree_snapshot(&home), before);
    let alias = t.path().join("archive-alias.tar.gz");
    symlink(&archive, &alias).unwrap();
    assert_ok(&invoke(
        &[
            "-r".as_ref(),
            alias.as_os_str(),
            "--home".as_ref(),
            home.as_os_str(),
        ],
        None,
    ));
    assert_eq!(
        fs::read(home.join("Documents/sample.txt")).unwrap(),
        b"restored data"
    );
    assert_eq!(
        fs::read(destination.join("other.txt")).unwrap(),
        b"other restored file"
    );
    assert_eq!(fs::read(&archive).unwrap(), bytes);
    assert_eq!(fs::read(&checksum).unwrap(), checksum_bytes);
    assert!(home.join(".mac_cpy_overwritten").is_dir());
}

#[test]
fn restore_rejects_source_collisions_before_any_changes() {
    let t = tempfile::tempdir().unwrap();
    for mode in [
        "archive",
        "checksum",
        "parent-file",
        "parent-link",
        "hardlink",
        "checksum-link-target",
    ] {
        let case = t.path().join(mode);
        let stage = case.join("stage");
        let home = case.join("home");
        write(stage.join("Documents/a-first"), b"new data");
        write(home.join("Documents/a-first"), b"original data");
        match mode {
            "archive" => write(
                stage.join("Documents/Backups/backup.tar.gz"),
                b"old archive",
            ),
            "checksum" => write(
                stage.join("Documents/Backups/backup.tar.gz.sha256"),
                b"old checksum",
            ),
            "parent-file" => write(
                stage.join("Documents/Backups"),
                b"would replace archive folder",
            ),
            "parent-link" => symlink("elsewhere", stage.join("Documents/Backups")).unwrap(),
            "hardlink" => write(
                stage.join("Documents/archive-alias"),
                b"would replace a hard link",
            ),
            _ => write(
                stage.join("Documents/checksum-target"),
                b"would replace checksum target",
            ),
        }
        let external_archive = small_archive(&stage, &case);
        let folder = home.join("Documents/Backups");
        fs::create_dir(&folder).unwrap();
        let archive = folder.join("backup.tar.gz");
        let checksum = folder.join("backup.tar.gz.sha256");
        fs::copy(external_archive, &archive).unwrap();
        fs::copy(case.join("backup.tar.gz.sha256"), &checksum).unwrap();
        if mode == "hardlink" {
            fs::hard_link(&archive, home.join("Documents/archive-alias")).unwrap();
        } else if mode == "checksum-link-target" {
            let target = home.join("Documents/checksum-target");
            fs::rename(&checksum, &target).unwrap();
            symlink(target, &checksum).unwrap();
        }
        let before = tree_snapshot(&home);
        for dry in [true, false] {
            let mut args = vec![
                "-r".as_ref(),
                archive.as_os_str(),
                "--home".as_ref(),
                home.as_os_str(),
            ];
            if dry {
                args.push("--dry-run".as_ref());
            }
            let result = invoke(&args, None);
            assert!(!result.status.success(), "{mode}");
            assert!(
                String::from_utf8_lossy(&result.stderr).contains("source archive or checksum"),
                "{mode}: {}",
                String::from_utf8_lossy(&result.stderr)
            );
            assert_eq!(tree_snapshot(&home), before, "{mode}");
        }
    }
}

#[test]
fn leaf_links_are_replaced_without_following_and_unchanged_links_are_kept() {
    use std::os::unix::fs::MetadataExt;
    let t = tempfile::tempdir().unwrap();
    let stage = t.path().join("stage");
    let home = t.path().join("home");
    write(stage.join("Documents/plain"), b"restored file");
    write(
        stage.join("Documents/nested/same[1]"),
        b"must not be excluded",
    );
    for name in ["changed", "file-to-link"] {
        symlink("plain", stage.join("Documents").join(name)).unwrap();
    }
    let odd_target = "missing\\target\nnext";
    symlink(odd_target, stage.join("Documents/same[1]")).unwrap();
    let external = t.path().join("external");
    write(external.join("sentinel"), b"external data");
    fs::create_dir_all(home.join("Documents")).unwrap();
    symlink(external.join("sentinel"), home.join("Documents/plain")).unwrap();
    symlink(&external, home.join("Documents/changed")).unwrap();
    write(home.join("Documents/file-to-link"), b"old regular file");
    symlink(odd_target, home.join("Documents/same[1]")).unwrap();
    let same_inode = fs::symlink_metadata(home.join("Documents/same[1]"))
        .unwrap()
        .ino();
    let archive = small_archive(&stage, t.path());
    assert_ok(&invoke(
        &[
            "-r".as_ref(),
            archive.as_os_str(),
            "--home".as_ref(),
            home.as_os_str(),
        ],
        None,
    ));
    assert_eq!(
        fs::read(external.join("sentinel")).unwrap(),
        b"external data"
    );
    assert_eq!(
        fs::read(home.join("Documents/plain")).unwrap(),
        b"restored file"
    );
    assert_eq!(
        fs::read(home.join("Documents/nested/same[1]")).unwrap(),
        b"must not be excluded"
    );
    for name in ["changed", "file-to-link"] {
        assert_eq!(
            fs::read_link(home.join("Documents").join(name)).unwrap(),
            Path::new("plain")
        );
    }
    assert_eq!(
        fs::symlink_metadata(home.join("Documents/same[1]"))
            .unwrap()
            .ino(),
        same_inode
    );
    let rollback = files(&home.join(".mac_cpy_overwritten")).pop().unwrap();
    let old = t.path().join("old");
    fs::create_dir(&old).unwrap();
    assert!(
        Command::new("/usr/bin/tar")
            .arg("-xzf")
            .arg(rollback)
            .arg("-C")
            .arg(&old)
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(
        fs::read_link(old.join("Documents/plain")).unwrap(),
        external.join("sentinel")
    );
    assert_eq!(
        fs::read_link(old.join("Documents/changed")).unwrap(),
        external
    );
    assert_eq!(
        fs::read(old.join("Documents/file-to-link")).unwrap(),
        b"old regular file"
    );
    assert!(!old.join("Documents/same[1]").try_exists().unwrap());
    assert!(fs::symlink_metadata(old.join("Documents/same[1]")).is_err());
}

#[test]
fn backup_inside_home_excludes_only_current_outputs_even_through_a_symlink() {
    let t = tempfile::tempdir().unwrap();
    let bin = t.path().join("bin");
    write(bin.join("brew"), br#"#!/bin/sh
case "$1" in
  bundle) for a in "$@"; do case "$a" in --file=*) printf 'brew "git"\n' > "${a#--file=}";; esac; done;;
  list) printf 'git 2.0\n';;
  info) printf '{"casks":[]}\n';;
  *) exit 2;;
esac
"#);
    fs::set_permissions(bin.join("brew"), fs::Permissions::from_mode(0o700)).unwrap();
    for mode in ["root", "nested", "alias"] {
        let home = t.path().join(mode);
        write(home.join("Documents/sample"), vec![b'x'; 1024 * 1024]);
        let destination = if mode == "root" {
            home.clone()
        } else {
            home.join("Documents/backup [1]")
        };
        fs::create_dir_all(&destination).unwrap();
        let previous = [
            "previous-backup.tar.gz",
            "previous-backup.tar.gz.partial",
            "previous-backup.tar.gz.sha256",
        ];
        for name in previous {
            write(destination.join(name), b"previous backup output");
        }
        let alias = t.path().join(format!("destination-{mode}"));
        symlink(&destination, &alias).unwrap();
        let argument = if mode == "alias" {
            &alias
        } else {
            &destination
        };
        let before = tree_snapshot(&home);
        let dry = invoke(
            &[
                argument.as_os_str(),
                "--home".as_ref(),
                home.as_os_str(),
                "--dry-run".as_ref(),
                "-e".as_ref(),
            ],
            Some(&bin),
        );
        assert_ok(&dry);
        assert!(String::from_utf8_lossy(&dry.stdout).contains("EXCLUDED FOR THIS RUN"));
        assert_eq!(tree_snapshot(&home), before);
        assert_ok(&invoke(
            &[argument.as_os_str(), "--home".as_ref(), home.as_os_str()],
            Some(&bin),
        ));
        let archive = files(&destination)
            .into_iter()
            .find(|p| {
                let name = p.file_name().unwrap().to_string_lossy();
                name.starts_with("backup-") && name.ends_with(".tar.gz")
            })
            .unwrap();
        let output = Command::new("/usr/bin/tar")
            .arg("-tzf")
            .arg(&archive)
            .output()
            .unwrap();
        assert_ok(&output);
        let listing = String::from_utf8(output.stdout).unwrap();
        let paths: Vec<_> = listing
            .lines()
            .map(|p| p.trim_start_matches("./"))
            .collect();
        for name in previous {
            let relative = destination.strip_prefix(&home).unwrap().join(name);
            assert!(
                paths.contains(&relative.to_str().unwrap()),
                "{mode}: {listing}"
            );
            assert_eq!(
                fs::read(destination.join(name)).unwrap(),
                b"previous backup output"
            );
        }
        assert!(paths.contains(&"Documents/sample"));
        let current = archive.strip_prefix(&home).unwrap().to_str().unwrap();
        for ending in ["", ".partial", ".sha256"] {
            assert!(
                !paths.contains(&format!("{current}{ending}").as_str()),
                "{mode}: self-inclusion"
            );
        }
        assert!(
            archive
                .with_file_name(format!(
                    "{}.sha256",
                    archive.file_name().unwrap().to_str().unwrap()
                ))
                .exists()
        );
    }
}

#[test]
fn destination_conflicts_abort_before_creating_any_restore_files() {
    let t = tempfile::tempdir().unwrap();
    let stage = t.path().join("stage");
    write(stage.join("Documents/a-first"), b"new first file");
    write(stage.join("Documents/z-last/config"), b"new config");
    let archive = small_archive(&stage, t.path());
    for conflict in ["parent-link", "parent-file", "leaf-directory"] {
        let home = t.path().join(conflict);
        write(home.join("Documents/a-first"), b"original first file");
        let external = t.path().join(format!("external-{conflict}"));
        write(external.join("config"), b"external original");
        match conflict {
            "parent-link" => symlink(&external, home.join("Documents/z-last")).unwrap(),
            "parent-file" => write(home.join("Documents/z-last"), b"original parent file"),
            _ => {
                write(
                    home.join("Documents/z-last/config/keep"),
                    b"original directory data",
                );
            }
        }
        let before = tree_snapshot(&home);
        for dry in [true, false] {
            let mut args = vec![
                "-r".as_ref(),
                archive.as_os_str(),
                "--home".as_ref(),
                home.as_os_str(),
            ];
            if dry {
                args.push("--dry-run".as_ref());
            }
            let result = invoke(&args, None);
            assert!(!result.status.success(), "{conflict}");
            assert_eq!(
                fs::read(home.join("Documents/a-first")).unwrap(),
                b"original first file"
            );
            assert_eq!(
                fs::read(external.join("config")).unwrap(),
                b"external original"
            );
            assert_eq!(files(&home), vec![home.join("Documents")]);
            assert!(!home.join(".mac_cpy_overwritten").exists());
            assert!(!home.join(".mac_cpy_restore").exists());
            assert_eq!(tree_snapshot(&home), before);
        }
    }
}

#[test]
fn terminal_progress_redraws_and_keeps_scan_warnings_above_the_block() {
    use std::process::Stdio;
    struct RestorePermissions(PathBuf);
    impl Drop for RestorePermissions {
        fn drop(&mut self) {
            let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o700));
        }
    }
    let t = tempfile::tempdir().unwrap();
    let home = t.path().join("home");
    let dst = t.path().join("destination");
    fs::create_dir(&home).unwrap();
    fs::create_dir(&dst).unwrap();
    write(
        home.join("Documents/sample.bin"),
        vec![b'x'; 2 * 1024 * 1024],
    );
    write(home.join(".config/htop/config"), b"not readable");
    let denied = home.join(".config/htop");
    let _permissions = RestorePermissions(denied.clone());
    fs::set_permissions(denied, fs::Permissions::from_mode(0o000)).unwrap();
    let transcript = t.path().join("terminal.log");
    let out = Command::new("/usr/bin/script")
        .env("XDG_CONFIG_HOME", t.path().join("user-config"))
        .args(["-q"])
        .arg(&transcript)
        .arg(env!("CARGO_BIN_EXE_mac_cpy"))
        .arg(&dst)
        .arg("--home")
        .arg(&home)
        .args(["--dry-run", "--estimate"])
        .env("TERM", "xterm-256color")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_ok(&out);
    let transcript = fs::read_to_string(transcript).unwrap();
    assert!(
        transcript.contains("\x1b[9A\r\x1b[JWARNING: Skipping"),
        "warning must clear the old block before logging: {transcript:?}"
    );
    let after_warning = transcript.split("WARNING: Skipping").nth(1).unwrap();
    assert!(after_warning.contains("\x1b[1mStage: Scanning\x1b[0m"));
    assert!(after_warning.contains("Files/dirs:"));
    assert!(
        transcript.contains("Progress:")
            && transcript.contains("Elapsed:")
            && transcript.contains("ETA (stage):")
    );
    assert!(transcript.contains("SKIPPED PATHS (1)"));
    assert!(files(&dst).is_empty());
}

#[test]
fn inaccessible_paths_are_logged_excluded_and_reported_without_aborting() {
    struct RestorePermissions(Vec<PathBuf>);
    impl Drop for RestorePermissions {
        fn drop(&mut self) {
            for path in &self.0 {
                let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o700));
            }
        }
    }
    let t = tempfile::tempdir().unwrap();
    let home = t.path().join("home");
    let dst = t.path().join("destination");
    let bin = t.path().join("bin");
    for path in [&home, &dst, &bin] {
        fs::create_dir(path).unwrap();
    }
    let home = fs::canonicalize(home).unwrap();
    write(home.join("Documents/keep.txt"), b"keep");
    write(home.join("Documents/blocked1.txt"), b"not a wildcard match");
    write(home.join("Documents/no-read.txt"), b"skip");
    write(home.join("Documents/blocked[1].txt"), b"skip");
    write(home.join(".config/htop/config"), b"skip subtree");
    write(home.join("RootBlocked/config"), b"skip root");
    let denied: Vec<_> = [
        "Documents/no-read.txt",
        "Documents/blocked[1].txt",
        ".config/htop",
        "RootBlocked",
    ]
    .into_iter()
    .map(|p| home.join(p))
    .collect();
    let _permissions = RestorePermissions(denied.clone());
    for path in &denied {
        fs::set_permissions(path, fs::Permissions::from_mode(0o000)).unwrap();
    }
    write(bin.join("brew"),br#"#!/bin/sh
case "$1" in
bundle) for a in "$@"; do case "$a" in --file=*) printf 'brew "git"\n' > "${a#--file=}";; esac; done;;
list) printf 'git 2.0\n';;
info) printf '{"casks":[]}\n';;
*) exit 2;;
esac
"#);
    fs::set_permissions(bin.join("brew"), fs::Permissions::from_mode(0o700)).unwrap();
    let out = invoke(
        &[dst.as_os_str(), "--home".as_ref(), home.as_os_str()],
        Some(&bin),
    );
    assert_ok(&out);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stdout.contains("DONE WITH SKIPS"));
    let summary = stdout
        .split("SKIPPED PATHS (4)")
        .nth(1)
        .expect("final list of four skipped paths");
    for path in &denied {
        assert!(
            stderr.contains(&format!("WARNING: Skipping {}:", path.display())),
            "expected path {} in stderr: {stderr}",
            path.display()
        );
        assert!(summary.contains(&path.display().to_string()));
    }
    let archive = files(&dst)
        .into_iter()
        .find(|p| p.to_string_lossy().ends_with(".tar.gz"))
        .unwrap();
    let listing = Command::new("/usr/bin/tar")
        .arg("-tzf")
        .arg(&archive)
        .output()
        .unwrap();
    assert_ok(&listing);
    let listing = String::from_utf8_lossy(&listing.stdout);
    for path in [
        "Documents/no-read.txt",
        "Documents/blocked[1].txt",
        ".config/htop",
        "RootBlocked",
    ] {
        assert!(!listing.contains(path), "unexpected archived path: {path}");
    }
    assert!(listing.contains("Documents/blocked1.txt"));
    assert!(listing.contains("Documents/keep.txt"));
    let report = Command::new("/usr/bin/tar")
        .arg("-xzOf")
        .arg(&archive)
        .arg(".mac_cpy_restore/metadata/skipped-paths.txt")
        .output()
        .unwrap();
    assert_ok(&report);
    assert_eq!(
        String::from_utf8_lossy(&report.stdout),
        format!("SKIPPED PATHS (4){summary}")
    );
    // Restore permissions only for fixture cleanup, then prove source data was untouched.
    drop(_permissions);
    assert_eq!(
        fs::read(home.join("Documents/no-read.txt")).unwrap(),
        b"skip"
    );
    assert!(home.join(".config/htop/config").is_file());
}

#[test]
fn selection_matches_current_contract_and_plain_dry_run_has_no_writes() {
    let t = tempfile::tempdir().unwrap();
    let home = t.path().join("home");
    let dst = t.path().join("destination");
    fs::create_dir(&home).unwrap();
    fs::create_dir(&dst).unwrap();
    for rel in [
        "Documents/keep",
        ".config/zed/settings.json",
        ".vscode-oss/extensions/plugin/package.json",
        "Library/Application Support/VSCodium/User/settings.json",
        "Library/Application Support/VSCodium/User/History/drop",
        "Library/Application Support/Zed/drop",
        "Library/DBeaverData/drop",
        "Library/Application Support/JetBrains/IdeaIC2026.2/options/preferences.xml",
        "Library/Application Support/JetBrains/RustRover2040/keymaps/custom.xml",
        "Library/Application Support/JetBrains/RustRover2040/new-settings/custom.bin",
        "Library/Application Support/JetBrains/RustRover2040/plugins/plugin/settings.xml",
        "Library/Application Support/JetBrains/RustRover2040/idea.vmoptions",
        "Library/Application Support/JetBrains/PyCharm2026.3/options/editor.xml",
        "Library/Application Support/JetBrains/PyCharmCE2025.1/options/editor.xml",
        "Library/Application Support/JetBrains/CLion2026.3/options/editor.xml",
        "Library/Application Support/JetBrains/IdeaIC2025.1-backup/options/editor.xml",
        "Library/Application Support/JetBrains/IntelliJIdea2025.1-backup/drop",
        "Library/Application Support/JetBrains/Daemon/drop",
        "Library/Application Support/JetBrains/Toolbox/drop",
        "Library/Caches/JetBrains/RustRover2040/cache.bin",
        "Library/Logs/JetBrains/RustRover2040/idea.log",
        "Games/drop",
    ] {
        write(home.join(rel), b"fixture");
    }
    write(
        home.join("Library/Application Support/Firefox/Profiles/main/prefs.js"),
        b"not included",
    );
    write(
        home.join("Documents/Restore Firefox/FirefoxBackup_Main.html"),
        b"backup fixture",
    );
    let out = invoke(
        &[
            dst.as_os_str(),
            "--home".as_ref(),
            home.as_os_str(),
            "--dry-run".as_ref(),
        ],
        None,
    );
    assert_ok(&out);
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(
        !out.stderr.windows(2).any(|p| p == b"\x1b["),
        "redirected logs must not contain cursor-control sequences"
    );
    let included = text
        .split("INCLUDED PATHS (directories are recursive)\n")
        .nth(1)
        .unwrap()
        .split("\nEXCLUDED ROOTS")
        .next()
        .unwrap();
    for rel in [
        ".config",
        ".vscode-oss/extensions",
        "Documents",
        "Library/Application Support/VSCodium/User/settings.json",
        "Library/Application Support/JetBrains",
    ] {
        assert!(included.lines().any(|l| l == rel), "missing {rel}");
    }
    for excluded in [
        "DBeaver",
        "Support/Firefox",
        "Caches/JetBrains",
        "Logs/JetBrains",
        "Support/Zed",
        "History",
        "places.sqlite",
        "cookies.sqlite",
        "logins.json",
        "cache.bin",
    ] {
        assert!(!included.contains(excluded), "unexpected {excluded}");
    }
    assert!(!text.contains("ESTIMATED SIZE"));
    assert!(files(&dst).is_empty());
    let bad = invoke(&[dst.as_os_str(), "-e".as_ref()], None);
    assert!(!bad.status.success());
}

#[test]
fn real_archive_roundtrip_estimates_rollback_and_corruption() {
    let t = tempfile::tempdir().unwrap();
    let home = t.path().join("home with spaces");
    let dst = t.path().join("destination");
    let bin = t.path().join("bin");
    for p in [&home, &dst, &bin] {
        fs::create_dir(p).unwrap();
    }
    write(home.join("Documents/example.txt"), b"original\n");
    write(
        home.join("Documents/заметки с пробелами.txt"),
        b"unicode filename\n",
    );
    write(
        home.join("Documents/quoted\"name.txt"),
        b"quoted filename\n",
    );
    write(home.join(".config/heroic/drop"), b"excluded");
    write(home.join(".config/zed/settings.json"), b"{}");
    fs::set_permissions(
        home.join("Documents/example.txt"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    symlink("example.txt", home.join("Documents/link")).unwrap();
    assert!(
        Command::new("/usr/bin/xattr")
            .args(["-w", "com.example.mac-cpy-test", "preserved"])
            .arg(home.join("Documents/example.txt"))
            .status()
            .unwrap()
            .success()
    );
    // Incompressible generated test data, never user files.
    let mut state = 42u64;
    let data: Vec<u8> = (0..2 * 1024 * 1024)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect();
    write(home.join("sample.bin"), &data);
    write(bin.join("brew"),br#"#!/bin/sh
case "$1" in
  bundle) for a in "$@"; do case "$a" in --file=*) printf 'brew "git"\n' > "${a#--file=}";; esac; done;;
  list) printf 'git 2.0\n';;
  info) printf '{"casks":[]}\n';;
  *) exit 2;;
esac
"#);
    fs::set_permissions(bin.join("brew"), fs::Permissions::from_mode(0o700)).unwrap();
    let estimate = invoke(
        &[
            dst.as_os_str(),
            "--home".as_ref(),
            home.as_os_str(),
            "--dry-run".as_ref(),
            "--estimate".as_ref(),
        ],
        Some(&bin),
    );
    assert_ok(&estimate);
    let text = String::from_utf8_lossy(&estimate.stdout);
    assert!(text.contains("ESTIMATED SIZE") && text.contains("Archive creation:"));
    assert!(files(&dst).is_empty());
    let backup = invoke(
        &[dst.as_os_str(), "--home".as_ref(), home.as_os_str()],
        Some(&bin),
    );
    assert_ok(&backup);
    let archive = files(&dst)
        .into_iter()
        .find(|p| p.to_string_lossy().ends_with(".tar.gz"))
        .unwrap();
    let list = Command::new("/usr/bin/tar")
        .arg("-tzf")
        .arg(&archive)
        .output()
        .unwrap();
    assert_ok(&list);
    let entries = String::from_utf8_lossy(&list.stdout);
    assert!(!entries.contains("heroic") && !entries.contains("metadata/opt/homebrew/etc"));
    assert!(entries.contains(".mac_cpy_restore/mac_cpy"));
    let progress_log = String::from_utf8_lossy(&backup.stderr);
    assert!(progress_log.contains("Creating archive") && progress_log.contains("Files/dirs:"));
    let creation = progress_log
        .split("Stage: Creating archive")
        .nth(1)
        .unwrap()
        .split("Stage:")
        .next()
        .unwrap();
    assert!(creation.contains("Total size (uncompressed): ~"));
    assert!(creation.contains("Estimated archive: ~"));
    assert!(progress_log.contains("Elapsed:") && progress_log.contains("ETA (stage):"));
    assert!(!progress_log.contains("\x1b["));
    for name in ["exclude.mac_cpy", "root_exclude.mac_cpy", "include.mac_cpy"] {
        assert!(entries.contains(&format!(".mac_cpy_restore/{name}")));
    }
    let restored = t.path().join("restored");
    fs::create_dir(&restored).unwrap();
    let preview = invoke(
        &[
            "--restore".as_ref(),
            archive.as_os_str(),
            "--home".as_ref(),
            restored.as_os_str(),
            "--dry-run".as_ref(),
        ],
        None,
    );
    assert_ok(&preview);
    assert!(files(&restored).is_empty());
    let result = invoke(
        &[
            "-r".as_ref(),
            archive.as_os_str(),
            "--home".as_ref(),
            restored.as_os_str(),
        ],
        None,
    );
    assert_ok(&result);
    assert_eq!(fs::read(restored.join("sample.bin")).unwrap(), data);
    assert_eq!(
        fs::read(restored.join("Documents/заметки с пробелами.txt")).unwrap(),
        b"unicode filename\n"
    );
    assert_eq!(
        fs::read(restored.join("Documents/quoted\"name.txt")).unwrap(),
        b"quoted filename\n"
    );
    assert_eq!(
        fs::read_link(restored.join("Documents/link")).unwrap(),
        Path::new("example.txt")
    );
    assert_eq!(
        fs::metadata(restored.join("Documents/example.txt"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let xattr = Command::new("/usr/bin/xattr")
        .args(["-p", "com.example.mac-cpy-test"])
        .arg(restored.join("Documents/example.txt"))
        .output()
        .unwrap();
    assert_ok(&xattr);
    assert_eq!(xattr.stdout, b"preserved\n");
    let again = invoke(
        &[
            "-r".as_ref(),
            archive.as_os_str(),
            "--home".as_ref(),
            restored.as_os_str(),
        ],
        None,
    );
    assert_ok(&again);
    assert!(String::from_utf8_lossy(&again.stdout).contains("Unchanged symlinks: 1"));
    let overwrite = t.path().join("overwrite");
    fs::create_dir(&overwrite).unwrap();
    write(overwrite.join("Documents/example.txt"), b"previous\n");
    write(overwrite.join("keep.txt"), b"keep");
    assert_ok(&invoke(
        &[
            "-r".as_ref(),
            archive.as_os_str(),
            "--home".as_ref(),
            overwrite.as_os_str(),
        ],
        None,
    ));
    assert_eq!(fs::read(overwrite.join("keep.txt")).unwrap(), b"keep");
    let rollback = files(&overwrite.join(".mac_cpy_overwritten"))
        .pop()
        .unwrap();
    let previous = Command::new("/usr/bin/tar")
        .arg("-xzOf")
        .arg(rollback)
        .arg("Documents/example.txt")
        .output()
        .unwrap();
    assert_ok(&previous);
    assert_eq!(previous.stdout, b"previous\n");
    let mut bad = archive.as_os_str().to_os_string();
    bad.push(".bad.tar.gz");
    let bad = PathBuf::from(bad);
    write(&bad, b"corrupt");
    let mut sidecar = bad.as_os_str().to_os_string();
    sidecar.push(".sha256");
    let mut original_sidecar = archive.as_os_str().to_os_string();
    original_sidecar.push(".sha256");
    fs::copy(original_sidecar, sidecar).unwrap();
    let fail = invoke(
        &[
            "-r".as_ref(),
            bad.as_os_str(),
            "--home".as_ref(),
            overwrite.as_os_str(),
        ],
        None,
    );
    assert!(!fail.status.success());
    assert!(String::from_utf8_lossy(&fail.stderr).contains("SHA-256 mismatch"));
    assert_eq!(
        fs::read(home.join("Documents/example.txt")).unwrap(),
        b"original\n"
    );
}
