use crate::{Result, check_cancel, error, patterns, policy};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

pub(crate) fn expand(
    home: &Path,
    pattern: &str,
    issues: &mut crate::scan::Scan,
) -> Result<BTreeSet<PathBuf>> {
    let mut result = BTreeSet::new();
    if !patterns::is_pattern(pattern) {
        if !directory_prefix(
            home,
            Path::new(pattern).parent().unwrap_or(Path::new("")),
            issues,
        )? {
            return Ok(result);
        }
        if !policy::reserved(Path::new(pattern)) {
            let path = home.join(pattern);
            match std::fs::symlink_metadata(&path) {
                Ok(_) => {
                    result.insert(PathBuf::from(pattern));
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => issues.record_error(&path, Some(Path::new(pattern)), &e),
            }
        }
        return Ok(result);
    }
    let matcher = patterns::compile(pattern)?;
    let mut base = PathBuf::new();
    for component in pattern.split('/') {
        if patterns::is_pattern(component) {
            break;
        }
        base.push(component);
    }
    let root = home.join(&base);
    // An explicit literal prefix narrows traversal; '*' never crosses a separator.
    let mut walk = walkdir::WalkDir::new(&root)
        .follow_links(false)
        .follow_root_links(false);
    if !pattern.contains("**") {
        walk = walk.max_depth(
            pattern
                .split('/')
                .count()
                .saturating_sub(base.components().count()),
        );
    }
    if !directory_prefix(home, &base, issues)? {
        return Ok(result);
    }
    for entry in walk.into_iter().filter_entry(|e| {
        e.path()
            .strip_prefix(home)
            .is_ok_and(|p| !policy::reserved(p))
    }) {
        check_cancel()?;
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                if let (Some(path), Some(reason)) = (e.path(), e.io_error()) {
                    issues.record_error(path, Some(path.strip_prefix(home)?), reason);
                    continue;
                }
                return Err(error(format!("Cannot expand include {pattern:?}: {e}")));
            }
        };
        let relative = entry.path().strip_prefix(home)?;
        if !relative.as_os_str().is_empty() && matcher.is_match(relative) {
            policy::validate(relative)?;
            result.insert(relative.into());
        }
    }
    Ok(result)
}

fn directory_prefix(home: &Path, relative: &Path, issues: &mut crate::scan::Scan) -> Result<bool> {
    // Literal includes and glob prefixes must never traverse directory links.
    let mut prefix = home.to_path_buf();
    for component in relative.components() {
        prefix.push(component);
        match std::fs::symlink_metadata(&prefix) {
            Ok(metadata) if metadata.file_type().is_symlink() => return Ok(false),
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(e) => {
                issues.record_error(&prefix, Some(prefix.strip_prefix(home)?), &e);
                return Ok(false);
            }
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        os::unix::fs::{PermissionsExt, symlink},
    };

    fn expand(home: &Path, pattern: &str) -> Result<BTreeSet<PathBuf>> {
        super::expand(home, pattern, &mut crate::scan::Scan::default())
    }

    #[test]
    fn inaccessible_glob_directories_are_reported_and_other_matches_survive() {
        let home = tempfile::tempdir().unwrap();
        let blocked = home.path().join("Library/Vendor/blocked");
        let allowed = home.path().join("Library/Vendor/allowed/options");
        fs::create_dir_all(&blocked).unwrap();
        fs::create_dir_all(&allowed).unwrap();
        fs::set_permissions(&blocked, fs::Permissions::from_mode(0o000)).unwrap();
        let mut issues = crate::scan::Scan::default();
        let result = super::expand(home.path(), "Library/Vendor/*/options", &mut issues);
        fs::set_permissions(&blocked, fs::Permissions::from_mode(0o700)).unwrap();
        let paths = result.unwrap();
        assert!(paths.contains(Path::new("Library/Vendor/allowed/options")));
        assert!(issues.problems.contains_key(&blocked));
        assert!(issues.report().contains("Library/Vendor/blocked"));
    }

    #[test]
    fn shell_globs_match_versions_braces_classes_and_recursive_paths() {
        let t = tempfile::tempdir().unwrap();
        let home = t.path();
        for path in [
            "Library/Vendor/IDE2026/options/a",
            "Library/Vendor/IDE2027/keymaps/a",
            "Library/Vendor/IDE2027/cache/a",
            "Library/Vendor/IDE2027/deep/options/a",
        ] {
            fs::create_dir_all(home.join(path).parent().unwrap()).unwrap();
            fs::write(home.join(path), b"fixture").unwrap();
        }
        let paths = expand(home, "Library/Vendor/IDE202?/{options,keymaps}").unwrap();
        assert_eq!(paths.len(), 2);
        assert!(paths.contains(Path::new("Library/Vendor/IDE2026/options")));
        assert!(paths.contains(Path::new("Library/Vendor/IDE2027/keymaps")));
        assert_eq!(expand(home, "Library/Vendor/**/options").unwrap().len(), 2);
        assert_eq!(
            expand(home, "Library/Vendor/IDE202[67]/options")
                .unwrap()
                .len(),
            1
        );
        assert!(
            expand(home, "Library/Missing/*/options")
                .unwrap()
                .is_empty()
        );
        let outside = tempfile::tempdir().unwrap();
        fs::create_dir(outside.path().join("options")).unwrap();
        symlink(outside.path(), home.join("Library/Vendor/Linked")).unwrap();
        assert!(expand(home, "Library/Vendor/Linked/*").unwrap().is_empty());
        assert!(
            expand(home, "Library/Vendor/Linked/options")
                .unwrap()
                .is_empty()
        );
        assert!(
            expand(home, "Library/Vendor/Linked")
                .unwrap()
                .contains(Path::new("Library/Vendor/Linked"))
        );
        assert_eq!(expand(home, "Library/Vendor/*/options").unwrap().len(), 1);
        assert!(
            expand(home, "**")
                .unwrap()
                .contains(Path::new("Library/Vendor/IDE2026/options/a"))
        );
    }
}
