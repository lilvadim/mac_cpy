use crate::{
    Result, check_cancel,
    config::Config,
    policy,
    progress::{self, Progress, Unit},
};
use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    fs::{self, File},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

pub(crate) struct FileInfo {
    pub path: PathBuf,
    pub size: u64,
    pub hardlink: Option<(u64, u64)>,
}

#[derive(Clone)]
pub(crate) struct ScanProblem {
    pub relative: Option<PathBuf>,
    pub reason: String,
}

#[derive(Default)]
pub(crate) struct Scan {
    pub included: BTreeSet<PathBuf>,
    pub files: Vec<FileInfo>,
    pub problems: BTreeMap<PathBuf, ScanProblem>,
    pub gitignored: BTreeSet<PathBuf>,
    pub gitignore_enabled: bool,
}

impl Scan {
    pub fn merge_problems(&mut self, other: &Scan) {
        for (path, problem) in &other.problems {
            self.problems
                .entry(path.clone())
                .or_insert_with(|| problem.clone());
        }
    }

    pub fn skips(&self, relative: &Path) -> bool {
        self.problems.values().any(|p| {
            p.relative
                .as_ref()
                .is_some_and(|path| relative.starts_with(path))
        })
    }

    pub fn entries(&self) -> u64 {
        self.included
            .iter()
            .filter(|path| !self.skips(path))
            .count() as u64
    }

    pub fn accepted_files(&self) -> impl Iterator<Item = &FileInfo> {
        self.files.iter().filter(|file| !self.skips(&file.path))
    }

    pub fn print_gitignored(&self) {
        if self.gitignore_enabled {
            println!("\nGITIGNORE EXCLUSIONS ({})", self.gitignored.len());
            for path in &self.gitignored {
                println!("{}", path.display());
            }
        }
    }

    pub fn report(&self) -> String {
        let mut report = format!("SKIPPED PATHS ({})\n", self.problems.len());
        for (path, problem) in &self.problems {
            report.push_str(&format!("  {}: {}\n", path.display(), problem.reason));
        }
        report
    }

    pub fn print_skipped(&self) {
        if !self.problems.is_empty() {
            print!("\n{}", self.report());
        }
    }

    pub fn record_error(&mut self, path: &Path, relative: Option<&Path>, e: &std::io::Error) {
        if self.problems.contains_key(path) {
            return;
        }
        self.problems.insert(
            path.into(),
            ScanProblem {
                relative: relative.map(Path::to_path_buf),
                reason: e.to_string(),
            },
        );
        progress::log(&format!("WARNING: Skipping {}: {e}", path.display()));
    }
}

struct Scanner<'a> {
    home: &'a Path,
    config: &'a Config,
    result: Scan,
    seen: HashSet<PathBuf>,
    gitignore: Option<crate::gitignore::Rules>,
    progress: Progress,
}

impl Scanner<'_> {
    fn visit(&mut self, relative: &Path) -> Result<()> {
        check_cancel()?;
        policy::validate(relative)?;
        let forced = relative
            .ancestors()
            .any(|p| self.config.forced_includes.contains(p));
        if !self.seen.insert(relative.into())
            || self.result.skips(relative)
            || policy::reserved(relative)
            || self
                .config
                .run_excludes
                .iter()
                .any(|p| relative.starts_with(p))
            || (!forced && self.config.exclude_rules.matches(relative))
        {
            return Ok(());
        }
        let absolute = self.home.join(relative);
        let metadata = match fs::symlink_metadata(&absolute) {
            Ok(metadata) => metadata,
            Err(e) => {
                self.result.record_error(&absolute, Some(relative), &e);
                return Ok(());
            }
        };
        if !forced
            && let Some(rules) = &mut self.gitignore
            && rules.ignored(relative, metadata.is_dir())?
        {
            self.result.gitignored.insert(relative.into());
            return Ok(());
        }
        crate::checks::directive_collision(self.home, relative)?;
        let mut children = Vec::new();
        if metadata.is_dir() {
            let entries = match fs::read_dir(&absolute) {
                Ok(entries) => entries,
                Err(e) => {
                    self.result.record_error(&absolute, Some(relative), &e);
                    return Ok(());
                }
            };
            for entry in entries {
                match entry {
                    Ok(entry) => children.push(entry.file_name()),
                    Err(e) => {
                        // Without an entry name, the entire directory cannot be scanned reliably.
                        self.result.record_error(&absolute, Some(relative), &e);
                        return Ok(());
                    }
                }
            }
            children.sort();
        } else if metadata.is_file() {
            // Probe readability without reading file contents.
            if let Err(e) = File::open(&absolute) {
                self.result.record_error(&absolute, Some(relative), &e);
                return Ok(());
            }
            self.result.files.push(FileInfo {
                path: relative.into(),
                size: metadata.len(),
                hardlink: (metadata.nlink() > 1).then(|| (metadata.dev(), metadata.ino())),
            });
        }
        self.result.included.insert(relative.into());
        self.progress.set(self.result.included.len() as u64);
        for child in children {
            self.visit(&relative.join(child))?;
        }
        Ok(())
    }
}

pub(crate) fn collect(home: &Path, selected: &[PathBuf], config: &Config) -> Result<Scan> {
    let mut scanner = Scanner {
        home,
        config,
        result: Scan {
            gitignore_enabled: config.gitignore,
            ..Scan::default()
        },
        seen: HashSet::new(),
        gitignore: config.gitignore.then(|| crate::gitignore::Rules::new(home)),
        progress: Progress::new("Scanning", None, Unit::Items),
    };
    for relative in selected {
        scanner.visit(relative)?;
    }
    // System metadata is checked separately, not counted as home data.
    if let Err(e) = File::open("/etc/hosts") {
        scanner
            .result
            .record_error(Path::new("/etc/hosts"), None, &e);
    }
    scanner.progress.finish(true);
    Ok(scanner.result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn collects_metadata_once_without_following_links_or_reading_excluded_subtrees() {
        let t = tempfile::tempdir().unwrap();
        let home = t.path().join("home");
        let config_dir = t.path().join("config");
        fs::create_dir_all(home.join(".cargo/registry")).unwrap();
        fs::create_dir_all(home.join("Documents")).unwrap();
        fs::create_dir(&config_dir).unwrap();
        for name in crate::config::NAMES {
            fs::write(config_dir.join(name), "").unwrap();
        }
        fs::write(
            config_dir.join(crate::config::NAMES[0]),
            ".cargo/registry\n",
        )
        .unwrap();
        let config = Config::load(&config_dir).unwrap();
        fs::write(home.join("Documents/a"), b"abc").unwrap();
        fs::hard_link(home.join("Documents/a"), home.join("Documents/b")).unwrap();
        symlink("/", home.join("Documents/outside")).unwrap();
        fs::write(home.join(".cargo/registry/ignored"), vec![0; 1000]).unwrap();
        let scan = collect(
            &home,
            &[".cargo".into(), "Documents".into(), "Documents/a".into()],
            &config,
        )
        .unwrap();
        assert_eq!(scan.files.len(), 2);
        assert_eq!(scan.entries(), 5);
        assert!(scan.included.contains(Path::new("Documents/outside")));
        assert!(!scan.included.contains(Path::new(".cargo/registry")));
        assert!(scan.files.iter().all(|file| file.size == 3));
        assert_eq!(scan.files[0].hardlink, scan.files[1].hardlink);
        assert!(scan.problems.is_empty());
    }
}
