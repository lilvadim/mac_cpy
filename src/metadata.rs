use crate::{Result, config::Config, error, layout::INFO_DIR, process, scan::Scan};
use serde_json::Value;
use std::{
    collections::BTreeSet,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

pub(crate) fn collect(
    home: &Path,
    stage: &Path,
    selected: &[PathBuf],
    config: &Config,
    scan: &Scan,
) -> Result<()> {
    let info = stage.join(INFO_DIR);
    let metadata = info.join("metadata");
    fs::create_dir_all(&metadata)?;
    config.save_beside(&info)?;
    if config.gitignore {
        fs::write(
            metadata.join("gitignored-paths.txt"),
            scan.gitignored
                .iter()
                .map(|p| format!("{}\n", p.display()))
                .collect::<String>(),
        )?;
    }
    if !config.run_excludes.is_empty() {
        fs::write(
            metadata.join("excluded-for-run.txt"),
            config
                .run_excludes
                .iter()
                .map(|p| format!("{}\n", p.display()))
                .collect::<String>(),
        )?;
    }
    fs::write(info.join("FORMAT"), "macos-copy-archive-v2\n")?;
    fs::write(
        info.join("paths.txt"),
        selected
            .iter()
            .map(|p| format!("{}\n", p.display()))
            .collect::<String>(),
    )?;
    if !scan.problems.contains_key(Path::new("/etc/hosts")) {
        fs::copy("/etc/hosts", metadata.join("hosts"))?;
    }
    if !scan.problems.is_empty() {
        fs::write(metadata.join("skipped-paths.txt"), scan.report())?;
    }
    fs::write(
        metadata.join("macos.txt"),
        process::output(&mut Command::new("/usr/bin/sw_vers"))?,
    )?;
    fs::write(
        metadata.join("source.txt"),
        format!(
            "Home source: {}\nSystem metadata captured from the running Mac.\nmac_cpy {}\n",
            home.display(),
            env!("CARGO_PKG_VERSION")
        ),
    )?;
    let mut managed = BTreeSet::new();
    let mut package_apps = BTreeSet::new();
    let mut brewfile = None;
    if process::available("brew") {
        let mut file = OsString::from("--file=");
        file.push(metadata.join("Brewfile"));
        process::run(
            Command::new("brew")
                .env("HOMEBREW_NO_AUTO_UPDATE", "1")
                .args([
                    OsString::from("bundle"),
                    "dump".into(),
                    "--force".into(),
                    file,
                ])
                .stdout(Stdio::null()),
        )?;
        let dumped = fs::read_to_string(metadata.join("Brewfile"))?;
        managed.extend(mas_names(&dumped));
        brewfile = Some(dumped);
        fs::write(
            metadata.join("Homebrew-versions.txt"),
            process::output(
                Command::new("brew")
                    .env("HOMEBREW_NO_AUTO_UPDATE", "1")
                    .args(["list", "--versions"]),
            )?,
        )?;
        let json: Value = serde_json::from_slice(&process::output(
            Command::new("brew")
                .env("HOMEBREW_NO_AUTO_UPDATE", "1")
                .args(["info", "--json=v2", "--installed"]),
        )?)?;
        if let Some(casks) = json.get("casks").and_then(Value::as_array) {
            package_apps = crate::pkg_inventory::applications(casks)?;
            for cask in casks {
                if let Some(artifacts) = cask.get("artifacts").and_then(Value::as_array) {
                    for artifact in artifacts {
                        if let Some(apps) = artifact.get("app").and_then(Value::as_array) {
                            for app in apps {
                                let path = app
                                    .as_str()
                                    .or_else(|| app.get("target").and_then(Value::as_str));
                                if let Some(name) = path.and_then(|p| Path::new(p).file_name()) {
                                    managed.insert(name.to_os_string());
                                }
                            }
                        }
                    }
                }
            }
        }
    } else {
        fs::write(
            metadata.join("Homebrew-unavailable.txt"),
            "Homebrew not found; no package manifest generated.\n",
        )?;
        crate::progress::log(
            "WARNING: Homebrew not found; saving app inventory without package ownership information.",
        );
    }
    fs::write(
        metadata.join("Install-commands.txt"),
        install_commands(brewfile.as_deref()),
    )?;
    let mut apps = BTreeSet::new();
    for root in [PathBuf::from("/Applications"), home.join("Applications")] {
        inventory(&root, 1, &mut apps)?;
    }
    let mut text = String::from(
        "Candidates not covered by Brewfile casks or mas entries (app/mas names and pkgutil receipt paths; verify manually):\n",
    );
    for path in apps {
        if !package_apps.contains(&path)
            && !path.file_name().is_some_and(|name| managed.contains(name))
        {
            text.push_str(&format!("{}\n", path.display()));
        }
    }
    fs::write(metadata.join("Applications-not-in-Brew.txt"), text)?;
    let executable = std::env::current_exe()?;
    if !executable.is_file() {
        return Err(error("Cannot locate current executable"));
    }
    fs::copy(executable, info.join("mac_cpy"))?;
    fs::write(
        info.join("README.txt"),
        "Restore with mac_cpy -r ARCHIVE --home HOME.\nInstall apps separately using metadata/Brewfile; see metadata/Install-commands.txt. Review hosts manually.\nArchives contain secrets and are not encrypted. Close applications before backup/restore.\nHome files and application data follow the archived include/exclude configuration.\n",
    )?;
    Ok(())
}

fn install_commands(brewfile: Option<&str>) -> String {
    let mut commands = String::from(concat!(
        "# Run commands manually after restoring; mac_cpy does not execute them.\n",
        "# Official installers use their current release, without pinned versions.\n",
        "# Homebrew: https://brew.sh/\n",
        "# Install Homebrew if missing; follow its Next steps to persist PATH.\n",
        "/bin/bash -c \"$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)\"\n\n",
        "# Enable Homebrew in the current shell (Apple Silicon or Intel).\n",
        "if [ -x /opt/homebrew/bin/brew ]; then\n",
        "    eval \"$(/opt/homebrew/bin/brew shellenv)\"\n",
        "elif [ -x /usr/local/bin/brew ]; then\n",
        "    eval \"$(/usr/local/bin/brew shellenv)\"\n",
        "fi\n",
    ));
    if brewfile.is_some_and(|text| {
        text.lines()
            .any(|line| dumped_entry(line, "cargo").is_some())
    }) {
        commands.push_str(concat!(
            "\n# Cargo packages found in Brewfile: install Rust before brew bundle.\n",
            "# rustup: https://rustup.rs/\n",
            "curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh\n",
            ". \"${CARGO_HOME:-$HOME/.cargo}/env\"\n",
        ));
    }
    if brewfile.is_some() {
        commands.push_str(concat!(
            "\n# Install packages from the restored Brewfile.\n",
            "# For a custom restore home, replace $HOME with its path.\n",
        ));
        commands.push_str(&format!(
            "brew bundle install --file=\"$HOME/{INFO_DIR}/metadata/Brewfile\"\n"
        ));
    } else {
        commands.push_str("\n# No Brewfile was captured; install packages manually.\n");
    }
    commands
}

fn dumped_entry<'a>(line: &'a str, kind: &str) -> Option<&'a str> {
    let rest = line.trim().strip_prefix(kind)?;
    let entry = rest.strip_prefix(char::is_whitespace)?.trim_start();
    // Homebrew dumps quoted names. Do not match comments, package names or Ruby code.
    entry.starts_with('"').then_some(entry)
}

fn mas_names(brewfile: &str) -> BTreeSet<OsString> {
    let mut names = BTreeSet::new();
    // Read the generated dump as data; never execute its Ruby DSL.
    for line in brewfile.lines() {
        let Some(entry) = dumped_entry(line, "mas") else {
            continue;
        };
        let mut strings = serde_json::Deserializer::from_str(entry).into_iter::<String>();
        if let Some(Ok(name)) = strings.next() {
            let suffix = entry[strings.byte_offset()..].trim();
            if suffix.strip_prefix(", id:").is_some_and(|id| {
                let id = id.trim();
                !id.is_empty() && id.bytes().all(|c| c.is_ascii_digit())
            }) {
                names.insert(OsString::from(format!("{name}.app")));
                continue;
            }
        }
        crate::progress::log(
            "WARNING: Cannot match a generated Brewfile mas entry; its app may remain in Applications-not-in-Brew.txt.",
        );
    }
    names
}

fn inventory(root: &Path, depth: u8, apps: &mut BTreeSet<PathBuf>) -> Result<()> {
    if !root.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "app") {
            apps.insert(path);
        } else if depth > 0 && entry.file_type()?.is_dir() {
            inventory(&path, depth - 1, apps)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_notes_detect_cargo_entries_and_keep_rust_before_bundle() {
        for entry in [
            "cargo \"ripgrep\"\n",
            "  cargo\t\"tool\", source: \"https://example.com/tool?tag=v1\"\n",
        ] {
            let notes = install_commands(Some(entry));
            assert!(
                notes
                    .contains("https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh")
            );
            assert!(
                notes.find("https://sh.rustup.rs").unwrap()
                    < notes.find("brew bundle install").unwrap()
            );
            assert!(notes.contains("${CARGO_HOME:-$HOME/.cargo}/env"));
        }
        for text in [
            "",
            "# cargo \"tool\"\n",
            "brew \"cargo\"\n",
            "cask \"cargo\"\n",
            "cargo_helper \"tool\"\n",
            "puts 'cargo'\n",
        ] {
            let notes = install_commands(Some(text));
            assert!(!notes.contains("https://sh.rustup.rs"), "{text}");
            assert!(notes.contains("brew bundle install"));
        }
        let notes = install_commands(None);
        assert!(notes.contains("https://brew.sh/"));
        assert!(!notes.contains("https://sh.rustup.rs"));
        assert!(!notes.contains("brew bundle install"));
    }

    #[test]
    fn mas_inventory_reads_dumped_names_without_evaluating_brewfile() {
        let names = mas_names(concat!(
            "brew \"git\"\n",
            "cask \"firefox\"\n",
            "# mas \"Comment\", id: 1\n",
            "mas \"Store App\", id: 1234\n",
            "  mas \"Календарь\", id: 5678\n",
            "mas \"An \\\"App\\\"\", id: 9012\n",
            "puts 'must not execute'\n",
        ));
        assert_eq!(names.len(), 3);
        for name in ["Store App.app", "Календарь.app", "An \"App\".app"] {
            assert!(names.contains(std::ffi::OsStr::new(name)));
        }
    }
}
