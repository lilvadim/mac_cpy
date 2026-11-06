use crate::{
    Result,
    config::{Config, GITIGNORE_DIRECTIVE},
    error,
    layout::{INFO_DIR, OVERWRITTEN_DIR},
};
use std::{
    collections::BTreeSet,
    fs,
    path::{Component, Path, PathBuf},
};

// These private archive/rollback directories are reserved, not user-selectable data.
pub(crate) fn reserved(path: &Path) -> bool {
    path.starts_with(INFO_DIR) || path.starts_with(OVERWRITTEN_DIR)
}

pub(crate) fn validate(path: &Path) -> Result<()> {
    let text = path
        .to_str()
        .ok_or_else(|| error("Non-UTF-8 paths are not supported"))?;
    if text.is_empty()
        || text.contains('\\')
        || text.chars().any(char::is_control)
        || path.is_absolute()
        || path.components().any(|p| {
            matches!(
                p,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(error(format!(
            "Unsafe or unsupported relative path: {text:?}"
        )));
    }
    Ok(())
}

#[derive(Clone, Default)]
pub(crate) struct Exclusions(Vec<globset::GlobMatcher>);

impl Exclusions {
    pub fn new(patterns: &[String]) -> Result<Self> {
        Ok(Self(
            patterns
                .iter()
                .map(|p| crate::patterns::compile(p))
                .collect::<Result<_>>()?,
        ))
    }

    pub fn matches(&self, path: &Path) -> bool {
        path.ancestors()
            .any(|ancestor| self.0.iter().any(|pattern| pattern.is_match(ancestor)))
    }
}

pub(crate) fn select(
    home: &Path,
    config: &mut Config,
    issues: &mut crate::scan::Scan,
) -> Result<Vec<PathBuf>> {
    let mut paths = BTreeSet::new();
    for entry in fs::read_dir(home)? {
        let entry = entry?;
        let rel = PathBuf::from(entry.file_name());
        validate(&rel)?;
        if !reserved(&rel) && !config.root_excludes.iter().any(|p| rel == Path::new(p)) {
            paths.insert(rel);
        }
    }
    config.forced_includes.clear();
    for rel in &config.includes {
        config
            .forced_includes
            .extend(crate::includes::expand(home, rel, issues)?);
    }
    paths.extend(config.forced_includes.iter().cloned());
    for path in &paths {
        validate(path)?;
    }
    let mut selected: Vec<PathBuf> = Vec::new();
    for path in paths {
        if !config.run_excludes.iter().any(|p| path.starts_with(p))
            && (config.forced_includes.contains(&path)
                || !selected.iter().any(|parent| path.starts_with(parent)))
        {
            selected.push(path);
        }
    }
    Ok(selected)
}

pub(crate) fn print_selection(paths: &[PathBuf], config: &Config) {
    println!("CONFIG: {}", config.directory.display());
    println!("\nINCLUDED PATHS (directories are recursive)");
    for path in paths {
        println!("{}", path.display());
    }
    println!("\nEXCLUDED ROOTS");
    for p in &config.root_excludes {
        println!("{p}");
    }
    println!("Always reserved: {INFO_DIR}, {OVERWRITTEN_DIR}");
    println!("Selected exceptions are listed above.");
    println!("\nFORCED INCLUDES (override exclude and .gitignore)");
    for path in &config.forced_includes {
        println!("{}", path.display());
    }
    println!("\nEXCLUDE RULES (except forced includes)");
    for p in &config.excludes {
        println!("{p}");
    }
    if config.gitignore {
        println!("{GITIGNORE_DIRECTIVE} (local rules, including nested files)");
    }
    if !config.run_excludes.is_empty() {
        println!("\nEXCLUDED FOR THIS RUN (current backup output only)");
        for path in &config.run_excludes {
            println!("{}", path.display());
        }
    }
    println!(
        "\nADDITIONAL METADATA\n/etc/hosts\nBrewfile, package versions, app inventory, executable\n"
    );
}

pub(crate) fn tar_exclusion(path: &Path) -> String {
    let mut pattern = String::from("./");
    for c in path.to_str().unwrap().chars() {
        if matches!(c, '*' | '?' | '[' | ']' | '\\') {
            pattern.push('\\');
        }
        pattern.push(c);
    }
    format!("--exclude={pattern}")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn safe_paths() {
        for p in ["/etc/passwd", "../outside", "a/../../b", "a\nb", "a\\b"] {
            assert!(validate(Path::new(p)).is_err());
        }
        assert!(validate(Path::new("./Library/Application Support/Firefox")).is_ok());
    }
    #[test]
    fn exclusion_boundaries() {
        let excludes = Exclusions::new(&[
            ".cargo/registry".into(),
            "**/.DS_Store".into(),
            "node_modules".into(),
            "myprojects/**/target".into(),
        ])
        .unwrap();
        assert!(excludes.matches(Path::new(".cargo/registry/index/file")));
        assert!(!excludes.matches(Path::new(".cargo/registry-custom/file")));
        assert!(excludes.matches(Path::new("Documents/.DS_Store")));
        assert!(excludes.matches(Path::new("node_modules/package/file")));
        assert!(!excludes.matches(Path::new("project/node_modules/package/file")));
        assert!(excludes.matches(Path::new("myprojects/demo/target/classes/A.class")));
        assert!(!excludes.matches(Path::new("other/target/classes/A.class")));
    }
}
