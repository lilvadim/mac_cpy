use crate::{Result, error};
use std::{ffi::OsString, path::PathBuf};

fn usage() -> String {
    format!(
        r#"mac_cpy DIRECTORY [--metadata-only] [--home HOME] [--dry-run [-e]]
mac_cpy -r ARCHIVE [--home HOME] [--dry-run]
mac_cpy --finalize ARCHIVE.tar.gz.partial [--dry-run]

--home HOME         Override source/destination home (defaults to $HOME)
--restore, -r FILE   Restore a trusted .tar.gz archive and adjacent .sha256
--finalize FILE     Verify an existing .partial, calculate SHA-256, complete it
--metadata-only     Archive metadata only; exclude all home files
--dry-run           Preview backup/restore; finalize: verify without changes
--estimate, -e      Backup dry-run only: estimate size and benchmark up to 64 MiB
--help, -h          Show this text

CONFIGURATION (backup)
  Read all three files from $XDG_CONFIG_HOME/mac_cpy when XDG_CONFIG_HOME is
  nonempty (must be absolute), otherwise from ~/.config/mac_cpy.
  If that directory is absent, read all three beside the executable.
  An incomplete/invalid set is an error; sets are never mixed.
  --home changes the data location, not the configuration location.
  Configs are read on every run and saved in the archive.
  Restore/finalize do not require current config files.

FILES AND RULE PRIORITY
  1. Start with home entries, except those in root_exclude.mac_cpy.
  2. Apply exclude.mac_cpy and enabled .gitignore rules to ordinary data.
  3. Force paths from include.mac_cpy, overriding all of the above exclusions.
     An included directory forces its entire subtree, including ignored files.
  From home, always exclude {info_dir}, {overwritten_dir} and the current
  backup's archive, partial file and checksum when they are inside home.
  Line order in the three path lists does not change their priority.

SYNTAX
  One home-relative path per line; files and directories are supported.
  Directories are recursive. Spaces are literal: do not quote paths.
  Blank lines and lines starting with # (after whitespace) are ignored.
  No shell expansion, absolute paths, backslashes or .. components.

  root_exclude.mac_cpy: literal top-level names only, e.g. Library.
  exclude.mac_cpy and include.mac_cpy: home-relative paths or Unix globs:
    *       zero or more characters within one path component
    ?       one character within one path component
    [abc]   a character set or range, e.g. [a-z]
    {{a,b}}   alternatives
    **      recursive directories (as a whole path component)
    node_modules means only ~/node_modules; **/node_modules means any depth.
    Excluding a directory excludes its contents; including one forces its contents.
    Globs also match dotfiles. No regular expressions or shell execution.
    Include example: Library/Application Support/JetBrains/*/{{options,keymaps}}
    Exclude example: myprojects/**/target
    Patterns with no matches are ignored; include discovery does not follow directory links.
    Overlapping selected roots are scanned and archived once.

  Only exclude.mac_cpy supports {gitignore_directive}, enabling local and nested .gitignore.
  The exact directive line is reserved; prefix ./ to treat it as a literal path.
  A file, directory or link with the directive name in the home root blocks backup
  only if it passes all filters (also checked in dry-run). Temporarily rename it.
  Metadata-only does not copy home data, so it skips this collision check.
  Inside .gitignore, ! can undo ignore rules, but a fully ignored parent directory
  must be unignored before its children. Also, ! cannot undo exclusions from our
  root_exclude/exclude configs. Use include.mac_cpy to force a path in either case.

  --dry-run lists selected paths and exclusions; add -e for size/time estimates.
  --metadata-only skips home selection but saves configs and system metadata.

Close source applications. Run without sudo. Archives are NOT encrypted."#,
        gitignore_directive = crate::config::GITIGNORE_DIRECTIVE,
        info_dir = crate::layout::INFO_DIR,
        overwritten_dir = crate::layout::OVERWRITTEN_DIR,
    )
}

#[derive(Debug)]
pub(crate) struct Options {
    pub location: PathBuf,
    pub home: PathBuf,
    pub restore: bool,
    pub finalize: bool,
    pub dry_run: bool,
    pub estimate: bool,
    pub metadata_only: bool,
}

impl Options {
    pub fn parse(args: Vec<OsString>) -> Result<Option<Self>> {
        let mut home = std::env::var_os("HOME").map(PathBuf::from);
        let (mut location, mut restore, mut dry_run, mut estimate) = (None, false, false, false);
        let mut finalize = false;
        let mut metadata_only = false;
        let mut iter = args.into_iter();
        while let Some(argument) = iter.next() {
            match argument.to_str() {
                Some("--help" | "-h") => {
                    println!("{}", usage());
                    return Ok(None);
                }
                Some("--home") => home = Some(value(&mut iter, "--home")?.into()),
                Some("--restore" | "-r" | "--finalize") => {
                    if location.is_some() {
                        return Err(error("Specify only one backup location"));
                    }
                    let flag = argument.to_str().unwrap();
                    location = Some(PathBuf::from(value(&mut iter, flag)?));
                    finalize = flag == "--finalize";
                    restore = !finalize;
                }
                Some("--dry-run") => dry_run = true,
                Some("--metadata-only") => metadata_only = true,
                Some("--estimate" | "-e") => estimate = true,
                Some(s) if s.starts_with('-') => return Err(error(format!("Unknown option: {s}"))),
                _ => {
                    if location.replace(PathBuf::from(argument)).is_some() {
                        return Err(error("Specify only one backup location"));
                    }
                }
            }
        }
        if estimate && (!dry_run || restore || finalize) {
            return Err(error("--estimate requires --dry-run in backup mode"));
        }
        if metadata_only && (restore || finalize || estimate) {
            return Err(error(
                "--metadata-only is a backup option; it cannot be combined with --restore, --finalize or --estimate",
            ));
        }
        Ok(Some(Self {
            location: location.ok_or_else(|| error(usage()))?,
            home: home.ok_or_else(|| error("Cannot determine HOME; use --home"))?,
            restore,
            finalize,
            dry_run,
            estimate,
            metadata_only,
        }))
    }
}

fn value(iter: &mut impl Iterator<Item = OsString>, flag: &str) -> Result<OsString> {
    iter.next()
        .filter(|s| !s.is_empty() && !s.to_string_lossy().starts_with('-'))
        .ok_or_else(|| error(format!("Specify a value after {flag}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args(a: &[&str]) -> Vec<OsString> {
        a.iter().map(OsString::from).collect()
    }
    #[test]
    fn aliases() {
        let p = Options::parse(args(&["/tmp", "--home", "/Users/test", "--dry-run", "-e"]))
            .unwrap()
            .unwrap();
        assert!(p.estimate && p.dry_run && !p.restore);
        let p = Options::parse(args(&["-r", "/tmp/a.tar.gz", "--home", "/Users/test"]))
            .unwrap()
            .unwrap();
        assert!(p.restore);
        let p = Options::parse(args(&["/tmp", "--metadata-only", "--dry-run"]))
            .unwrap()
            .unwrap();
        assert!(p.metadata_only && p.dry_run);
    }
    #[test]
    fn short_h_shows_help_instead_of_selecting_home() {
        for a in [&["-h"][..], &["--help"], &["/tmp", "-h", "/Users/test"]] {
            assert!(Options::parse(args(a)).unwrap().is_none());
        }
    }
    #[test]
    fn rejects_invalid_flags() {
        for a in [
            &["/tmp", "-e"][..],
            &["-r", "a", "--dry-run", "-e"],
            &["--home"],
            &["/tmp", "/other"],
            &["--unknown"],
            &["--finalize", "a", "-r", "b"],
            &["--finalize", "a", "--dry-run", "-e"],
            &["-r", "a", "--metadata-only"],
            &["--finalize", "a", "--metadata-only"],
            &["/tmp", "--metadata-only", "--dry-run", "-e"],
        ] {
            assert!(Options::parse(args(a)).is_err(), "{a:?}");
        }
    }
}
