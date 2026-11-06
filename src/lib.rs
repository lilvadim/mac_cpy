mod archive;
mod checks;
mod cli;
mod config;
mod estimate;
mod gitignore;
mod includes;
mod layout;
mod metadata;
mod patterns;
mod pkg_inventory;
mod policy;
mod process;
mod progress;
mod restore_plan;
mod scan;
mod storage;

use std::{
    ffi::OsString,
    fs, io,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

pub(crate) type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    INTERRUPTED.store(true, Ordering::Relaxed);
}

pub fn initialize() {
    progress::start_run();
    // Only an atomic store runs in the handler; cleanup happens in normal Rust code.
    unsafe {
        libc::umask(0o077);
        libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
    }
}

pub fn interrupted() -> bool {
    INTERRUPTED.load(Ordering::Relaxed)
}

pub(crate) fn check_cancel() -> Result<()> {
    if interrupted() {
        Err(io::Error::new(io::ErrorKind::Interrupted, "Interrupted").into())
    } else {
        Ok(())
    }
}

pub(crate) fn error(message: impl Into<String>) -> Box<dyn std::error::Error + Send + Sync> {
    io::Error::other(message.into()).into()
}

pub(crate) fn exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

pub fn run(args: Vec<OsString>) -> Result<()> {
    let Some(options) = cli::Options::parse(args)? else {
        return Ok(());
    };
    if !cfg!(target_os = "macos") {
        return Err(error("This utility requires macOS and its built-in bsdtar"));
    }
    // Never run a whole-user migration as root by accident.
    if unsafe { libc::geteuid() } == 0 {
        return Err(error("Run as your normal user, without sudo"));
    }
    if options.finalize {
        return archive::finalize(&options.location, options.dry_run);
    }
    let home = fs::canonicalize(&options.home)?;
    if home == Path::new("/") || !home.is_dir() {
        return Err(error("Invalid home directory"));
    }
    match options.restore {
        true => archive::restore(&options.location, &home, options.dry_run),
        false => {
            let destination = fs::canonicalize(&options.location)?;
            if !destination.is_dir() {
                return Err(error("Output must be an existing directory"));
            }
            let archive = archive::backup_path(&destination)?;
            let mut config = config::Config::discover()?;
            for path in archive::output_paths(&archive) {
                if let Ok(relative) = path.strip_prefix(&home) {
                    policy::validate(relative)?;
                    config.run_excludes.push(relative.into());
                }
            }
            let mut selection_issues = scan::Scan::default();
            let selected = if options.metadata_only {
                Vec::new()
            } else {
                policy::select(&home, &mut config, &mut selection_issues)?
            };
            let steps = if !options.dry_run {
                4
            } else if options.estimate {
                3
            } else {
                1
            };
            if options.metadata_only {
                println!("[1/{steps}] Checking metadata sources (home files excluded)...");
            } else {
                println!("[1/{steps}] Scanning: {}", home.display());
            }
            if options.dry_run {
                if options.metadata_only {
                    println!(
                        "CONFIG: {}\n\nMETADATA ONLY\n/etc/hosts\nBrewfile and package versions\nApplication inventory\nmacOS and source information\nmac_cpy executable and configuration snapshots\nNo home files selected.",
                        config.directory.display()
                    );
                } else {
                    policy::print_selection(&selected, &config);
                }
                if options.estimate {
                    println!("[2/3] Estimating size...");
                    let mut scan = scan::collect(&home, &selected, &config)?;
                    scan.merge_problems(&selection_issues);
                    scan.print_gitignored();
                    estimate::print(&scan)?;
                    println!("[3/3] Testing read + gzip + destination write (up to 64 MiB)...");
                    if let Err(e) = estimate::benchmark(&home, &destination, &scan) {
                        if interrupted() {
                            scan.print_skipped();
                        }
                        check_cancel()?;
                        eprintln!("WARNING: Pipeline test failed: {e}");
                    }
                    println!("DRY RUN complete. No archive created; speed-test file removed.");
                    scan.print_skipped();
                } else {
                    let scan_roots = if config.gitignore {
                        Some(selected.as_slice())
                    } else {
                        selected
                            .iter()
                            .find(|path| path.as_path() == Path::new(config::GITIGNORE_DIRECTIVE))
                            .map(std::slice::from_ref)
                    };
                    if let Some(roots) = scan_roots.filter(|_| !options.metadata_only) {
                        let mut scan = scan::collect(&home, roots, &config)?;
                        scan.merge_problems(&selection_issues);
                        scan.print_gitignored();
                        scan.print_skipped();
                    } else {
                        selection_issues.print_skipped();
                    }
                    if options.metadata_only {
                        println!("DRY RUN complete. Destination unchanged.");
                    } else {
                        println!(
                            "DRY RUN complete. Destination unchanged. Add --estimate for size/time estimates."
                        );
                    }
                }
                Ok(())
            } else {
                // Skip inaccessible source paths for this run, without editing the user's configs.
                let mut scan = scan::collect(&home, &selected, &config)?;
                scan.merge_problems(&selection_issues);
                let result = archive::backup(
                    &home,
                    &archive,
                    &selected,
                    &config,
                    &scan,
                    options.metadata_only,
                );
                scan.print_skipped();
                result
            }
        }
    }
}
