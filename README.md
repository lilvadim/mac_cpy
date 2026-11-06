# mac_cpy

Selective macOS home-directory backup and restore, written in Rust. Files are streamed directly into a `.tar.gz` archive at the destination using macOS's built-in `/usr/bin/tar`. No second local copy of the selected data is created.

The archive includes the selected files, configuration snapshots, a Homebrew manifest when available, an application inventory, a copy of `/etc/hosts`, and the running `mac_cpy` executable. A separate SHA-256 file accompanies the archive.

This is a file migration tool, not a bootable system image or a replacement for a complete system backup. It does not install applications, apply system preferences, or automatically replace `/etc/hosts`.

## Requirements

- macOS and its built-in BSD tar. Run as your regular user; running as root is rejected.
- Rust 1.88 or later to build. No Python, Ruby, or shell script implements the backup logic; external tools are launched directly. Homebrew itself has its own dependencies.
- Homebrew is optional. If it is absent, the backup continues without a Brewfile or package versions. If it is present, failures in its metadata-export commands abort the backup.
- Existing source home and destination directories, with sufficient access and free space.

The Apple Silicon deployment target is **macOS 11.0 Big Sur**, set in [`.cargo/config.toml`](.cargo/config.toml). This is a build target, not a claim that the program has been runtime-tested on Big Sur. Dependencies are locked in `Cargo.lock`. Homebrew and individual applications have separate OS requirements.

Close source applications and stop builds before backing up. If selected data contains Firefox or Codex data, a real backup checks their processes and requires them to be closed. The program never closes them automatically. Dry runs and metadata-only backups do not perform this process check.

## Build and install

Run these commands from the project checkout:

```sh
cargo build --release --locked
./target/release/mac_cpy --help

# Install the binary into Cargo's bin directory, normally ~/.cargo/bin.
cargo install --path . --locked

# Copy the initial configuration without overwriting existing personal rules.
config_dir="${XDG_CONFIG_HOME:-$HOME/.config}/mac_cpy"
mkdir -p "$config_dir"
cp -n exclude.mac_cpy root_exclude.mac_cpy include.mac_cpy "$config_dir/"
```

Ensure Cargo's bin directory is in `PATH`. `cargo install` does not install the configuration files. Review the supplied rules before your first backup: they are an opinionated starting point, not a complete selection for every user.

Alternatively, run the binary from `target/release`. `build.rs` copies the three configuration files beside debug/release binaries, including cross-build output directories. These adjacent files are used only when the user configuration directory is absent.

## Usage

```text
mac_cpy DIRECTORY [--metadata-only] [--home HOME] [--dry-run [-e]]
mac_cpy -r ARCHIVE [--home HOME] [--dry-run]
mac_cpy --finalize ARCHIVE.tar.gz.partial [--dry-run]
```

| Option | Meaning |
| --- | --- |
| `--home HOME` | Override the source home for backup or destination home for restore; defaults to `$HOME`. No short alias. |
| `--restore`, `-r` | Restore a trusted archive with its adjacent `.sha256` file. |
| `--metadata-only` | Back up manifests and service files, without home files or application data. |
| `--dry-run` | Preview a backup or restore; with finalize, validate without completing the archive. |
| `--estimate`, `-e` | Add size estimates and a sample compression/write benchmark to a backup dry run. |
| `--finalize FILE` | Validate and complete an existing `.tar.gz.partial` without repacking. |
| `--help`, `-h` | Show options, configuration lookup, rule priority, and syntax. |

`--estimate` requires `--dry-run` in backup mode. `--metadata-only` cannot be combined with estimate, restore, or finalize.

```sh
# Preview selected paths and exclusions.
mac_cpy /Volumes/Backup --dry-run

# Estimate size and test the read → gzip → destination-write pipeline.
mac_cpy /Volumes/Backup --dry-run --estimate

# Create a backup, or just its metadata.
mac_cpy /Volumes/Backup
mac_cpy /Volumes/Backup --metadata-only

# Use a different source home.
mac_cpy /Volumes/Backup --home /Users/other --dry-run

# Check the archive and destination before restoring.
mac_cpy -r /Volumes/Backup/backup-example.tar.gz --dry-run
mac_cpy -r /Volumes/Backup/backup-example.tar.gz
```

Backup and restore can use an archive inside the selected home. Backup excludes only the exact paths of the current archive, its `.partial`, and its `.sha256`, not the entire destination directory. **Older backups in that directory can be included again** under the normal rules. Restore refuses any plan that would overwrite its source archive or checksum, including aliases through hard links. These protections do not prevent concurrent changes by other processes.

## Configuration

All three files are required:

| File | Rules |
| --- | --- |
| [`root_exclude.mac_cpy`](root_exclude.mac_cpy) | Literal names immediately under home, such as `Library` or `Games`. Ordinary traversal does not enter these roots. |
| [`exclude.mac_cpy`](exclude.mac_cpy) | Home-relative literal paths or Unix globs; matching a directory excludes its subtree. Also accepts `@GITIGNORE`. |
| [`include.mac_cpy`](include.mac_cpy) | Home-relative paths or globs that force inclusion, overriding the exclusions above and `.gitignore`. An included directory forces its entire subtree. |

Lookup order:

1. `$XDG_CONFIG_HOME/mac_cpy` when `XDG_CONFIG_HOME` is nonempty; it must be absolute. Otherwise `~/.config/mac_cpy`.
2. If that directory does not exist, the directory containing the running executable.

An existing but incomplete or invalid user configuration is an error; files from different locations are never mixed. `--home` changes the data directory, not configuration lookup. Rules are reloaded on each run and archived as loaded. Restore and finalize do not require current configuration files and do not reapply current exclusions.

### Priority and syntax

Ordinary selection starts with home entries except `root_exclude`, then applies `exclude` and enabled `.gitignore` rules. Finally, `include` forces matching paths back in, even below excluded parents. Line order does not change this priority. Overlapping selections are archived once.

The home directories `.mac_cpy_restore` and `.mac_cpy_overwritten`, and the current archive's output paths, are always reserved. `include` cannot force them into a backup of home.

Use one path or pattern per line:

- Empty lines and lines beginning with `#` after trimming whitespace are ignored. Leading/trailing whitespace is trimmed; internal spaces are literal. Do not quote paths.
- `exclude` and `include` support `*`, `?`, `[abc]`, `[a-z]`, `{a,b}`, and a separate `**` path component for recursive matching. Globs match dotfiles and are case-sensitive.
- Paths are relative to home. `node_modules` matches only `~/node_modules`; use `**/node_modules` for any depth.
- There is no shell expansion, regex syntax, `~` expansion, variable substitution, or command execution. Absolute paths, `..` components, backslashes, and control characters are rejected. Paths must be UTF-8.
- Missing include paths and patterns with no matches are ignored. Include discovery does not follow directory symlinks, including parents of literal include paths; a final symlink itself can be selected.

```text
# exclude.mac_cpy examples
Downloads/*.dmg
myprojects/**/target
**/.DS_Store
@GITIGNORE
```

```text
# include.mac_cpy examples
Library/Application Support/VSCodium/User/settings.json
Library/Application Support/JetBrains/*/{options,keymaps,codestyles}
```

### Local .gitignore files

The exact line `@GITIGNORE` in `exclude.mac_cpy` enables local and nested `.gitignore` rules through the Rust `ignore` library. It does not load global Git excludes or `.git/info/exclude`. Removing or commenting out the directive disables this behavior. There is no unconditional exclusion of directories named `target`.

Nested rules override parent rules. Git-style `!` negation cannot rescue a child while its parent remains ignored, and cannot override explicit `root_exclude`/`exclude` rules. Use `include.mac_cpy` for an explicit override. Errors loading `.gitignore` are fatal.

The directive is recognized only as an exact trimmed line; `./@GITIGNORE` is an ordinary literal path. An actual `HOME/@GITIGNORE` file, directory, or symlink blocks backup only if it survives selection and filtering, including in dry-run mode; temporarily rename it. Nested files with that name are allowed. Metadata-only mode skips this collision check.

## What the supplied rules preserve

Most home contents, including dotfiles, are selected by default. The supplied configuration then excludes broad roots and selected generated data; inspect the linked configuration files for the exact lists.

- `Library` is excluded except for explicit includes: Ghostty application support, VS Code/VSCodium `User/settings.json`, user fonts and keyboard layouts, selected JetBrains settings, and selected Firefox files.
- VS Code/VSCodium extension directories `.vscode/extensions` and `.vscode-oss/extensions` are included again. Keybindings, snippets, editor profiles, history, and workspace/global storage are not explicitly selected from `Library`.
- JetBrains includes `options`, `keymaps`, `codestyles`, `templates`, `scratches`, and `settingsSync` below each matching application/version directory.
- Firefox includes registration files, settings, tabs/session backups, selected permissions/search/container data, `chrome`, and profile-group SQLite/WAL files. All profiles in the standard `Profiles/*` directory match; external profile locations require extra rules. Bookmarks/history, cookies, saved passwords, and extensions are not explicitly included.
- Applications, Trash, selected game/VM roots, Docker data paths, Android AVDs, selected package caches, and `.codex/tmp` are excluded. There is no universal game/container-data detector: other locations remain subject to ordinary rules.
- Homebrew's installation directory and `etc` are not copied as separate sources. Application binaries in excluded application folders are not copied; the service copy of `mac_cpy` is included.

The configuration is authoritative. Adding a broader forced include may also include caches, ignored build output, or additional application data.

## Archive contents and metadata

A successful run produces:

```text
backup-<date-time>-<pid>-<nonce>.tar.gz
backup-<date-time>-<pid>-<nonce>.tar.gz.sha256
```

Selected home paths keep their relative structure. Service files live under `.mac_cpy_restore/`:

| File or directory | Contents |
| --- | --- |
| `FORMAT`, `paths.txt`, `README.txt` | Format marker, selected roots, and restore notes. `paths.txt` is not an exhaustive file manifest. |
| `mac_cpy` and the three `.mac_cpy` files | Executable and exact loaded rule snapshots. |
| `MODE` | Present for metadata-only backups, containing `metadata-only`. |
| `metadata/Brewfile` | Generated by `brew bundle dump`, if Homebrew is available. |
| `metadata/Homebrew-versions.txt` | Informational `brew list --versions`; not used to restore pinned versions. |
| `metadata/Applications-not-in-Brew.txt` | Candidates to review for manual installation. |
| `metadata/Install-commands.txt` | Manual Homebrew installation/PATH/Brewfile instructions; rustup instructions when the generated Brewfile contains `cargo` entries. Nothing is executed automatically. |
| `metadata/hosts`, `metadata/macos.txt`, `metadata/source.txt` | Readable `/etc/hosts`, running system version, and source information. |
| Optional metadata reports | `skipped-paths.txt`, `gitignored-paths.txt`, and `excluded-for-run.txt`. |

Metadata describes the running Mac even with a custom `--home`. Without Homebrew, `Homebrew-unavailable.txt` replaces its manifests. Package/app inventories can include games or Docker software even when their data is excluded.

The application inventory scans `/Applications` and `~/Applications`, plus one folder level. It subtracts cask `app` names and generated Brewfile `mas` names. For casks containing `pkg`, `uninstall.pkgutil` patterns identify installed receipts; `pkgutil` supplies application paths relative to each package's install location. This handles packages such as Karabiner-Elements. Unreadable receipts produce warnings and leave candidates visible. Packages without receipt patterns, differing App Store names, and system apps exposed in `/Applications` (such as Safari) may still appear. This is not an authoritative ownership database.

## Backup behavior and integrity

1. Scan the selected files, inspect metadata, and probe regular-file readability without reading their contents. Symlinks are stored as links; external targets are not traversed.
2. Check selected application processes and collect metadata into a local temporary directory.
3. Stream the exact scanned selection and service metadata into the destination `.tar.gz.partial`. No snapshot is taken: files can still change between scanning and archiving.
4. Read the archive to validate tar/gzip and required service entries, then read it separately for SHA-256. Write the sidecar and rename the completed archive.

Native tar uses PAX format, `--mac-metadata`, and `--xattrs`. Metadata is stored inside the archive, so the archive itself can be kept on exFAT, SMB, or a Linux server. FAT32 cannot hold an individual archive larger than its file-size limit. Restore on macOS to a filesystem that supports the required metadata, normally APFS.

Source access/metadata errors detected during scanning skip the affected file or subtree for that run. Warnings identify full paths; the final `SKIPPED PATHS` list and archived report record them. `DONE WITH SKIPS` still exits with code 0 and is not a complete backup of the requested selection. Invalid paths/configs, `.gitignore` errors, archive-time failures, full disks, and verification failures remain fatal.

Archives are **not encrypted** and may contain credentials. SHA-256 detects a changed archive relative to the sidecar; it does not authenticate its creator or prove completeness. Keep both files together and restore only trusted archives.

If storage explicitly reports that synchronization is unsupported, the program warns and continues without confirming durable storage. Other synchronization failures remain fatal. Native tar metadata warnings are not suppressed.

### Completing a partial archive

```sh
mac_cpy --finalize /Volumes/Backup/backup-example.tar.gz.partial --dry-run
mac_cpy --finalize /Volumes/Backup/backup-example.tar.gz.partial
```

Finalize validates tar/gzip and mandatory service entries, computes SHA-256, and completes the existing file without repacking. It refuses symlink inputs or existing final archive/checksum paths. A dry run validates without writing a sidecar or renaming. It cannot recover missing files or prove that an interrupted run captured all intended data. A failed or interrupted backup may leave a `.partial`; do not treat it as a completed backup merely because it exists.

## Restore behavior

Restore reads directly from the supplied archive; it does not first copy it to the internal disk or extract it into a staging tree.

1. Verify the archive against its adjacent `.sha256` and check its format marker.
2. Read the original archive paths and a separate tar/mtree type/link index, compare them, and reject unsafe or duplicate paths and invalid parent structures.
3. Inspect the destination and protect the source archive/checksum. Before replacing home files, save existing non-directory entries to `~/.mac_cpy_overwritten/before-<stamp>.tar.gz`, preserving their original relative paths. Ordinary files are saved even if their contents happen to match.
4. Extract into home, merging real directories. Existing destination files absent from the archive are not removed.

Preflight errors do not create restore directories or replace home files. They do not guarantee that later writes will succeed. Restore is **not a transaction**: an I/O failure, power loss, or concurrent destination change can leave a partial restore. No automatic whole-restore rollback is implemented. Native `--safe-writes` is used for individual files when available; older tar versions continue with a warning when it is unavailable.

Symlinks with exactly the same stored target text are left unchanged. Different final symlinks can be replaced by archived files/links, and ordinary files can be replaced by archived symlinks. A symlink in a parent directory, a destination symlink where an archived directory is needed, or a file/directory type conflict aborts preflight. Link text such as `file` and `./file` is compared literally.

File modes and supported native metadata are restored; original UID/GID ownership is not restored (`--no-same-owner`). ACLs, permissions, available space, and application-format compatibility can still affect migration. In particular, copying application data from a newer macOS/app version to an older one is not guaranteed to work.

The checksum, format, index, and extraction stages require multiple archive reads. On slow flash/network storage, copying the archive and sidecar to a fast local disk first may reduce restore time if space permits. Keep a standalone binary or source checkout before erasing the original system; otherwise extract `.mac_cpy_restore/mac_cpy` manually to bootstrap restore.

Older archives using different service-directory names (for example `.macos-copy-info`) are not supported by the current reader. Use the matching earlier executable for those archives.

After restoring, review `~/.mac_cpy_restore/metadata/Install-commands.txt`, install applications separately, and compare hosts manually:

```sh
brew bundle install --file="$HOME/.mac_cpy_restore/metadata/Brewfile"
diff -u /etc/hosts "$HOME/.mac_cpy_restore/metadata/hosts"
```

The saved package-version list is informational; Brewfile installs available packages, subject to its entries and Homebrew behavior. Keychain/password migration and cloud-only originals require separate preparation.

## Progress and estimates

In an interactive terminal, a bordered progress block redraws in place. Warnings remain above it, and skipped paths are listed at the end. Non-TTY output and `TERM=dumb` use plain stage summaries without cursor controls.

Scanning has no known total in advance. Archive creation, validation, and extraction count tar events, including directories and service entries; their percentages and stage ETA are item-based, so a single large file can stall the percentage and increase ETA. SHA-256 progress counts bytes.

During creation, a background calculation uses the completed scan's cached metadata; it does not scan home again or run a compression benchmark. `Total size (uncompressed)` includes an approximate allowance for headers/metadata. `Estimated archive` assumes gzip output at **70%** of that size, a planning heuristic rather than a measured compression ratio.

`--dry-run` lists selections and exclusions. With `@GITIGNORE`, it also walks selected paths and lists ignored entries. Without that directive, it does not fully scan ordinary data, apart from checking a selected directive-name collision. It does not export Homebrew metadata or write an archive to the destination.

`--dry-run --estimate` performs the full scan and estimates size without following symlinks or double-counting hard-linked file data. It benchmarks native tar/gzip directly to a temporary archive at the destination using up to 64 whole files, each at most 8 MiB, with about 64 MiB total input. Less than 1 MiB of eligible input makes time estimation unavailable. File changes/resource forks can exceed this approximate bound. The estimate covers creation, not final validation and SHA-256. Reported sample compression does not replace the fixed size-planning heuristic.

The benchmark file is cleaned up after normal completion, errors, and handled SIGINT/SIGTERM; forced termination or power loss can leave it behind. Unlike a plain dry run, estimate mode writes temporary data and reads actual file contents. Reading cloud-backed files may download them.

## Development

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

Tests use temporary homes, synthetic data, mocked Homebrew, and native macOS tar. They cover selection, configuration precedence, estimates, checksums, backup/restore, links, metadata, permissions, conflict preflight, and replaced-file preservation. They are not a full-system migration test or a guarantee of compatibility across application versions.

Core modules: `scan` collects filesystem metadata; `policy`, `patterns`, `includes`, and `gitignore` implement selection; `estimate` consumes the scan; `metadata` and `pkg_inventory` collect manifests; `archive` and `restore_plan` implement backup and restore; `process`, `progress`, and `storage` handle execution, reporting, and synchronization.
