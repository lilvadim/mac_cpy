use std::{
    io::{self, IsTerminal, Write},
    sync::{Arc, Mutex, OnceLock, Weak, mpsc},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

static RUN_START: OnceLock<Instant> = OnceLock::new();
static ACTIVE: Mutex<Option<Weak<Mutex<State>>>> = Mutex::new(None);

#[derive(Clone, Copy)]
pub(crate) enum Unit {
    Items,
    Bytes,
    Unknown,
}

pub(crate) fn start_run() {
    RUN_START.get_or_init(Instant::now);
}

struct State {
    label: String,
    done: u64,
    total: Option<u64>,
    unit: Unit,
    start: Instant,
    terminal: bool,
    drawn: usize,
    finished: Option<bool>,
    estimating: bool,
    estimated_archive: Option<u64>,
    total_uncompressed: Option<u64>,
}

#[derive(Clone)]
pub(crate) struct Handle(Arc<Mutex<State>>);
impl Handle {
    pub fn estimated(
        &self,
        total: Option<u64>,
        uncompressed: Option<u64>,
        compressed: Option<u64>,
    ) {
        let mut state = self.0.lock().unwrap();
        state.total = total;
        state.estimated_archive = compressed;
        state.total_uncompressed = uncompressed;
        state.estimating = false;
    }
    pub fn set(&self, done: u64) {
        self.0.lock().unwrap().done = done;
    }
    pub fn advance(&self) {
        self.0.lock().unwrap().done += 1;
    }
}

pub(crate) struct Progress {
    handle: Handle,
    stop: Option<mpsc::Sender<()>>,
    thread: Option<JoinHandle<()>>,
    closed: bool,
}

impl Progress {
    pub fn new(label: &str, total: Option<u64>, unit: Unit) -> Self {
        Self::create(label, total, unit, false)
    }
    pub fn estimating(label: &str) -> Self {
        Self::create(label, None, Unit::Items, true)
    }
    fn create(label: &str, total: Option<u64>, unit: Unit, estimating: bool) -> Self {
        RUN_START.get_or_init(Instant::now);
        let terminal = io::stderr().is_terminal() && std::env::var("TERM").as_deref() != Ok("dumb");
        let state = Arc::new(Mutex::new(State {
            label: label.into(),
            done: 0,
            total,
            unit,
            start: Instant::now(),
            terminal,
            drawn: 0,
            finished: None,
            estimating,
            estimated_archive: None,
            total_uncompressed: None,
        }));
        *ACTIVE.lock().unwrap() = Some(Arc::downgrade(&state));
        let mut progress = Self {
            handle: Handle(state.clone()),
            stop: None,
            thread: None,
            closed: false,
        };
        if terminal {
            state
                .lock()
                .unwrap()
                .render(&mut io::stderr().lock(), width());
            let (sender, receiver) = mpsc::channel();
            progress.stop = Some(sender);
            progress.thread = Some(thread::spawn(move || {
                while matches!(
                    receiver.recv_timeout(Duration::from_millis(200)),
                    Err(mpsc::RecvTimeoutError::Timeout)
                ) {
                    state
                        .lock()
                        .unwrap()
                        .render(&mut io::stderr().lock(), width());
                }
            }));
        }
        progress
    }
    pub fn handle(&self) -> Handle {
        self.handle.clone()
    }
    pub fn set(&self, done: u64) {
        self.handle.set(done);
    }
    pub fn finish(mut self, success: bool) {
        self.close(success);
    }
    fn close(&mut self, success: bool) {
        if self.closed {
            return;
        }
        self.closed = true;
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let mut active = ACTIVE.lock().unwrap();
        if active
            .as_ref()
            .and_then(Weak::upgrade)
            .is_some_and(|s| Arc::ptr_eq(&s, &self.handle.0))
        {
            *active = None;
        }
        drop(active);
        let mut state = self.handle.0.lock().unwrap();
        state.finished = Some(success);
        if success {
            state.total = Some(state.done);
        }
        let mut output = io::stderr().lock();
        if state.terminal {
            state.clear(&mut output);
            let _ = output.flush();
        } else {
            for line in state.lines() {
                let _ = writeln!(output, "{line}");
            }
        }
    }
}
impl Drop for Progress {
    fn drop(&mut self) {
        self.close(false);
    }
}

// Warnings become permanent log lines above the live block, never inside its redraw area.
pub(crate) fn log(message: &str) {
    let state = ACTIVE.lock().unwrap().as_ref().and_then(Weak::upgrade);
    if let Some(state) = state {
        let mut state = state.lock().unwrap();
        let mut output = io::stderr().lock();
        state.clear(&mut output);
        let _ = writeln!(output, "{message}");
        if state.terminal {
            state.render(&mut output, width());
        }
    } else {
        eprintln!("{message}");
    }
}

impl State {
    fn lines(&self) -> [String; 7] {
        let elapsed = RUN_START.get_or_init(Instant::now).elapsed();
        let total = self.total.map(|n| n.max(self.done));
        let count = match self.unit {
            Unit::Unknown => "Files: ? / ?".into(),
            Unit::Items => format!(
                "Files/dirs: {} / {}",
                self.done,
                total.map(|v| v.to_string()).unwrap_or_else(|| "?".into())
            ),
            Unit::Bytes => format!(
                "Read: {:.1} / {} MiB",
                self.done as f64 / 1048576.,
                total
                    .map(|v| format!("{:.1}", v as f64 / 1048576.))
                    .unwrap_or_else(|| "?".into())
            ),
        };
        let percent = if self.finished == Some(true) {
            Some(100.)
        } else {
            total
                .filter(|n| *n > 0)
                .map(|n| (self.done as f64 / n as f64 * 100.).min(99.9))
        };
        let eta = if self.finished == Some(true) {
            "00:00:00".into()
        } else if self.finished == Some(false) {
            "stopped".into()
        } else if let Some(total) = total.filter(|n| *n > self.done && self.done > 0) {
            format!(
                "~{}",
                duration(
                    self.start.elapsed().as_secs_f64() * (total - self.done) as f64
                        / self.done as f64
                )
            )
        } else if total.is_some_and(|n| self.done >= n && self.done > 0) {
            "finishing".into()
        } else {
            "?".into()
        };
        let percentage = if self.estimating {
            "Estimating...".to_owned()
        } else {
            percent
                .map(|p| format!("~{p:.1}%"))
                .unwrap_or_else(|| "? (total unknown)".into())
        };
        let size = |bytes: Option<u64>| {
            bytes
                .map(|b| format!("~{:.2} GiB", b as f64 / 1024_f64.powi(3)))
                .unwrap_or_else(|| {
                    if self.estimating {
                        "Estimating..."
                    } else {
                        "?"
                    }
                    .into()
                })
        };
        [
            format!("Stage: {}", self.label),
            count,
            format!("Progress: {percentage}"),
            format!(
                "Total size (uncompressed): {}",
                size(self.total_uncompressed)
            ),
            format!("Estimated archive: {}", size(self.estimated_archive)),
            format!("Elapsed: {}", duration(elapsed.as_secs_f64())),
            format!("ETA (stage): {eta}"),
        ]
    }
    fn clear(&mut self, output: &mut impl Write) {
        if self.drawn > 0 {
            let _ = write!(output, "\x1b[{}A\r\x1b[J", self.drawn);
            self.drawn = 0;
        }
    }
    fn render(&mut self, output: &mut impl Write, columns: usize) {
        self.clear(output);
        let border = "-".repeat(columns.saturating_sub(1).min(78));
        let _ = writeln!(output, "\r{border}");
        let lines = self.lines();
        let rows = lines.len() + 2;
        for (index, line) in lines.into_iter().enumerate() {
            let line: String = line.chars().take(columns.saturating_sub(1)).collect();
            if index == 0 && self.terminal {
                let _ = writeln!(output, "\r\x1b[1m{line}\x1b[0m");
            } else {
                let _ = writeln!(output, "\r{line}");
            }
        }
        let _ = writeln!(output, "\r{border}");
        let _ = output.flush();
        self.drawn = rows;
    }
}

fn duration(seconds: f64) -> String {
    let seconds = seconds.max(0.).round() as u64;
    format!(
        "{:02}:{:02}:{:02}",
        seconds / 3600,
        seconds % 3600 / 60,
        seconds % 60
    )
}

fn width() -> usize {
    let mut size = std::mem::MaybeUninit::<libc::winsize>::zeroed();
    // TIOCGWINSZ writes a winsize only when the ioctl succeeds.
    if unsafe { libc::ioctl(libc::STDERR_FILENO, libc::TIOCGWINSZ, size.as_mut_ptr()) } == 0 {
        let columns = unsafe { size.assume_init() }.ws_col as usize;
        if columns > 0 {
            return columns;
        }
    }
    80
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn redraw_reuses_nine_rows_and_unknown_totals_are_honest() {
        let mut s = State {
            label: "Scan".into(),
            done: 7,
            total: None,
            unit: Unit::Items,
            start: Instant::now(),
            terminal: true,
            drawn: 0,
            finished: None,
            estimating: false,
            estimated_archive: None,
            total_uncompressed: None,
        };
        let mut bytes = Vec::new();
        s.render(&mut bytes, 80);
        s.done = 8;
        s.render(&mut bytes, 80);
        let text = String::from_utf8(bytes).unwrap();
        assert_eq!(text.matches("\x1b[9A").count(), 1);
        assert!(text.contains("\x1b[1mStage: Scan\x1b[0m"));
        assert!(text.contains(&"-".repeat(78)));
        assert!(s.lines().iter().all(|line| !line.contains(" | ")));
        assert!(text.contains("total unknown"));
        assert!(text.contains("Files/dirs: 8 / ?"));
    }
    #[test]
    fn percentages_and_stage_eta() {
        let s = State {
            label: "Archive".into(),
            done: 25,
            total: Some(100),
            unit: Unit::Items,
            start: Instant::now() - Duration::from_secs(10),
            terminal: false,
            drawn: 0,
            finished: None,
            estimating: false,
            estimated_archive: Some(7 * 1024_u64.pow(3)),
            total_uncompressed: Some(10 * 1024_u64.pow(3)),
        };
        let text = s.lines().join(" ");
        assert!(text.contains("~25.0%"));
        assert!(text.contains("~00:00:30"));
        assert!(text.contains("Total size (uncompressed): ~10.00 GiB"));
        assert!(text.contains("Estimated archive: ~7.00 GiB"));
    }
}
