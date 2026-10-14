//! Runs a recording: a timer thread takes screenshots, and a few low-priority worker threads
//! save them, so the timer never waits on the disk.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::capture::{self, CaptureError, Capturer, Frame, Method, WindowId};
use crate::encode::{self, Format};
use crate::naming;

/// Gives up after this many failed captures in a row, so a broken setup doesn't spin forever.
const MAX_FAILURES_IN_A_ROW: u32 = 20;

pub struct Config {
    pub window: WindowId,
    /// Used in file names when the window's current title can't be read.
    pub title: String,
    pub folder: PathBuf,
    pub interval: Duration,
    pub format: Format,
    pub method: Method,
    /// Stop after this many screenshots. `None` means record until stopped.
    pub limit: Option<u64>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum State {
    Starting,
    Running,
    /// Waiting for the user to restore the minimized window.
    Minimized,
    /// Stopped by the user or after reaching the limit.
    Stopped,
    /// Stopped by a problem, with a message for the user.
    Ended(String),
}

/// Progress that the app reads while a recording runs.
pub struct Stats {
    pub saved: AtomicU64,
    /// Screenshots dropped because saving couldn't keep up.
    pub skipped: AtomicU64,
    pub failed: AtomicU64,
    state: Mutex<State>,
    last_file: Mutex<Option<String>>,
    last_error: Mutex<Option<String>>,
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub saved: u64,
    pub skipped: u64,
    pub failed: u64,
    pub state: State,
    pub last_file: Option<String>,
    pub last_error: Option<String>,
}

impl Stats {
    fn new() -> Self {
        Stats {
            saved: AtomicU64::new(0),
            skipped: AtomicU64::new(0),
            failed: AtomicU64::new(0),
            state: Mutex::new(State::Starting),
            last_file: Mutex::new(None),
            last_error: Mutex::new(None),
        }
    }

    fn set_state(&self, state: State) {
        *self.state.lock().unwrap() = state;
    }

    fn set_error(&self, message: String) {
        *self.last_error.lock().unwrap() = Some(message);
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            saved: self.saved.load(Ordering::Relaxed),
            skipped: self.skipped.load(Ordering::Relaxed),
            failed: self.failed.load(Ordering::Relaxed),
            state: self.state.lock().unwrap().clone(),
            last_file: self.last_file.lock().unwrap().clone(),
            last_error: self.last_error.lock().unwrap().clone(),
        }
    }
}

type Opener = Box<dyn FnOnce() -> Result<Box<dyn Capturer>, CaptureError> + Send>;

pub struct Recorder {
    stop: Arc<AtomicBool>,
    stats: Arc<Stats>,
    thread: Option<JoinHandle<()>>,
}

impl Recorder {
    pub fn start(config: Config) -> Recorder {
        let (window, method) = (config.window, config.method);
        Self::start_with(config, Box::new(move || capture::open(window, method)))
    }

    /// Starts with a custom way to open the capturer, so tests can use a fake window.
    fn start_with(config: Config, open: Opener) -> Recorder {
        let stop = Arc::new(AtomicBool::new(false));
        let stats = Arc::new(Stats::new());
        let thread = {
            let (stop, stats) = (stop.clone(), stats.clone());
            thread::Builder::new()
                .name("snaplapse-timer".into())
                .spawn(move || record(config, open, &stop, &stats))
                .expect("couldn't start the recording thread")
        };
        Recorder {
            stop,
            stats,
            thread: Some(thread),
        }
    }

    pub fn stats(&self) -> &Stats {
        &self.stats
    }

    /// Asks the recording to stop without waiting. Screenshots already taken still get saved.
    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    pub fn stop_requested(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    /// True once the recording has stopped and every screenshot is saved.
    pub fn is_done(&self) -> bool {
        self.thread.as_ref().is_none_or(|t| t.is_finished())
    }

    /// Stops and waits until every screenshot is saved.
    pub fn stop(&mut self) {
        self.request_stop();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        self.stop();
    }
}

struct Job {
    frame: Frame,
    path: PathBuf,
    format: Format,
}

fn record(config: Config, open: Opener, stop: &AtomicBool, stats: &Arc<Stats>) {
    if let Err(e) = std::fs::create_dir_all(&config.folder) {
        stats.set_state(State::Ended(format!("Couldn't create the folder: {e}")));
        return;
    }
    let mut capturer = match open() {
        Ok(c) => c,
        Err(e) => {
            stats.set_state(State::Ended(e.to_string()));
            return;
        }
    };

    let workers = thread::available_parallelism().map_or(1, |n| (n.get() / 2).clamp(1, 4));
    let (sender, receiver) = sync_channel::<Job>(workers + 1);
    let receiver = Arc::new(Mutex::new(receiver));
    let worker_threads: Vec<_> = (0..workers)
        .map(|i| {
            let (receiver, stats) = (receiver.clone(), stats.clone());
            thread::Builder::new()
                .name(format!("snaplapse-save-{i}"))
                .spawn(move || save_jobs(&receiver, &stats))
                .expect("couldn't start a saving thread")
        })
        .collect();

    let mut number = naming::next_number(&config.folder);
    let mut title = config.title.clone();
    let mut taken = 0u64;
    let mut failures_in_a_row = 0u32;
    let mut next = Instant::now();
    let mut final_state = State::Stopped;

    while sleep_until(next, stop) {
        match capturer.grab() {
            Ok(frame) => {
                failures_in_a_row = 0;
                if let Some(current) = capturer.title() {
                    title = current;
                }
                let path = config
                    .folder
                    .join(naming::file_name(number, &title, config.format));
                match sender.try_send(Job {
                    frame,
                    path,
                    format: config.format,
                }) {
                    Ok(()) => {
                        number += 1;
                        taken += 1;
                        stats.set_state(State::Running);
                    }
                    Err(TrySendError::Full(_)) => {
                        stats.skipped.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(TrySendError::Disconnected(_)) => {
                        final_state = State::Ended("Saving stopped unexpectedly".into());
                        break;
                    }
                }
            }
            Err(CaptureError::Minimized) => stats.set_state(State::Minimized),
            Err(CaptureError::Gone) => {
                final_state = State::Ended(CaptureError::Gone.to_string());
                break;
            }
            Err(CaptureError::Failed(message)) => {
                stats.failed.fetch_add(1, Ordering::Relaxed);
                failures_in_a_row += 1;
                if failures_in_a_row >= MAX_FAILURES_IN_A_ROW {
                    final_state = State::Ended(message);
                    break;
                }
                stats.set_error(message);
            }
        }
        if config.limit.is_some_and(|limit| taken >= limit) {
            break;
        }
        next += config.interval;
        // If a capture ran long, take the next one right away instead of rushing to catch up.
        next = next.max(Instant::now());
    }

    drop(capturer);
    drop(sender);
    for worker in worker_threads {
        let _ = worker.join();
    }
    stats.set_state(final_state);
}

/// Waits until `deadline` in short naps so Stop takes effect quickly. Returns false if stopped.
fn sleep_until(deadline: Instant, stop: &AtomicBool) -> bool {
    loop {
        if stop.load(Ordering::Relaxed) {
            return false;
        }
        let now = Instant::now();
        if now >= deadline {
            return true;
        }
        thread::sleep((deadline - now).min(Duration::from_millis(50)));
    }
}

fn save_jobs(receiver: &Mutex<Receiver<Job>>, stats: &Stats) {
    lower_thread_priority();
    loop {
        let job = receiver.lock().unwrap().recv();
        let Ok(job) = job else { return };
        match encode::save(&job.frame, &job.path, job.format) {
            Ok(()) => {
                stats.saved.fetch_add(1, Ordering::Relaxed);
                let name = job
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned());
                *stats.last_file.lock().unwrap() = name;
            }
            Err(e) => {
                stats.failed.fetch_add(1, Ordering::Relaxed);
                stats.set_error(format!("Couldn't save {}: {e}", job.path.display()));
            }
        }
    }
}

/// Lets everything else on the computer run first, so saving never slows down the user.
fn lower_thread_priority() {
    #[cfg(windows)]
    unsafe {
        use windows::Win32::System::Threading::{
            GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL,
        };
        let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL);
    }
    #[cfg(target_os = "linux")]
    unsafe {
        // On Linux, a thread's "nice" level can be set with its thread ID.
        let tid = libc::syscall(libc::SYS_gettid) as libc::id_t;
        libc::setpriority(libc::PRIO_PROCESS, tid, 10);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pretend window that returns a small image, and closes after `close_after` captures.
    struct FakeWindow {
        grabs: u32,
        close_after: Option<u32>,
    }

    impl Capturer for FakeWindow {
        fn grab(&mut self) -> Result<Frame, CaptureError> {
            self.grabs += 1;
            if self.close_after.is_some_and(|n| self.grabs > n) {
                return Err(CaptureError::Gone);
            }
            Ok(Frame {
                width: 4,
                height: 3,
                bgra: vec![128; 4 * 3 * 4],
            })
        }

        fn title(&self) -> Option<String> {
            Some("Fake: Window".into())
        }
    }

    fn config(folder: PathBuf, limit: Option<u64>) -> Config {
        Config {
            window: WindowId(1),
            title: "Fallback".into(),
            folder,
            interval: Duration::from_millis(20),
            format: Format::Png,
            method: Method::Auto,
            limit,
        }
    }

    fn wait_until_done(recorder: &Recorder) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !recorder.is_done() {
            assert!(Instant::now() < deadline, "recording didn't finish");
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn sorted_names(dir: &std::path::Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn saves_numbered_files_until_the_limit() {
        let dir = crate::test_dir("recorder-limit");
        let fake = || {
            Ok(Box::new(FakeWindow {
                grabs: 0,
                close_after: None,
            }) as Box<dyn Capturer>)
        };
        let recorder = Recorder::start_with(config(dir.clone(), Some(5)), Box::new(fake));
        wait_until_done(&recorder);
        let snapshot = recorder.stats().snapshot();
        assert_eq!(snapshot.state, State::Stopped);
        assert_eq!(snapshot.saved, 5);
        assert_eq!(
            sorted_names(&dir),
            (1..=5)
                .map(|n| format!("{n:06} - Fake Window.png"))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn continues_numbering_in_a_used_folder() {
        let dir = crate::test_dir("recorder-continue");
        std::fs::write(dir.join("000041 - old.png"), b"").unwrap();
        let fake = || {
            Ok(Box::new(FakeWindow {
                grabs: 0,
                close_after: None,
            }) as Box<dyn Capturer>)
        };
        let recorder = Recorder::start_with(config(dir.clone(), Some(2)), Box::new(fake));
        wait_until_done(&recorder);
        assert_eq!(
            sorted_names(&dir),
            vec![
                "000041 - old.png",
                "000042 - Fake Window.png",
                "000043 - Fake Window.png"
            ]
        );
    }

    #[test]
    fn stops_when_the_window_closes() {
        let dir = crate::test_dir("recorder-gone");
        let fake = || {
            Ok(Box::new(FakeWindow {
                grabs: 0,
                close_after: Some(3),
            }) as Box<dyn Capturer>)
        };
        let recorder = Recorder::start_with(config(dir.clone(), None), Box::new(fake));
        wait_until_done(&recorder);
        let snapshot = recorder.stats().snapshot();
        assert_eq!(snapshot.state, State::Ended("The window was closed".into()));
        assert_eq!(snapshot.saved, 3);
    }

    #[test]
    fn stop_button_ends_the_recording_quickly() {
        let dir = crate::test_dir("recorder-stop");
        let fake = || {
            Ok(Box::new(FakeWindow {
                grabs: 0,
                close_after: None,
            }) as Box<dyn Capturer>)
        };
        let mut cfg = config(dir, None);
        cfg.interval = Duration::from_secs(3600);
        let mut recorder = Recorder::start_with(cfg, Box::new(fake));
        thread::sleep(Duration::from_millis(100));
        let started = Instant::now();
        recorder.stop();
        assert!(started.elapsed() < Duration::from_millis(500));
        assert_eq!(recorder.stats().snapshot().saved, 1);
    }

    #[test]
    fn reports_a_window_that_cannot_be_opened() {
        let dir = crate::test_dir("recorder-open-fail");
        let fail = || Err(CaptureError::Failed("no such window".into()));
        let recorder = Recorder::start_with(config(dir, None), Box::new(fail));
        wait_until_done(&recorder);
        assert_eq!(
            recorder.stats().snapshot().state,
            State::Ended("no such window".into())
        );
    }
}
