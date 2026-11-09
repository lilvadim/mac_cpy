# mac_cpy

Small, configurable macOS backups for developers and Homebrew users. Save your projects, dotfiles, and selected application settings in a compressed archive; reinstall software from a Brewfile.

[Documentation](docs/usage.md)

- **Smaller backups:** skip reproducible caches, build output, and app binaries according to your rules, then gzip the selected data.
- **Flexible configuration:** three text files, home-relative paths, and Unix globs. Explicit includes override exclusions.
- **Smart exclusions:** local and nested `.gitignore` rules skip generated project files without blindly excluding every folder named `target`.
- **Direct streaming:** Rust handles selection; native macOS tar writes straight to the destination, without staging a second copy. Speed depends on storage and compression.
- **Simple workflow:** home is detected automatically. Preview, back up, and restore with short commands; checksums and replaced-file backups are built in.

## Install

Requires macOS and Rust 1.88+ to build. From the project checkout:

```sh
cargo install --path . --locked
```

Ensure `~/.cargo/bin` is in `PATH`. The Apple Silicon build targets macOS 11+, but has not been runtime-tested on Big Sur. Homebrew is optional; it is needed to capture a Brewfile.

## 1. Configure

Copy the supplied rules once, without overwriting existing personal files, then edit them:

```sh
config_dir="${XDG_CONFIG_HOME:-$HOME/.config}/mac_cpy"
mkdir -p "$config_dir"
cp -n exclude.mac_cpy root_exclude.mac_cpy include.mac_cpy "$config_dir/"
```

One entry per line, relative to home. Spaces need no quotes; lines starting with `#` are comments. Examples from the supplied rules:

| File | Example entries | Effect |
| --- | --- | --- |
| [`root_exclude.mac_cpy`](root_exclude.mac_cpy) | `Library`, `Applications`, `Games` | Skip entire top-level entries during ordinary traversal. |
| [`exclude.mac_cpy`](exclude.mac_cpy) | `.cargo/registry`, `.gradle/caches`, `**/.DS_Store`, `@GITIGNORE` | Exclude paths and enable local `.gitignore` rules. |
| [`include.mac_cpy`](include.mac_cpy) | `Library/Application Support/VSCodium/User/settings.json`, `.vscode-oss/extensions`, `Library/Application Support/JetBrains` | Force selected settings and extensions back in. |

**Priority:** root exclusions → path exclusions and `.gitignore` → forced includes. An included directory forces its entire subtree, including ignored files. Other home contents are eligible by default: review the supplied rules. [Full configuration guide](docs/usage.md#configuration)

## 2. Preview and estimate

The destination directory must already exist.

```sh
mac_cpy /Volumes/Backup --dry-run
mac_cpy /Volumes/Backup --dry-run --estimate
```

The first command lists selected paths and exclusions. The second estimates uncompressed/compressed size and creation time **without creating a full backup**. It reads a small sample and writes a temporary gzip archive to the destination (about 64 MiB maximum input), then deletes it. Time includes reading, compression, and writing; final verification is extra. Size planning assumes 30% compression savings, not a guaranteed ratio.

## 3. Back up

Close applications whose data you are saving, then run without `sudo`:

```sh
mac_cpy /Volumes/Backup
```

Creates `backup-<stamp>.tar.gz` and `.tar.gz.sha256`, validates the archive, and displays live progress. Keep both files together. **DONE WITH SKIPS** means some selected data was omitted; review the report before removing originals.

Use `--home /path/to/home` for another source, or `--metadata-only` to save just manifests and service files.

## Restore

On the receiving Mac, preview first, then restore:

```sh
mac_cpy --restore /Volumes/Backup/backup-example.tar.gz --dry-run
mac_cpy --restore /Volumes/Backup/backup-example.tar.gz
```

Restore checks SHA-256 and destination conflicts before extracting directly into your current home. Replaced files are first saved in `~/.mac_cpy_overwritten/before-<stamp>.tar.gz` with their original relative paths. Files absent from the archive remain in place. Use `--home /path/to/home` for another destination. The archive can stay on the external disk or inside home.

Restore is **not fully atomic** and has no automatic rollback after a write failure. See [restore behavior](docs/usage.md#restore-behavior) for symlinks, ownership, and compatibility limits.

## Metadata and app installation

After restoring, `~/.mac_cpy_restore/metadata/` contains:

| File | Purpose |
| --- | --- |
| `Brewfile` | Homebrew manifest, including `mas`/`cargo` entries when exported by Homebrew. |
| `Install-commands.txt` | Manual commands to install Homebrew, configure PATH, apply Brewfile, and install rustup when `cargo` entries are present. |
| `Homebrew-versions.txt` | Informational version list; not used to pin restored versions. |
| `Applications-not-in-Brew.txt` | Candidates for manual installation, checked against casks, App Store entries, and Homebrew package receipts where available. |
| `hosts`, `macos.txt`, `source.txt` | Saved `/etc/hosts`, running macOS version, and source information. |
| Optional reports | Skipped paths, `.gitignore` exclusions, and exclusions specific to the run. |

The service directory also holds the binary and three loaded configs. Without Homebrew, its manifests are omitted and an explanatory file is saved. Apps and system settings are not applied automatically. Review the saved installation commands, then run as needed:

```sh
brew bundle install --file="$HOME/.mac_cpy_restore/metadata/Brewfile"
diff -u /etc/hosts "$HOME/.mac_cpy_restore/metadata/hosts"
```

**Archives are not encrypted and may contain secrets.** Restore only trusted archives. This is selective file migration; data from newer application versions may not work on older ones.

[Full guide](docs/usage.md) · [BSD 3-Clause licence](LICENCE)
