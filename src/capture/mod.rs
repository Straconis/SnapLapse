//! Finding windows and grabbing their contents. Each platform has its own backend.

use std::fmt;

use serde::{Deserialize, Serialize};

#[cfg(windows)]
mod windows;
#[cfg(target_os = "linux")]
mod x11;
#[cfg(windows)]
pub use self::windows::show_error;

/// Identifies a window for as long as it stays open, even if its title changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WindowId(pub u64);

#[derive(Clone, Debug)]
pub struct WindowInfo {
    pub id: WindowId,
    pub title: String,
}

/// One captured image, stored as rows of blue-green-red-alpha bytes with no padding.
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

impl fmt::Debug for Frame {
    // Shows the size only. Printing millions of pixel bytes would bury test output.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Frame({}x{})", self.width, self.height)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum CaptureError {
    /// The window exists but is minimized, so the system isn't drawing it.
    Minimized,
    /// The window was closed.
    Gone,
    Failed(String),
}

impl fmt::Display for CaptureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CaptureError::Minimized => f.write_str("The window is minimized"),
            CaptureError::Gone => f.write_str("The window was closed"),
            CaptureError::Failed(msg) => f.write_str(msg),
        }
    }
}

impl std::error::Error for CaptureError {}

/// How to capture on Windows. Other platforms have a single method and ignore this.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Method {
    /// Windows Graphics Capture, falling back to PrintWindow if it isn't available.
    #[default]
    Auto,
    /// Windows Graphics Capture: the modern API OBS and Teams use.
    GraphicsCapture,
    /// The older PrintWindow API. Never shows a capture border, but some apps come out black.
    PrintWindow,
}

impl Method {
    pub const ALL: [Method; 3] = [Method::Auto, Method::GraphicsCapture, Method::PrintWindow];

    pub fn label(self) -> &'static str {
        match self {
            Method::Auto => "Automatic",
            Method::GraphicsCapture => "Windows Graphics Capture",
            Method::PrintWindow => "PrintWindow (no border)",
        }
    }
}

pub trait Capturer {
    fn grab(&mut self) -> Result<Frame, CaptureError>;
    /// The window's current title, which can change while recording (for example, browser tabs).
    fn title(&self) -> Option<String>;
}

/// Lists the windows a person would expect to see in the taskbar or task switcher.
#[cfg(windows)]
pub fn list_windows() -> Result<Vec<WindowInfo>, String> {
    Ok(windows::list_windows())
}

#[cfg(windows)]
pub fn open(id: WindowId, method: Method) -> Result<Box<dyn Capturer>, CaptureError> {
    windows::open(id, method)
}

/// Lists the windows a person would expect to see in the taskbar or task switcher.
#[cfg(target_os = "linux")]
pub fn list_windows() -> Result<Vec<WindowInfo>, String> {
    x11::list_windows()
}

/// Linux has a single capture method, so `method` is ignored.
#[cfg(target_os = "linux")]
pub fn open(id: WindowId, _method: Method) -> Result<Box<dyn Capturer>, CaptureError> {
    x11::open(id).map(|c| Box::new(c) as Box<dyn Capturer>)
}

#[cfg(not(any(windows, target_os = "linux")))]
pub fn list_windows() -> Result<Vec<WindowInfo>, String> {
    Err("This platform isn't supported yet".into())
}

#[cfg(not(any(windows, target_os = "linux")))]
pub fn open(_id: WindowId, _method: Method) -> Result<Box<dyn Capturer>, CaptureError> {
    Err(CaptureError::Failed(
        "This platform isn't supported yet".into(),
    ))
}

/// A note to show in the app about limits of the current desktop session, if any.
pub fn platform_note() -> Option<&'static str> {
    #[cfg(target_os = "linux")]
    if std::env::var("XDG_SESSION_TYPE").is_ok_and(|s| s.eq_ignore_ascii_case("wayland")) {
        return Some("Wayland session: only apps running through XWayland are listed for now.");
    }
    None
}

/// Whether this build offers a choice of capture method.
pub const HAS_METHOD_CHOICE: bool = cfg!(windows);
