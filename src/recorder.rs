//! Runs a recording: a timer thread takes screenshots, and a few low-priority worker threads
//! save them, so the timer never waits on the disk.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::capture::{self, CaptureError, Capturer, Frame, Method, WindowId};
use crate::encode::{self, Format};
use crate::naming;
use crate::pack;
use serde::{Deserialize, Serialize};

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
    /// Include the mouse pointer in screenshots.
    pub pointer: bool,
    pub output: Output,
    /// For ZIP output, the largest each ZIP part may be.
    pub zip_split: pack::ZipSplit,
    /// Stop after this many screenshots. `None` means record until stopped.
    pub limit: Option<u64>,
}

/// How each recording is kept, so separate recordings never mix.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Output {
    /// A folder named after the start time and window, such as `2026-10-14 19-30-05 - Foundry VTT`.
    #[default]
    Folder,
    /// The same folder, packed into one ZIP file when the recording ends.
    Zip,
}

impl Output {
    pub const ALL: [Output; 2] = [Output::Folder, Output::Zip];

    pub fn label(self) -> &'static str {
        match self {
            Output::Folder => "A folder per recording",
            Output::Zip => "A ZIP file per recording",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum State {
    Starting,
    Running,
    /// Waiting for the user to restore the minimized window.
    Minimized,
    /// Packing the finished recording into a ZIP file.
    Packing,
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
    /// Name of this recording's folder, or of its ZIP file once packed.
    destination: Mutex<Option<String>>,
    last_file: Mutex<Option<String>>,
    last_error: Mutex<Option<String>>,
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub saved: u64,
    pub skipped: u64,
    pub failed: u64,
    pub state: State,
    pub destination: Option<String>,
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
            destination: Mutex::new(None),
            last_file: Mutex::new(None),
            last_error: Mutex::new(None),
        }
    }

    fn set_state(&self, state: State) {
        *self.state.lock().unwrap() = state;
    }

    fn set_destination(&self, path: &Path) {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
        *self.destination.lock().unwrap() = name;
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
            destination: self.destination.lock().unwrap().clone(),
            last_file: self.last_file.lock().unwrap().clone(),
            last_error: self.last_error.lock().unwrap().clone(),
        }
    }
}

/// The current recording, shared between the window and the tray icon.
pub type SharedRecorder = Arc<Mutex<Option<Recorder>>>;

type Opener = Box<dyn FnOnce() -> Result<Box<dyn Capturer>, CaptureError> + Send>;

pub struct Recorder {
    stop: Arc<AtomicBool>,
    stats: Arc<Stats>,
    thread: Option<JoinHandle<()>>,
}

impl Recorder {
    pub fn start(config: Config) -> Recorder {
        let (window, method, pointer) = (config.window, config.method, config.pointer);
        Self::start_with(
            config,
            Box::new(move || capture::open(window, method, pointer)),
        )
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
    let started = chrono::Local::now().format("%Y-%m-%d %H-%M-%S").to_string();
    let name = naming::recording_name(&started, &config.title);
    let dir = naming::unique_recording_dir(&config.folder, &name);
    if let Err(e) = std::fs::create_dir(&dir) {
        stats.set_state(State::Ended(format!("Couldn't create the folder: {e}")));
        return;
    }
    stats.set_destination(&dir);

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

    let mut number = 1;
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
                let path = dir.join(naming::file_name(number, &title, config.format));
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
    if let Some(problem) = finish_recording(&dir, &config, stats) {
        if final_state == State::Stopped {
            final_state = State::Ended(problem);
        }
    }
    stats.set_state(final_state);
}

/// Tidies up after the last screenshot is saved: removes the folder if nothing was saved, or packs
/// it into a ZIP file. Returns a message if packing failed. The pictures stay in the folder then.
fn finish_recording(dir: &Path, config: &Config, stats: &Stats) -> Option<String> {
    if stats.saved.load(Ordering::Relaxed) == 0 {
        // Only removes the folder if it's empty.
        let _ = std::fs::remove_dir(dir);
        *stats.destination.lock().unwrap() = None;
        return None;
    }
    if config.output != Output::Zip {
        return None;
    }
    stats.set_state(State::Packing);
    match pack::zip_folder(dir, config.zip_split.max_bytes()) {
        Ok(parts) => {
            if let [only] = parts.as_slice() {
                stats.set_destination(only);
            } else {
                let name = dir.file_name().unwrap_or_default().to_string_lossy();
                *stats.destination.lock().unwrap() =
                    Some(format!("{name} ({} ZIP parts)", parts.len()));
            }
            None
        }
        Err(e) => Some(format!(
            "Couldn't create the ZIP file ({e}). The pictures are in the folder instead."
        )),
    }
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

    fn fake(close_after: Option<u32>) -> Opener {
        Box::new(move || {
            Ok(Box::new(FakeWindow {
                grabs: 0,
                close_after,
            }) as Box<dyn Capturer>)
        })
    }

    fn config(folder: PathBuf, limit: Option<u64>) -> Config {
        Config {
            window: WindowId(1),
            title: "VTT".into(),
            folder,
            interval: Duration::from_millis(20),
            format: Format::Png,
            method: Method::Auto,
            pointer: false,
            output: Output::Folder,
            zip_split: pack::ZipSplit::Off,
            limit,
        }
    }

    fn record_until_done(config: Config, open: Opener) -> Snapshot {
        let recorder = Recorder::start_with(config, open);
        let deadline = Instant::now() + Duration::from_secs(10);
        while !recorder.is_done() {
            assert!(Instant::now() < deadline, "recording didn't finish");
            thread::sleep(Duration::from_millis(10));
        }
        recorder.stats().snapshot()
    }

    fn sorted_names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    fn shot_names(count: u64) -> Vec<String> {
        (1..=count)
            .map(|n| format!("{n:06} - Fake Window.png"))
            .collect()
    }

    /// Checks a recording folder's name looks like `2026-10-14 19-30-05 - VTT`.
    fn assert_recording_name(name: &str) {
        let (time, title) = name.split_once(" - ").expect("name has a time and a title");
        assert!(title.starts_with("VTT"), "{name}");
        assert_eq!(time.len(), "2026-10-14 19-30-05".len(), "{name}");
        assert!(
            time.chars()
                .all(|c| c.is_ascii_digit() || c == '-' || c == ' '),
            "{name}"
        );
    }

    #[test]
    fn saves_numbered_files_in_a_folder_for_the_recording() {
        let base = crate::test_dir("recorder-limit");
        let snapshot = record_until_done(config(base.clone(), Some(5)), fake(None));
        assert_eq!(snapshot.state, State::Stopped);
        assert_eq!(snapshot.saved, 5);
        let folders = sorted_names(&base);
        assert_eq!(folders.len(), 1);
        assert_recording_name(&folders[0]);
        assert_eq!(snapshot.destination.as_deref(), Some(folders[0].as_str()));
        assert_eq!(sorted_names(&base.join(&folders[0])), shot_names(5));
    }

    #[test]
    fn each_recording_gets_its_own_folder_numbered_from_one() {
        let base = crate::test_dir("recorder-separate");
        record_until_done(config(base.clone(), Some(2)), fake(None));
        record_until_done(config(base.clone(), Some(3)), fake(None));
        let folders = sorted_names(&base);
        assert_eq!(folders.len(), 2, "{folders:?}");
        let mut counts: Vec<_> = folders
            .iter()
            .map(|f| sorted_names(&base.join(f)))
            .collect();
        counts.sort_by_key(Vec::len);
        assert_eq!(counts, vec![shot_names(2), shot_names(3)]);
    }

    #[test]
    fn zip_mode_leaves_one_zip_with_every_screenshot() {
        let base = crate::test_dir("recorder-zip");
        let mut cfg = config(base.clone(), Some(4));
        cfg.output = Output::Zip;
        let snapshot = record_until_done(cfg, fake(None));
        assert_eq!(snapshot.state, State::Stopped);
        let entries = sorted_names(&base);
        assert_eq!(entries.len(), 1, "{entries:?}");
        let zip_name = &entries[0];
        assert!(zip_name.ends_with(".zip"));
        assert_recording_name(zip_name.trim_end_matches(".zip"));
        assert_eq!(snapshot.destination.as_deref(), Some(zip_name.as_str()));
        let archive =
            zip::ZipArchive::new(std::fs::File::open(base.join(zip_name)).unwrap()).unwrap();
        let mut names: Vec<_> = archive.file_names().map(String::from).collect();
        names.sort();
        assert_eq!(names, shot_names(4));
    }

    #[test]
    fn a_recording_with_no_screenshots_leaves_nothing_behind() {
        let base = crate::test_dir("recorder-empty");
        let snapshot = record_until_done(config(base.clone(), None), fake(Some(0)));
        assert_eq!(snapshot.state, State::Ended("The window was closed".into()));
        assert!(sorted_names(&base).is_empty());
        assert_eq!(snapshot.destination, None);
    }

    #[test]
    fn stops_when_the_window_closes() {
        let base = crate::test_dir("recorder-gone");
        let snapshot = record_until_done(config(base, None), fake(Some(3)));
        assert_eq!(snapshot.state, State::Ended("The window was closed".into()));
        assert_eq!(snapshot.saved, 3);
    }

    #[test]
    fn stop_button_ends_the_recording_quickly() {
        let base = crate::test_dir("recorder-stop");
        let mut cfg = config(base, None);
        cfg.interval = Duration::from_secs(3600);
        let mut recorder = Recorder::start_with(cfg, fake(None));
        thread::sleep(Duration::from_millis(100));
        let started = Instant::now();
        recorder.stop();
        assert!(started.elapsed() < Duration::from_millis(500));
        assert_eq!(recorder.stats().snapshot().saved, 1);
    }

    #[test]
    fn reports_a_window_that_cannot_be_opened() {
        let base = crate::test_dir("recorder-open-fail");
        let fail: Opener = Box::new(|| Err(CaptureError::Failed("no such window".into())));
        let snapshot = record_until_done(config(base.clone(), None), fail);
        assert_eq!(snapshot.state, State::Ended("no such window".into()));
        assert!(sorted_names(&base).is_empty());
    }
}
