//! Command-line mode, for scripting and testing. Running with no arguments opens the app instead.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::capture::{self, Method, WindowId};
use crate::encode::{self, Format};
use crate::pack::ZipSplit;
use crate::recorder::{Config, Output, Recorder, State};

const USAGE: &str = "\
SnapLapse: timed screenshots of any window, even when it's covered.

Usage:
  snaplapse                       Open the app
  snaplapse --list                List open windows and their IDs
  snaplapse --shot ID FILE        Save one screenshot of window ID (format from FILE's extension)
  snaplapse --record ID FOLDER [--every-ms N] [--count N] [--format png|jpg|webp] [--method auto|wgc|printwindow] [--zip [--split 100mb|500mb|1gb|2gb|4gb]]
                                  Save a screenshot every N milliseconds (default 1000) until
                                  COUNT screenshots are saved (default 10). Each recording
                                  gets its own folder in FOLDER; --zip packs it into a ZIP file
  snaplapse --help | --version
";

pub fn run(args: &[String]) -> i32 {
    let result = match args[0].as_str() {
        "--help" | "-h" => {
            print!("{USAGE}");
            Ok(())
        }
        "--version" | "-V" => {
            println!("SnapLapse {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        "--list" => list(),
        "--shot" => shot(&args[1..]),
        "--record" => record(&args[1..]),
        other => Err(format!("Unknown option {other:?}\n\n{USAGE}")),
    };
    match result {
        Ok(()) => 0,
        Err(message) => {
            eprintln!("{message}");
            1
        }
    }
}

fn list() -> Result<(), String> {
    for window in capture::list_windows()? {
        println!("{:#x}\t{}", window.id.0, window.title);
    }
    Ok(())
}

fn shot(args: &[String]) -> Result<(), String> {
    let [id, file] = args else {
        return Err(format!("--shot needs a window ID and a file\n\n{USAGE}"));
    };
    let id = parse_id(id)?;
    let path = PathBuf::from(file);
    let format = path
        .extension()
        .and_then(|e| Format::from_extension(&e.to_string_lossy()))
        .ok_or("The file name must end in .png, .jpg or .webp")?;
    let frame = capture::open(id, Method::Auto)
        .and_then(|mut c| c.grab())
        .map_err(|e| e.to_string())?;
    encode::save(&frame, &path, format).map_err(|e| e.to_string())?;
    println!(
        "Saved {} ({}×{})",
        path.display(),
        frame.width,
        frame.height
    );
    Ok(())
}

fn record(args: &[String]) -> Result<(), String> {
    let (id, folder, options) = match args {
        [id, folder, options @ ..] => (parse_id(id)?, PathBuf::from(folder), options),
        _ => {
            return Err(format!(
                "--record needs a window ID and a folder\n\n{USAGE}"
            ))
        }
    };
    let mut every_ms = 1000u64;
    let mut count = 10u64;
    let mut format = Format::Png;
    let mut method = Method::Auto;
    let mut output = Output::Folder;
    let mut zip_split = ZipSplit::Off;
    let mut rest = options.iter();
    while let Some(option) = rest.next() {
        if option == "--zip" {
            output = Output::Zip;
            continue;
        }
        let value = rest
            .next()
            .ok_or_else(|| format!("{option} needs a value"))?;
        match option.as_str() {
            "--every-ms" => {
                every_ms = value
                    .parse()
                    .map_err(|_| "--every-ms must be a whole number")?
            }
            "--count" => {
                count = value
                    .parse()
                    .map_err(|_| "--count must be a whole number")?
            }
            "--format" => {
                format = Format::from_extension(value).ok_or("--format must be png, jpg or webp")?
            }
            "--split" => {
                zip_split = match value.to_ascii_lowercase().as_str() {
                    "100mb" => ZipSplit::Mb100,
                    "500mb" => ZipSplit::Mb500,
                    "1gb" => ZipSplit::Gb1,
                    "2gb" => ZipSplit::Gb2,
                    "4gb" => ZipSplit::Gb4,
                    _ => return Err("--split must be 100mb, 500mb, 1gb, 2gb or 4gb".into()),
                }
            }
            "--method" => {
                method = match value.as_str() {
                    "auto" => Method::Auto,
                    "wgc" => Method::GraphicsCapture,
                    "printwindow" => Method::PrintWindow,
                    _ => return Err("--method must be auto, wgc or printwindow".into()),
                }
            }
            other => return Err(format!("Unknown option {other:?}")),
        }
    }

    let title = capture::list_windows()?
        .into_iter()
        .find(|w| w.id == id)
        .map_or_else(|| "Window".to_string(), |w| w.title);
    let started = Instant::now();
    let mut recorder = Recorder::start(Config {
        window: id,
        title,
        folder: folder.clone(),
        interval: Duration::from_millis(every_ms.max(10)),
        format,
        method,
        output,
        zip_split,
        limit: Some(count),
    });
    while !recorder.is_done() {
        std::thread::sleep(Duration::from_millis(50));
    }
    recorder.stop();
    let s = recorder.stats().snapshot();
    let destination = s
        .destination
        .as_deref()
        .map_or(folder.clone(), |d| folder.join(d));
    println!(
        "Saved {} screenshots to {} in {:.1}s ({} skipped, {} failed)",
        s.saved,
        destination.display(),
        started.elapsed().as_secs_f64(),
        s.skipped,
        s.failed
    );
    match s.state {
        State::Ended(reason) => Err(reason),
        _ => Ok(()),
    }
}

/// Accepts IDs in hex (`0x1a2b`, as `--list` prints them) or decimal.
fn parse_id(text: &str) -> Result<WindowId, String> {
    let parsed = match text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        Some(hex) => u64::from_str_radix(hex, 16),
        None => text.parse(),
    };
    parsed
        .map(WindowId)
        .map_err(|_| format!("{text:?} isn't a window ID"))
}

/// On Windows the app has no console of its own, so print to the one it was started from.
pub fn attach_console() {
    #[cfg(windows)]
    unsafe {
        use windows::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_ids_parse_as_hex_or_decimal() {
        assert_eq!(parse_id("0x1a"), Ok(WindowId(26)));
        assert_eq!(parse_id("26"), Ok(WindowId(26)));
        assert!(parse_id("window").is_err());
    }

    #[test]
    fn unknown_options_fail() {
        assert_eq!(run(&["--bogus".to_string()]), 1);
    }
}
