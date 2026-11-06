use crate::{
    Result, check_cancel, process,
    scan::{FileInfo, Scan},
};
use std::{collections::HashSet, path::Path, process::Command, time::Instant};

const MIB: u64 = 1024 * 1024;
const COMPRESSION_RATIO: f64 = 0.70;
const HEADER_ALLOWANCE: u64 = 1536;

#[derive(Default)]
struct Size {
    bytes: u64,
    files: u64,
    entries: u64,
}

impl Size {
    fn uncompressed(&self) -> u64 {
        self.bytes
            .saturating_add(self.entries.saturating_mul(HEADER_ALLOWANCE))
            .saturating_add(MIB)
    }
}

fn unique_files(scan: &Scan) -> impl Iterator<Item = &FileInfo> {
    let mut links = HashSet::new();
    scan.accepted_files()
        .filter(move |file| file.hardlink.is_none_or(|key| links.insert(key)))
}

fn size(scan: &Scan) -> Result<Size> {
    let mut size = Size {
        entries: scan.entries(),
        ..Size::default()
    };
    for file in unique_files(scan) {
        check_cancel()?;
        size.files += 1;
        size.bytes = size.bytes.saturating_add(file.size);
    }
    Ok(size)
}

pub(crate) fn background_totals(
    scan: &Scan,
    metadata_items: u64,
    metadata_bytes: u64,
) -> Result<(u64, u64, u64)> {
    let mut size = size(scan)?;
    size.entries = size.entries.saturating_add(metadata_items);
    size.bytes = size.bytes.saturating_add(metadata_bytes);
    let uncompressed = size.uncompressed();
    Ok((
        size.entries,
        uncompressed,
        (uncompressed as f64 * COMPRESSION_RATIO) as u64,
    ))
}

pub(crate) fn print(scan: &Scan) -> Result<()> {
    let size = size(scan)?;
    let gib = 1024_f64.powi(3);
    let uncompressed = size.uncompressed() as f64;
    println!(
        "ESTIMATED SIZE\n  Data: {:.2} GiB ({:.2} GB; {} bytes)",
        size.bytes as f64 / gib,
        size.bytes as f64 / 1e9,
        size.bytes
    );
    println!(
        "  Uncompressed archive: ~{:.2} GiB\n  Compressed archive:   ~{:.2} GiB (assumed 30% savings)",
        uncompressed / gib,
        uncompressed * COMPRESSION_RATIO / gib
    );
    println!(
        "  Files: {} | Plan space for at least {:.2} GiB; compression is not guaranteed.",
        size.files,
        uncompressed / gib
    );
    if !scan.problems.is_empty() {
        eprintln!(
            "WARNING: {} scan errors; size may be incomplete.",
            scan.problems.len()
        );
    }
    Ok(())
}

struct Sample<'a> {
    file: &'a FileInfo,
    score: f64,
}

fn samples(scan: &Scan) -> Result<Vec<Sample<'_>>> {
    let mut samples: Vec<Sample<'_>> = Vec::new();
    let mut random = 42u64;
    for file in unique_files(scan) {
        check_cancel()?;
        if file.size == 0 || file.size > 8 * MIB {
            continue;
        }
        // Deterministic byte-weighted reservoir, using only cached scan metadata.
        random ^= random << 13;
        random ^= random >> 7;
        random ^= random << 17;
        let u = (random as f64 / u64::MAX as f64).max(f64::MIN_POSITIVE);
        let score = -u.ln() / file.size as f64;
        if samples.len() < 64 || score < samples.last().unwrap().score {
            samples.push(Sample { file, score });
            samples.sort_by(|a, b| a.score.total_cmp(&b.score));
            samples.truncate(64);
        }
    }
    Ok(samples)
}

pub(crate) fn benchmark(home: &Path, destination: &Path, scan: &Scan) -> Result<()> {
    let mut selected = Vec::new();
    let mut total = 0u64;
    for sample in samples(scan)? {
        if total + sample.file.size <= 64 * MIB {
            total += sample.file.size;
            selected.push(sample.file);
        }
    }
    if total < MIB {
        println!("ESTIMATED TIME: unavailable (less than 1 MiB of eligible whole files).");
        return Ok(());
    }
    let file = tempfile::Builder::new()
        .prefix(".mac-cpy-speed-")
        .suffix(".tar.gz.tmp")
        .tempfile_in(destination)?;
    let mut command = Command::new("/usr/bin/tar");
    command
        .args([
            "--create",
            "--gzip",
            "--format=pax",
            "--mac-metadata",
            "--xattrs",
            "--no-recursion",
            "--verbose",
            "--file",
        ])
        .arg(file.path())
        .arg("-C")
        .arg(home);
    let sample_count = selected.len() as u64;
    for sample in selected {
        command.arg(Path::new(".").join(&sample.path));
    }
    let start = Instant::now();
    process::tar_progress(
        &mut command,
        "Benchmark",
        Some(sample_count),
        process::TarMode::Create,
    )?;
    crate::storage::sync(file.as_file(), file.path())?;
    let elapsed = start.elapsed().as_secs_f64().max(0.001);
    let speed = total as f64 / elapsed;
    let seconds = (size(scan)?.uncompressed() as f64 / speed).ceil() as u64;
    println!(
        "ESTIMATED TIME\n  tar + gzip + write: {:.1} MiB/s (input; {:.1} MiB sample)",
        speed / MIB as f64,
        total as f64 / MIB as f64
    );
    println!(
        "  Sample compressed size: {:.0}% of input",
        file.as_file().metadata()?.len() as f64 / total as f64 * 100.
    );
    println!(
        "  Archive creation: ~{}\n  Approximate; files larger than 8 MiB are not sampled. Final verification takes additional time.",
        duration(seconds)
    );
    Ok(())
}

fn duration(seconds: u64) -> String {
    if seconds >= 3600 {
        format!("{}h {:02}m", seconds / 3600, seconds % 3600 / 60)
    } else {
        format!("{}m {:02}s", seconds / 60, seconds % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::ScanProblem;
    use std::path::PathBuf;

    #[test]
    fn estimates_count_hardlinks_once_and_exclude_skipped_paths() {
        let mut scan = Scan {
            included: ["data", "data/a", "data/b", "data/c"]
                .map(PathBuf::from)
                .into(),
            files: vec![
                FileInfo {
                    path: "data/a".into(),
                    size: 1000,
                    hardlink: Some((1, 2)),
                },
                FileInfo {
                    path: "data/b".into(),
                    size: 1000,
                    hardlink: Some((1, 2)),
                },
                FileInfo {
                    path: "data/c".into(),
                    size: 2000,
                    hardlink: None,
                },
            ],
            ..Scan::default()
        };
        let totals = size(&scan).unwrap();
        assert_eq!((totals.bytes, totals.files, totals.entries), (3000, 2, 4));
        let (items, uncompressed, compressed) = background_totals(&scan, 1, 100).unwrap();
        assert_eq!(items, 5);
        assert_eq!(uncompressed, 3100 + 5 * HEADER_ALLOWANCE + MIB);
        assert_eq!(compressed, (uncompressed as f64 * COMPRESSION_RATIO) as u64);
        scan.problems.insert(
            "/source/data/c".into(),
            ScanProblem {
                relative: Some("data/c".into()),
                reason: "unreadable".into(),
            },
        );
        let totals = size(&scan).unwrap();
        assert_eq!((totals.bytes, totals.files, totals.entries), (1000, 1, 3));
        assert_eq!(samples(&scan).unwrap().len(), 1);
    }
}
