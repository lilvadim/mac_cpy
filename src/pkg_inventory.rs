use crate::{Result, check_cancel, error, process, progress};
use serde_json::Value;
use std::{
    collections::BTreeSet,
    path::{Component, Path, PathBuf},
    process::Command,
};

// Homebrew's pkg artifact does not list the app bundles installed by the package.
// Its uninstall.pkgutil expressions identify receipts that can supply those paths.
pub(crate) fn applications(casks: &[Value]) -> Result<BTreeSet<PathBuf>> {
    let mut receipts = BTreeSet::new();
    for pattern in receipt_patterns(casks) {
        check_cancel()?;
        match process::output(Command::new("/usr/sbin/pkgutil").arg(format!("--pkgs={pattern}"))) {
            Ok(bytes) => {
                for id in std::str::from_utf8(&bytes)?
                    .lines()
                    .filter(|id| !id.is_empty())
                {
                    receipts.insert(id.to_owned());
                }
            }
            Err(err) => {
                check_cancel()?;
                progress::log(&format!(
                    "WARNING: Cannot resolve Homebrew pkg receipts for {pattern:?}: {err}; related apps may remain in Applications-not-in-Brew.txt."
                ));
            }
        }
    }
    let mut apps = BTreeSet::new();
    for id in receipts {
        check_cancel()?;
        match receipt_apps(&id) {
            Ok(paths) => apps.extend(paths),
            Err(err) => {
                check_cancel()?;
                progress::log(&format!(
                    "WARNING: Cannot read Homebrew pkg receipt {id:?}: {err}; related apps may remain in Applications-not-in-Brew.txt."
                ));
            }
        }
    }
    Ok(apps)
}

fn receipt_patterns(casks: &[Value]) -> BTreeSet<String> {
    let mut patterns = BTreeSet::new();
    for cask in casks {
        let Some(artifacts) = cask["artifacts"].as_array() else {
            continue;
        };
        if !artifacts.iter().any(|a| a.get("pkg").is_some()) {
            continue;
        }
        for artifact in artifacts {
            if let Some(uninstalls) = artifact["uninstall"].as_array() {
                for uninstall in uninstalls {
                    match &uninstall["pkgutil"] {
                        Value::String(pattern) => {
                            patterns.insert(pattern.clone());
                        }
                        Value::Array(values) => {
                            patterns
                                .extend(values.iter().filter_map(Value::as_str).map(str::to_owned));
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    patterns
}

fn receipt_apps(id: &str) -> Result<BTreeSet<PathBuf>> {
    let info = process::output(Command::new("/usr/sbin/pkgutil").args(["--pkg-info", id]))?;
    let base = install_base(std::str::from_utf8(&info)?)?;
    let files = process::output(Command::new("/usr/sbin/pkgutil").args(["--files", id]))?;
    Ok(installed_apps(&base, std::str::from_utf8(&files)?))
}

fn install_base(info: &str) -> Result<PathBuf> {
    let field = |key: &str| {
        info.lines()
            .find_map(|line| line.strip_prefix(key))
            .map(str::trim)
    };
    let volume = Path::new(field("volume:").ok_or_else(|| error("Receipt has no volume"))?);
    let location =
        Path::new(field("location:").ok_or_else(|| error("Receipt has no install location"))?);
    if !volume.is_absolute()
        || location
            .components()
            .any(|c| matches!(c, Component::ParentDir))
    {
        return Err(error("Invalid receipt install location"));
    }
    Ok(volume.join(location.strip_prefix("/").unwrap_or(location)))
}

fn installed_apps(base: &Path, files: &str) -> BTreeSet<PathBuf> {
    let mut apps = BTreeSet::new();
    for line in files.lines() {
        let path = Path::new(line);
        if path.is_absolute() || path.components().any(|c| matches!(c, Component::ParentDir)) {
            continue;
        }
        let mut target = base.to_path_buf();
        for component in path.components() {
            target.push(component.as_os_str());
            if target.extension().is_some_and(|ext| ext == "app") {
                apps.insert(target);
                break;
            }
        }
    }
    apps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_pkg_casks_supply_receipt_patterns_in_string_or_array_form() {
        let casks = serde_json::json!([
            {"artifacts": [{"pkg": ["Example.pkg"]}, {"uninstall": [{"pkgutil": ["org.example.*", "org.helper"]}]}]},
            {"artifacts": [{"pkg": ["Other.pkg"]}, {"uninstall": [{"pkgutil": "org.other"}]}]},
            {"artifacts": [{"app": ["Manual.app"]}, {"uninstall": [{"pkgutil": "must.not.match"}]}]}
        ]);
        assert_eq!(
            receipt_patterns(casks.as_array().unwrap()),
            BTreeSet::from([
                "org.example.*".into(),
                "org.helper".into(),
                "org.other".into()
            ])
        );
    }

    #[test]
    fn receipt_paths_respect_install_location_and_match_complete_app_bundles() {
        let base =
            install_base("package-id: org.example\nvolume: /\nlocation: Applications\n").unwrap();
        let apps = installed_apps(
            &base,
            "./Example.app/Contents/Info.plist\nExample.app\nTools/Helper.app/Contents/MacOS/helper\n../Manual.app\n/Applications/Other.app\nExample.app/Contents/PlugIns/Inner.app\n",
        );
        assert_eq!(
            apps,
            BTreeSet::from([
                PathBuf::from("/Applications/Example.app"),
                PathBuf::from("/Applications/Tools/Helper.app")
            ])
        );
        assert_eq!(
            install_base("volume: /\nlocation: \n").unwrap(),
            Path::new("/")
        );
        assert_eq!(
            install_base("volume: /\nlocation: /Applications\n").unwrap(),
            Path::new("/Applications")
        );
        assert!(install_base("volume: /\nlocation: ../Applications\n").is_err());
        assert!(install_base("location: Applications\n").is_err());
    }
}
