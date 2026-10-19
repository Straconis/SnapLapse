// Release builds on Windows open without a console window.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod capture;
mod cleanup;
mod cli;
// The docking rules are only used on Windows, but they are tested everywhere.
#[cfg_attr(not(windows), allow(dead_code))]
mod dock;
#[cfg(windows)]
mod dock_windows;
mod encode;
mod naming;
mod pack;
mod recorder;
mod settings;
#[cfg(windows)]
mod tray;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !args.is_empty() {
        cli::attach_console();
        std::process::exit(cli::run(&args));
    }
    if let Err(e) = app::run() {
        let message = format!("SnapLapse couldn't start: {e}");
        eprintln!("{message}");
        #[cfg(windows)]
        capture::show_error(&message);
        std::process::exit(1);
    }
}

/// A fresh, empty folder for one test.
#[cfg(test)]
pub fn test_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("snaplapse-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
