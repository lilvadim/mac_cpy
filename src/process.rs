use crate::{
    Result, check_cancel, error,
    progress::{self, Progress, Unit},
};
use std::{
    fs::File,
    io::{BufRead, BufReader, Read},
    process::{Child, Command, Stdio},
    thread,
    time::Duration,
};

pub(crate) struct Guard(pub Child);
impl Drop for Guard {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

pub(crate) fn wait(child: &mut Child) -> Result<()> {
    loop {
        check_cancel()?;
        if let Some(status) = child.try_wait()? {
            return if status.success() {
                Ok(())
            } else {
                Err(error(format!("Command failed: {status}")))
            };
        }
        thread::sleep(Duration::from_millis(20));
    }
}

pub(crate) fn run(command: &mut Command) -> Result<()> {
    check_cancel()?;
    let mut child = Guard(command.stderr(Stdio::piped()).spawn()?);
    let stderr = child.0.stderr.take().unwrap();
    let reader = thread::spawn(move || read_stderr(stderr, None, None));
    let result = wait(&mut child.0);
    drop(child);
    let read_result = reader
        .join()
        .map_err(|_| error("Command log reader panicked"))?;
    result.and(read_result)
}

#[derive(Clone, Copy)]
pub(crate) enum TarMode {
    Create,
    Extract,
    List,
}

pub(crate) fn tar_progress(
    command: &mut Command,
    label: &str,
    total: Option<u64>,
    mode: TarMode,
) -> Result<Vec<u8>> {
    tar_progress_inner(command, label, total, mode, None)
}

pub(crate) fn tar_progress_estimated<'a>(
    command: &mut Command,
    label: &str,
    estimate: impl FnOnce() -> Result<(u64, u64, u64)> + Send + 'a,
) -> Result<Vec<u8>> {
    tar_progress_inner(
        command,
        label,
        None,
        TarMode::Create,
        Some(Box::new(estimate)),
    )
}

type Estimator<'a> = Box<dyn FnOnce() -> Result<(u64, u64, u64)> + Send + 'a>;

fn tar_progress_inner(
    command: &mut Command,
    label: &str,
    total: Option<u64>,
    mode: TarMode,
    estimate: Option<Estimator<'_>>,
) -> Result<Vec<u8>> {
    check_cancel()?;
    let progress = if estimate.is_some() {
        Progress::estimating(label)
    } else {
        Progress::new(label, total, Unit::Items)
    };
    let listing = matches!(mode, TarMode::List);
    command.stderr(Stdio::piped()).stdout(if listing {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    let mut child = Guard(command.spawn()?);
    // Start estimation only after tar is running; use cached scan metadata, not another disk walk.
    thread::scope(|scope| {
        let estimating = estimate.map(|estimate| {
            let handle = progress.handle();
            scope.spawn(move || match estimate() {
                Ok((items, uncompressed, compressed)) => {
                    handle.estimated(Some(items), Some(uncompressed), Some(compressed))
                }
                Err(e) => {
                    handle.estimated(None, None, None);
                    if !crate::interrupted() {
                        progress::log(&format!("WARNING: Background estimate unavailable: {e}"));
                    }
                }
            })
        });
        let stderr = child.0.stderr.take().unwrap();
        let prefix = match mode {
            TarMode::Create => Some("a "),
            TarMode::Extract => Some("x "),
            TarMode::List => None,
        };
        let handle = progress.handle();
        let logs = thread::spawn(move || read_stderr(stderr, prefix, Some(handle)));
        let output = child.0.stdout.take().map(|stdout| {
            let handle = progress.handle();
            thread::spawn(move || -> Result<Vec<u8>> {
                let mut reader = BufReader::new(stdout);
                let mut all = Vec::new();
                let mut line = Vec::new();
                loop {
                    line.clear();
                    if reader.read_until(b'\n', &mut line)? == 0 {
                        break;
                    }
                    if all.len() + line.len() > 256 * 1024 * 1024 {
                        return Err(error("Archive listing exceeds the 256 MiB safety limit"));
                    }
                    all.extend_from_slice(&line);
                    handle.advance();
                }
                Ok(all)
            })
        });
        let result = wait(&mut child.0);
        drop(child);
        let log_result = logs
            .join()
            .map_err(|_| error("Archive log reader panicked"))?;
        let data = match output {
            Some(output) => output
                .join()
                .map_err(|_| error("Archive list reader panicked"))?,
            None => Ok(Vec::new()),
        };
        if let Some(estimating) = estimating {
            estimating
                .join()
                .map_err(|_| error("Background estimator panicked"))?;
        }
        let success = result.is_ok() && log_result.is_ok() && data.is_ok();
        progress.finish(success);
        result?;
        log_result?;
        data
    })
}

fn read_stderr(
    reader: impl Read,
    prefix: Option<&str>,
    handle: Option<progress::Handle>,
) -> Result<()> {
    let mut reader = BufReader::new(reader);
    let mut bytes = Vec::new();
    loop {
        bytes.clear();
        if reader.read_until(b'\n', &mut bytes)? == 0 {
            break;
        }
        let line = String::from_utf8_lossy(&bytes);
        let line = line.trim_end_matches(['\n', '\r']);
        if prefix.is_some_and(|p| line.starts_with(p)) {
            if let Some(handle) = &handle {
                handle.advance();
            }
            if line.split_once(": ").is_some_and(|(_, detail)| {
                [
                    "Can't",
                    "Cannot",
                    "Permission denied",
                    "Error",
                    "Failed",
                    "Could not",
                    "Couldn't",
                ]
                .iter()
                .any(|p| detail.starts_with(p))
            }) {
                progress::log(line);
            }
        } else if !line.is_empty() {
            progress::log(line);
        }
    }
    Ok(())
}

pub(crate) fn output(command: &mut Command) -> Result<Vec<u8>> {
    let file = tempfile::NamedTempFile::new()?;
    command.stdout(Stdio::from(file.reopen()?));
    run(command)?;
    let mut bytes = Vec::new();
    File::open(file.path())?
        .take(256 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 256 * 1024 * 1024 {
        return Err(error("Command output exceeds the 256 MiB safety limit"));
    }
    Ok(bytes)
}

pub(crate) fn available(name: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|p| p.join(name).is_file()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn estimator_runs_after_child_start_and_concurrently_with_it() {
        let t = tempfile::tempdir().unwrap();
        let started = t.path().join("started");
        let ready = t.path().join("ready");
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "printf started > \"$1\"; printf 'a first\\n' >&2; i=0; while [ ! -f \"$2\" ] && [ \"$i\" -lt 100 ]; do sleep 0.01; i=$((i+1)); done; [ -f \"$2\" ] || exit 3; printf 'a second\\n' >&2", "test"])
            .arg(&started).arg(&ready);
        tar_progress_estimated(&mut command, "Test archive", || {
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            while !started.is_file() && std::time::Instant::now() < deadline {
                thread::sleep(Duration::from_millis(1));
            }
            let observed = started.is_file();
            std::fs::write(&ready, b"ready")?;
            if !observed {
                return Err(error("Estimator ran without a started child"));
            }
            Ok((2, 2048, 1024))
        })
        .unwrap();
        assert!(started.is_file() && ready.is_file());
    }
}
