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
pub fn open(
    id: WindowId,
    method: Method,
    pointer: bool,
) -> Result<Box<dyn Capturer>, CaptureError> {
    windows::open(id, method, pointer)
}

/// Lists the windows a person would expect to see in the taskbar or task switcher.
#[cfg(target_os = "linux")]
pub fn list_windows() -> Result<Vec<WindowInfo>, String> {
    x11::list_windows()
}

/// Linux has a single capture method, so `method` is ignored.
#[cfg(target_os = "linux")]
pub fn open(
    id: WindowId,
    _method: Method,
    pointer: bool,
) -> Result<Box<dyn Capturer>, CaptureError> {
    x11::open(id, pointer).map(|c| Box::new(c) as Box<dyn Capturer>)
}

#[cfg(not(any(windows, target_os = "linux")))]
pub fn list_windows() -> Result<Vec<WindowInfo>, String> {
    Err("This platform isn't supported yet".into())
}

#[cfg(not(any(windows, target_os = "linux")))]
pub fn open(
    _id: WindowId,
    _method: Method,
    _pointer: bool,
) -> Result<Box<dyn Capturer>, CaptureError> {
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

/// Draws a mouse pointer image onto a frame, for capture methods that leave the pointer out.
///
/// `pointer` holds `width` × `height` pixels as premultiplied ARGB, one number per pixel (the
/// form X11 provides). Its top-left corner goes at (`x`, `y`) in the frame; any part outside the
/// frame is left off.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn draw_pointer(frame: &mut Frame, pointer: &[u32], width: u32, height: u32, x: i32, y: i32) {
    let (fw, fh) = (frame.width as i64, frame.height as i64);
    for row in 0..height as i64 {
        let fy = y as i64 + row;
        if fy < 0 || fy >= fh {
            continue;
        }
        for col in 0..width as i64 {
            let fx = x as i64 + col;
            if fx < 0 || fx >= fw {
                continue;
            }
            let Some(&argb) = pointer.get((row * width as i64 + col) as usize) else {
                return;
            };
            let alpha = argb >> 24;
            if alpha == 0 {
                continue;
            }
            let i = ((fy * fw + fx) * 4) as usize;
            let keep = 255 - alpha;
            // Premultiplied: result = pointer colour + background × (1 − pointer opacity).
            for (channel, shift) in [(0usize, 0u32), (1, 8), (2, 16)] {
                let src = (argb >> shift) & 0xFF;
                let dst = u32::from(frame.bgra[i + channel]);
                frame.bgra[i + channel] = (src + (dst * keep + 127) / 255).min(255) as u8;
            }
        }
    }
}

/// Whether this build offers a choice of capture method.
pub const HAS_METHOD_CHOICE: bool = cfg!(windows);

#[cfg(test)]
mod tests {
    use super::*;

    fn grey_frame(width: u32, height: u32) -> Frame {
        Frame {
            width,
            height,
            bgra: vec![100; (width * height * 4) as usize],
        }
    }

    fn pixel(frame: &Frame, x: u32, y: u32) -> [u8; 3] {
        let i = ((y * frame.width + x) * 4) as usize;
        [frame.bgra[i], frame.bgra[i + 1], frame.bgra[i + 2]]
    }

    #[test]
    fn opaque_pointer_pixels_replace_the_picture() {
        let mut frame = grey_frame(4, 4);
        // A 2×1 pointer: opaque red, then fully transparent.
        draw_pointer(&mut frame, &[0xFF_FF0000, 0x00_000000], 2, 1, 1, 2);
        assert_eq!(pixel(&frame, 1, 2), [0, 0, 255]); // blue, green, red
        assert_eq!(pixel(&frame, 2, 2), [100, 100, 100]);
        assert_eq!(pixel(&frame, 0, 0), [100, 100, 100]);
    }

    #[test]
    fn see_through_pointer_pixels_blend() {
        let mut frame = grey_frame(1, 1);
        // Half-transparent white, premultiplied: 128 in every channel.
        draw_pointer(&mut frame, &[0x80_808080], 1, 1, 0, 0);
        assert_eq!(pixel(&frame, 0, 0), [178, 178, 178]); // 128 + 100 × 127/255
    }

    #[test]
    fn pointer_parts_outside_the_picture_are_left_off() {
        let mut frame = grey_frame(2, 2);
        let pointer = vec![0xFF_FFFFFF; 9];
        draw_pointer(&mut frame, &pointer, 3, 3, -2, -2); // only its bottom-right pixel lands
        assert_eq!(pixel(&frame, 0, 0), [255, 255, 255]);
        assert_eq!(pixel(&frame, 1, 0), [100, 100, 100]);
        draw_pointer(&mut frame, &pointer, 3, 3, 50, 50); // entirely outside: nothing happens
        assert_eq!(pixel(&frame, 1, 1), [100, 100, 100]);
    }

    mod properties {
        use super::super::*;
        use proptest::prelude::*;

        proptest! {
            /// Any pointer, anywhere, never panics and never touches the alpha bytes.
            #[test]
            fn drawing_a_pointer_is_always_safe(
                fw in 1u32..40, fh in 1u32..40, pw in 0u32..40, ph in 0u32..40,
                x in -60i32..60, y in -60i32..60, seed in any::<u32>(),
            ) {
                let mut frame = Frame { width: fw, height: fh, bgra: vec![7; (fw * fh * 4) as usize] };
                let pointer: Vec<u32> = (0..pw * ph).map(|i| i.wrapping_mul(2654435761) ^ seed).collect();
                draw_pointer(&mut frame, &pointer, pw, ph, x, y);
                prop_assert!(frame.bgra.iter().skip(3).step_by(4).all(|&a| a == 7));
            }
        }
    }
}
