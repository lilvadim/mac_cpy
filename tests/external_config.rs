#![cfg(target_os = "macos")]
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
};

fn write(path: impl AsRef<Path>, text: &str) {
    let path = path.as_ref();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}
fn invoke(exe: &Path, home: &Path, destination: &Path, dry: bool, bin: &Path) -> Output {
    let mut cmd = Command::new(exe);
    cmd.arg(destination).arg("--home").arg(home);
    if dry {
        cmd.arg("--dry-run");
    }
    let mut path = bin.as_os_str().to_owned();
    path.push(":/usr/bin:/bin:/usr/sbin:/sbin");
    cmd.env("PATH", path)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .current_dir("/")
        .output()
        .unwrap()
}
fn ok(out: &Output) {
    assert!(
        out.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
fn included(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout)
        .split("INCLUDED PATHS (directories are recursive)\n")
        .nth(1)
        .unwrap()
        .split("\nEXCLUDED ROOTS")
        .next()
        .unwrap()
        .to_owned()
}

#[test]
fn directive_collision_is_checked_after_filters_without_writing_a_backup() {
    let t = tempfile::tempdir().unwrap();
    let home = t.path().join("home");
    let destination = t.path().join("destination");
    let config = home.join(".config/mac_cpy");
    fs::create_dir_all(&config).unwrap();
    fs::create_dir(&destination).unwrap();
    let collision = home.join("@GITIGNORE");
    let run = |flags: &[&str], success: bool| {
        let out = Command::new(env!("CARGO_BIN_EXE_mac_cpy"))
            .arg(&destination)
            .arg("--home")
            .arg(&home)
            .args(flags)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .current_dir("/")
            .output()
            .unwrap();
        if success {
            ok(&out);
        } else {
            assert!(!out.status.success());
            let error = String::from_utf8_lossy(&out.stderr);
            assert!(error.contains("Temporarily rename"), "{error}");
            assert!(error.contains(collision.to_str().unwrap()), "{error}");
        }
        assert!(fs::read_dir(&destination).unwrap().next().is_none());
    };
    for kind in ["file", "directory", "symlink"] {
        for name in ["exclude.mac_cpy", "root_exclude.mac_cpy", "include.mac_cpy"] {
            write(config.join(name), "");
        }
        match kind {
            "file" => write(&collision, "preserve me"),
            "directory" => fs::create_dir(&collision).unwrap(),
            _ => std::os::unix::fs::symlink("missing-target", &collision).unwrap(),
        }
        run(&["--dry-run"], false);
        run(&["--dry-run", "--estimate"], false);
        run(&[], false);
        run(&["--metadata-only", "--dry-run"], true);
        write(config.join("exclude.mac_cpy"), "./@GITIGNORE\n");
        run(&["--dry-run"], true);
        write(config.join("exclude.mac_cpy"), "");
        write(config.join("root_exclude.mac_cpy"), "./@GITIGNORE\n");
        run(&["--dry-run"], true);
        write(config.join("root_exclude.mac_cpy"), "");
        write(home.join(".gitignore"), "@GITIGNORE\n");
        write(config.join("exclude.mac_cpy"), "@GITIGNORE\n");
        run(&["--dry-run"], true);
        write(config.join("include.mac_cpy"), "./@GITIGNORE\n");
        run(&["--dry-run"], false);
        match kind {
            "file" => {
                assert_eq!(fs::read_to_string(&collision).unwrap(), "preserve me");
                fs::remove_file(&collision).unwrap();
            }
            "directory" => fs::remove_dir(&collision).unwrap(),
            _ => {
                assert_eq!(
                    fs::read_link(&collision).unwrap(),
                    Path::new("missing-target")
                );
                fs::remove_file(&collision).unwrap();
            }
        }
    }
    for name in ["exclude.mac_cpy", "root_exclude.mac_cpy", "include.mac_cpy"] {
        write(config.join(name), "");
    }
    write(
        home.join("Documents/@GITIGNORE"),
        "nested path is unambiguous",
    );
    write(home.join("@gitignore"), "case-sensitive name");
    run(&["--dry-run", "--estimate"], true);
}

#[test]
fn adjacent_configs_are_live_and_affect_selection_scan_and_tar() {
    let t = tempfile::tempdir().unwrap();
    let install = t.path().join("install");
    let home = t.path().join("home");
    let dst = t.path().join("destination");
    let bin = t.path().join("commands");
    for p in [&install, &home, &dst, &bin] {
        fs::create_dir(p).unwrap();
    }
    let exe = install.join("mac_cpy");
    fs::copy(env!("CARGO_BIN_EXE_mac_cpy"), &exe).unwrap();
    write(home.join("Documents/keep.txt"), "keep");
    write(home.join("Documents/ignored.txt"), "skip");
    write(home.join("ExcludedRoot/skip"), "skip");
    write(home.join("Library/Custom/first.txt"), "first");
    write(home.join("Library/Custom/second.txt"), "second");
    write(home.join(".codex/sessions/omitted.json"), "excluded data");
    write(home.join("Applications/Store App.app/fixture"), "store app");
    write(
        home.join("Applications/Manual App.app/fixture"),
        "manual app",
    );
    // A copy without neighboring configs must fail instead of backing up everything.
    let missing = invoke(&exe, &home, &dst, true, &bin);
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("Cannot read configuration"));
    write(
        install.join("root_exclude.mac_cpy"),
        "Library\nExcludedRoot\n",
    );
    write(
        install.join("exclude.mac_cpy"),
        "Documents/ignored.txt\n.codex\n",
    );
    write(
        install.join("include.mac_cpy"),
        "Library/Custom/first.txt\n",
    );
    let first = invoke(&exe, &home, &dst, true, &bin);
    ok(&first);
    assert!(included(&first).contains("Library/Custom/first.txt"));
    assert!(!included(&first).contains("ExcludedRoot"));
    // Runtime edits apply immediately, with no compilation and no cwd dependence.
    write(
        install.join("include.mac_cpy"),
        "Library/Custom/{second,absent}.txt\n",
    );
    let second = invoke(&exe, &home, &dst, true, &bin);
    ok(&second);
    assert!(!included(&second).contains("Library/Custom/first.txt"));
    assert!(included(&second).contains("Library/Custom/second.txt"));
    let user_config = home.join(".config/mac_cpy");
    for name in ["exclude.mac_cpy", "root_exclude.mac_cpy", "include.mac_cpy"] {
        write(
            user_config.join(name),
            &fs::read_to_string(install.join(name)).unwrap(),
        );
    }
    // User configuration wins over conflicting portable defaults.
    write(
        install.join("include.mac_cpy"),
        "Library/Custom/first.txt\n",
    );
    let preferred = invoke(&exe, &home, &dst, true, &bin);
    ok(&preferred);
    assert!(
        String::from_utf8_lossy(&preferred.stdout)
            .contains(&format!("CONFIG: {}", user_config.display()))
    );
    assert!(included(&preferred).contains("Library/Custom/second.txt"));
    let default_home_config = Command::new(&exe)
        .arg(&dst)
        .arg("--home")
        .arg(&home)
        .arg("--dry-run")
        .env("HOME", &home)
        .env_remove("XDG_CONFIG_HOME")
        .current_dir("/")
        .output()
        .unwrap();
    ok(&default_home_config);
    assert!(
        String::from_utf8_lossy(&default_home_config.stdout)
            .contains(&format!("CONFIG: {}", user_config.display()))
    );
    write(
        bin.join("brew"),
        r#"#!/bin/sh
case "$1" in
bundle) for a in "$@"; do case "$a" in --file=*) printf 'brew "git"\nmas "Store App", id: 1234\ncargo "ripgrep"\n' > "${a#--file=}";; esac; done;;
list) printf 'git 2.0\n';;
info) printf '{"casks":[]}\n';;
*) exit 2;;
esac
"#,
    );
    fs::set_permissions(bin.join("brew"), fs::Permissions::from_mode(0o700)).unwrap();
    let backup = invoke(&exe, &home, &dst, false, &bin);
    ok(&backup);
    let archive: PathBuf = fs::read_dir(&dst)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.to_string_lossy().ends_with(".tar.gz"))
        .unwrap();
    let listing = Command::new("/usr/bin/tar")
        .arg("-tzf")
        .arg(&archive)
        .output()
        .unwrap();
    ok(&listing);
    let names = String::from_utf8_lossy(&listing.stdout);
    for absent in [
        "Documents/ignored.txt",
        ".codex/sessions/omitted.json",
        "ExcludedRoot/skip",
        "Library/Custom/first.txt",
    ] {
        assert!(!names.contains(absent), "{absent}");
    }
    assert!(names.contains("Library/Custom/second.txt"));
    let config = Command::new("/usr/bin/tar")
        .arg("-xzOf")
        .arg(&archive)
        .arg(".mac_cpy_restore/include.mac_cpy")
        .output()
        .unwrap();
    ok(&config);
    assert_eq!(config.stdout, b"Library/Custom/{second,absent}.txt\n");
    let inventory = Command::new("/usr/bin/tar")
        .arg("-xzOf")
        .arg(&archive)
        .arg(".mac_cpy_restore/metadata/Applications-not-in-Brew.txt")
        .output()
        .unwrap();
    ok(&inventory);
    let apps = String::from_utf8(inventory.stdout).unwrap();
    assert!(!apps.contains("Store App.app"));
    assert!(apps.contains("Manual App.app"));
    let commands = Command::new("/usr/bin/tar")
        .arg("-xzOf")
        .arg(&archive)
        .arg(".mac_cpy_restore/metadata/Install-commands.txt")
        .output()
        .unwrap();
    ok(&commands);
    let instructions = String::from_utf8(commands.stdout).unwrap();
    assert!(instructions.contains("https://brew.sh/"));
    assert!(
        instructions.find("https://sh.rustup.rs").unwrap()
            < instructions.find("brew bundle install").unwrap()
    );
    let script = t.path().join("check-commands.sh");
    write(&script, &instructions);
    ok(&Command::new("/bin/sh")
        .arg("-n")
        .arg(script)
        .output()
        .unwrap());
    // Invalid paths must stop before writing a new backup.
    write(user_config.join("include.mac_cpy"), "../outside\n");
    let invalid = invoke(&exe, &home, &dst, true, &bin);
    assert!(!invalid.status.success());
    // Excluding archive-internal metadata must not produce a falsely successful backup.
    write(user_config.join("include.mac_cpy"), "");
    write(user_config.join("exclude.mac_cpy"), "**/FORMAT\n.codex\n");
    let broken = invoke(&exe, &home, &dst, false, &bin);
    assert!(!broken.status.success());
    assert!(String::from_utf8_lossy(&broken.stderr).contains("required archive metadata"));
    assert_eq!(
        fs::read_to_string(home.join("Documents/ignored.txt")).unwrap(),
        "skip"
    );
}
