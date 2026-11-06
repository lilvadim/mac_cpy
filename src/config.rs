use crate::{Result, error, policy};
use std::{
    collections::BTreeSet,
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
};

pub(crate) const NAMES: [&str; 3] = ["exclude.mac_cpy", "root_exclude.mac_cpy", "include.mac_cpy"];
pub(crate) const GITIGNORE_DIRECTIVE: &str = "@GITIGNORE";

pub(crate) struct Config {
    pub directory: PathBuf,
    pub excludes: Vec<String>,
    pub exclude_rules: policy::Exclusions,
    pub gitignore: bool,
    pub root_excludes: Vec<String>,
    pub includes: Vec<String>,
    pub forced_includes: BTreeSet<PathBuf>,
    pub run_excludes: Vec<PathBuf>,
    originals: Vec<String>,
}

impl Config {
    pub fn discover() -> Result<Self> {
        let exe = std::env::current_exe()?;
        let directory = exe
            .parent()
            .ok_or_else(|| error("Executable directory unavailable"))?;
        let user_config = match std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
            Some(root) => {
                let root = PathBuf::from(root);
                if !root.is_absolute() {
                    return Err(error("XDG_CONFIG_HOME must be an absolute path"));
                }
                Some(root.join("mac_cpy"))
            }
            None => std::env::var_os("HOME")
                .filter(|v| !v.is_empty())
                .map(|home| PathBuf::from(home).join(".config/mac_cpy")),
        };
        Self::resolve(user_config.as_deref(), directory)
    }

    fn resolve(user_config: Option<&Path>, executable_directory: &Path) -> Result<Self> {
        if let Some(directory) = user_config {
            match fs::symlink_metadata(directory) {
                Ok(_) => return Self::load(directory),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    return Err(error(format!(
                        "Cannot inspect configuration directory {}: {e}",
                        directory.display()
                    )));
                }
            }
        }
        Self::load(executable_directory)
    }

    pub fn load(directory: &Path) -> Result<Self> {
        let mut originals = Vec::new();
        let mut lists = Vec::new();
        for name in NAMES {
            let path = directory.join(name);
            let file = File::open(&path)
                .map_err(|e| error(format!("Cannot read configuration {}: {e}", path.display())))?;
            let mut text = String::new();
            file.take(1024 * 1024 + 1)
                .read_to_string(&mut text)
                .map_err(|e| error(format!("Cannot read configuration {}: {e}", path.display())))?;
            if text.len() > 1024 * 1024 {
                return Err(error(format!(
                    "Configuration too large: {}",
                    path.display()
                )));
            }
            lists.push(parse(&text, name)?);
            originals.push(text);
        }
        let mut lists = lists.into_iter();
        let excluded = lists.next().unwrap();
        let excludes = excluded.paths;
        let gitignore = excluded.gitignore;
        Ok(Self {
            directory: directory.into(),
            exclude_rules: policy::Exclusions::new(&excludes)?,
            excludes,
            gitignore,
            root_excludes: lists.next().unwrap().paths,
            includes: lists.next().unwrap().paths,
            forced_includes: BTreeSet::new(),
            run_excludes: Vec::new(),
            originals,
        })
    }

    pub fn save_beside(&self, executable_directory: &Path) -> Result<()> {
        for (name, content) in NAMES.iter().zip(&self.originals) {
            std::fs::write(executable_directory.join(name), content)?;
        }
        Ok(())
    }
}

struct ParsedPaths {
    paths: Vec<String>,
    gitignore: bool,
}

fn parse(text: &str, name: &str) -> Result<ParsedPaths> {
    let mut seen = BTreeSet::new();
    let mut paths = Vec::new();
    let mut gitignore = false;
    for (index, line) in text.trim_start_matches('\u{feff}').lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line == GITIGNORE_DIRECTIVE {
            if name != NAMES[0] {
                return Err(error(format!(
                    "{name}:{}: {GITIGNORE_DIRECTIVE} is allowed only in exclude.mac_cpy",
                    index + 1
                )));
            }
            gitignore = true;
            continue;
        }
        let path = Path::new(line);
        policy::validate(path).map_err(|e| error(format!("{name}:{}: {e}", index + 1)))?;
        if path.components().all(|p| p == std::path::Component::CurDir)
            || (name == NAMES[1] && crate::patterns::is_pattern(line))
        {
            return Err(error(format!(
                "{name}:{}: expected a literal relative path, not a wildcard or '.'",
                index + 1
            )));
        }
        if name != NAMES[1] && crate::patterns::is_pattern(line) {
            crate::patterns::compile(line)
                .map_err(|e| error(format!("{name}:{}: {e}", index + 1)))?;
        }
        let normalized = path
            .components()
            .filter(|p| *p != std::path::Component::CurDir)
            .collect::<PathBuf>();
        if name == NAMES[1] && normalized.components().count() != 1 {
            return Err(error(format!(
                "{name}:{}: root exclusions must name a single home entry",
                index + 1
            )));
        }
        let normalized = normalized.to_str().unwrap().to_owned();
        if seen.insert(normalized.clone()) {
            paths.push(normalized);
        }
    }
    Ok(ParsedPaths { paths, gitignore })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn directive_is_distinct_from_a_normalized_literal_path() {
        let dir = tempfile::tempdir().unwrap();
        for name in NAMES {
            fs::write(dir.path().join(name), "").unwrap();
        }
        fs::write(
            dir.path().join(NAMES[0]),
            format!("./{GITIGNORE_DIRECTIVE}\n"),
        )
        .unwrap();
        let config = Config::load(dir.path()).unwrap();
        assert!(!config.gitignore);
        assert_eq!(config.excludes, [GITIGNORE_DIRECTIVE]);
        fs::write(
            dir.path().join(NAMES[0]),
            format!("# {GITIGNORE_DIRECTIVE}\n"),
        )
        .unwrap();
        assert!(!Config::load(dir.path()).unwrap().gitignore);
        fs::write(dir.path().join(NAMES[0]), GITIGNORE_DIRECTIVE).unwrap();
        let config = Config::load(dir.path()).unwrap();
        assert!(config.gitignore && config.excludes.is_empty());
    }
    #[test]
    fn accepts_spaces_comments_crlf_and_duplicates() {
        let paths=parse("# comment\r\n\nLibrary/Application Support/Test\r\n./Library/Application Support/Test\n",NAMES[2]).unwrap();
        assert_eq!(paths.paths, ["Library/Application Support/Test"]);
        assert_eq!(parse("./Library", NAMES[1]).unwrap().paths, ["Library"]);
    }
    #[test]
    fn rejects_unsafe_or_ambiguous_paths() {
        let parsed = parse(
            &format!("{GITIGNORE_DIRECTIVE}\n{GITIGNORE_DIRECTIVE}\n"),
            NAMES[0],
        )
        .unwrap();
        assert!(parsed.gitignore && parsed.paths.is_empty());
        assert!(parse(GITIGNORE_DIRECTIVE, NAMES[1]).is_err());
        assert!(parse(GITIGNORE_DIRECTIVE, NAMES[2]).is_err());
        assert!(parse("Library/*/{options,keymaps}", NAMES[2]).is_ok());
        assert!(parse("Library/*/foo", NAMES[0]).is_ok());
        assert!(parse("*", NAMES[1]).is_err());
        assert!(parse("Library/[", NAMES[0]).is_err());
        for line in ["/etc/hosts", "../outside", ".", "foo\\bar"] {
            assert!(parse(line, NAMES[2]).is_err(), "{line}");
        }
        assert!(parse("a/b", NAMES[1]).is_err());
    }
    #[test]
    fn missing_file_fails_instead_of_disabling_exclusions() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Config::load(dir.path()).is_err());
    }

    #[test]
    fn user_config_precedes_portable_config_and_incomplete_config_is_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let portable = dir.path().join("portable");
        let user = dir.path().join("user");
        fs::create_dir(&portable).unwrap();
        for name in NAMES {
            fs::write(portable.join(name), "").unwrap();
        }
        assert_eq!(
            Config::resolve(Some(&user), &portable).unwrap().directory,
            portable
        );
        fs::create_dir(&user).unwrap();
        fs::write(user.join(NAMES[0]), "Documents/skip\n").unwrap();
        assert!(Config::resolve(Some(&user), &portable).is_err());
        for name in &NAMES[1..] {
            fs::write(user.join(name), "").unwrap();
        }
        let config = Config::resolve(Some(&user), &portable).unwrap();
        assert_eq!(config.directory, user);
        assert_eq!(config.excludes, ["Documents/skip"]);
        fs::write(user.join(NAMES[0]), "../invalid\n").unwrap();
        assert!(Config::resolve(Some(&user), &portable).is_err());
    }
}
