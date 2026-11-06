use crate::{Result, check_cancel, error};
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use std::{
    collections::HashMap,
    fs, io,
    path::{Path, PathBuf},
};

pub(crate) struct Rules {
    home: PathBuf,
    matchers: HashMap<PathBuf, Gitignore>,
    directories: HashMap<PathBuf, bool>,
}

impl Rules {
    pub fn new(home: &Path) -> Self {
        Self {
            home: home.into(),
            matchers: HashMap::new(),
            directories: HashMap::new(),
        }
    }

    pub fn ignored(&mut self, relative: &Path, directory: bool) -> Result<bool> {
        check_cancel()?;
        if relative.as_os_str().is_empty() || relative == Path::new(".") {
            return Ok(false);
        }
        if directory && let Some(value) = self.directories.get(relative) {
            return Ok(*value);
        }
        let parent = relative.parent().unwrap_or(Path::new(""));
        // Git cannot re-include a child when its parent directory is excluded.
        if self.ignored(parent, true)? {
            if directory {
                self.directories.insert(relative.into(), true);
            }
            return Ok(true);
        }
        let mut parents: Vec<_> = parent.ancestors().collect();
        parents.reverse();
        let mut ignored = false;
        for base in parents {
            if !self.matchers.contains_key(base) {
                let folder = self.home.join(base);
                let file = folder.join(".gitignore");
                let mut builder = GitignoreBuilder::new(&folder);
                match fs::symlink_metadata(&file) {
                    Ok(metadata) if metadata.is_file() => {
                        if let Some(e) = builder.add(&file) {
                            return Err(error(format!("Cannot load {}: {e}", file.display())));
                        }
                    }
                    Ok(_) => {} // Git does not follow symbolic links to .gitignore files.
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => return Err(error(format!("Cannot inspect {}: {e}", file.display()))),
                }
                self.matchers.insert(base.into(), builder.build()?);
            }
            let matched = self.matchers[base].matched(self.home.join(relative), directory);
            if !matched.is_none() {
                ignored = matched.is_ignore();
            }
        }
        if directory {
            self.directories.insert(relative.into(), ignored);
        }
        Ok(ignored)
    }
}
