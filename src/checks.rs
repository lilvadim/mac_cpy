use crate::{Result, config::GITIGNORE_DIRECTIVE, error};
use std::{
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
};

pub(crate) fn directive_collision(home: &Path, relative: &Path) -> Result<()> {
    if relative == Path::new(GITIGNORE_DIRECTIVE) {
        Err(error(format!(
            "{} conflicts with the configuration directive {GITIGNORE_DIRECTIVE}. Temporarily rename this file or directory before running the backup.",
            home.join(relative).display()
        )))
    } else {
        Ok(())
    }
}

pub(crate) fn before_backup(selected: &[PathBuf]) -> Result<()> {
    // Add other application-specific consistency checks here.
    require_closed(
        selected,
        &["Library/Application Support/Firefox"],
        "firefox|Firefox",
        "Close Firefox completely before backing up its session and settings",
    )?;
    require_closed(
        selected,
        &[".codex", "Library/Application Support/Codex"],
        "codex|Codex|Codex Helper.*|Codex [(](Service|Renderer)[)]",
        "Close Codex completely (desktop app and CLI sessions) before backing up its data",
    )?;
    Ok(())
}

fn needs_check(selected: &[PathBuf], scopes: &[&str]) -> bool {
    selected.iter().any(|path| {
        scopes.iter().any(|scope| {
            let scope = Path::new(scope);
            path.starts_with(scope)
        })
    })
}

fn require_closed(
    selected: &[PathBuf],
    scopes: &[&str],
    process: &str,
    message: &str,
) -> Result<()> {
    if !needs_check(selected, scopes) {
        return Ok(());
    }
    let status = Command::new("/usr/bin/pgrep")
        .args(["-x", process])
        .stdout(Stdio::null())
        .status()?;
    check_status(status, message)
}

fn check_status(status: ExitStatus, message: &str) -> Result<()> {
    match status.code() {
        Some(0) => Err(error(message)),
        Some(1) => Ok(()),
        _ => Err(error(format!("Cannot check running processes: {status}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;
    #[test]
    fn checks_only_selected_scopes_and_rejects_running_processes() {
        let scopes = &["Library/Application Support/Firefox"];
        assert!(before_backup(&[]).is_ok());
        assert!(require_closed(&[PathBuf::from("Documents")], scopes, "[", "running").is_ok());
        assert_eq!(
            check_status(ExitStatus::from_raw(0), "running")
                .unwrap_err()
                .to_string(),
            "running"
        );
        assert!(check_status(ExitStatus::from_raw(1 << 8), "running").is_ok());
        assert!(check_status(ExitStatus::from_raw(3 << 8), "running").is_err());
    }

    #[test]
    fn codex_check_uses_actual_data_paths_and_skips_unrelated_ancestors() {
        let scopes = &[".codex", "Library/Application Support/Codex"];
        for path in [
            ".codex",
            ".codex/sessions",
            "Library/Application Support/Codex",
            "Library/Application Support/Codex/state.db",
        ] {
            assert!(needs_check(&[PathBuf::from(path)], scopes), "{path}");
        }
        for path in [
            "Documents",
            "Library",
            ".codex-other",
            "Library/Application Support/Codex-other",
        ] {
            assert!(!needs_check(&[PathBuf::from(path)], scopes), "{path}");
        }
        assert!(!needs_check(&[], scopes));
    }
}
