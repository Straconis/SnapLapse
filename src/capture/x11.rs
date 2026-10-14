//! Linux capture through X11's Composite extension.
//!
//! Composite keeps a window's full contents in its own off-screen image, so the window can be
//! captured while other windows cover it. This also covers XWayland apps in a Wayland session.

use x11rb::connection::Connection;
use x11rb::protocol::composite::{ConnectionExt as _, Redirect};
use x11rb::protocol::xproto::{Atom, AtomEnum, ConnectionExt as _, ImageFormat, MapState, Window};
use x11rb::rust_connection::RustConnection;

use super::{CaptureError, Capturer, Frame, WindowId, WindowInfo};

struct Atoms {
    client_list: Atom,
    net_wm_name: Atom,
    utf8_string: Atom,
    net_wm_pid: Atom,
}

impl Atoms {
    fn new(conn: &RustConnection) -> Result<Self, String> {
        let intern = |name: &[u8]| -> Result<Atom, String> {
            Ok(conn
                .intern_atom(false, name)
                .map_err(|e| e.to_string())?
                .reply()
                .map_err(|e| e.to_string())?
                .atom)
        };
        Ok(Atoms {
            client_list: intern(b"_NET_CLIENT_LIST")?,
            net_wm_name: intern(b"_NET_WM_NAME")?,
            utf8_string: intern(b"UTF8_STRING")?,
            net_wm_pid: intern(b"_NET_WM_PID")?,
        })
    }
}

fn connect() -> Result<(RustConnection, Window), String> {
    let (conn, screen) =
        x11rb::connect(None).map_err(|e| format!("Couldn't connect to the X11 display: {e}"))?;
    let root = conn.setup().roots[screen].root;
    Ok((conn, root))
}

fn window_title(conn: &RustConnection, atoms: &Atoms, win: Window) -> Option<String> {
    let utf8 = conn
        .get_property(false, win, atoms.net_wm_name, atoms.utf8_string, 0, 1024)
        .ok()
        .and_then(|c| c.reply().ok());
    if let Some(reply) = utf8.filter(|r| !r.value.is_empty()) {
        return Some(String::from_utf8_lossy(&reply.value).into_owned());
    }
    let legacy = conn
        .get_property(false, win, AtomEnum::WM_NAME, AtomEnum::ANY, 0, 1024)
        .ok()?
        .reply()
        .ok()?;
    (!legacy.value.is_empty()).then(|| String::from_utf8_lossy(&legacy.value).into_owned())
}

fn window_pid(conn: &RustConnection, atoms: &Atoms, win: Window) -> Option<u32> {
    let reply = conn
        .get_property(false, win, atoms.net_wm_pid, AtomEnum::CARDINAL, 0, 1)
        .ok()?
        .reply()
        .ok()?;
    let pid = reply.value32()?.next();
    pid
}

pub fn list_windows() -> Result<Vec<WindowInfo>, String> {
    let (conn, root) = connect()?;
    let atoms = Atoms::new(&conn)?;
    let reply = conn
        .get_property(false, root, atoms.client_list, AtomEnum::WINDOW, 0, 4096)
        .map_err(|e| e.to_string())?
        .reply()
        .map_err(|e| e.to_string())?;
    let me = std::process::id();
    let mut windows = Vec::new();
    for win in reply.value32().into_iter().flatten() {
        if window_pid(&conn, &atoms, win) == Some(me) {
            continue;
        }
        if let Some(title) = window_title(&conn, &atoms, win) {
            let title = title.trim();
            if !title.is_empty() {
                windows.push(WindowInfo {
                    id: WindowId(u64::from(win)),
                    title: title.to_string(),
                });
            }
        }
    }
    Ok(windows)
}

pub struct X11Capturer {
    conn: RustConnection,
    atoms: Atoms,
    win: Window,
}

pub fn open(id: WindowId) -> Result<X11Capturer, CaptureError> {
    let (conn, _) = connect().map_err(CaptureError::Failed)?;
    let atoms = Atoms::new(&conn).map_err(CaptureError::Failed)?;
    let win = u32::try_from(id.0).map_err(|_| CaptureError::Gone)?;
    conn.get_window_attributes(win)
        .map_err(|e| CaptureError::Failed(e.to_string()))?
        .reply()
        .map_err(|_| CaptureError::Gone)?;
    conn.composite_query_version(0, 4)
        .map_err(|e| CaptureError::Failed(e.to_string()))?
        .reply()
        .map_err(|_| {
            CaptureError::Failed("This X server doesn't support the Composite extension".into())
        })?;
    // Ask the server to keep this window's contents off-screen. If the desktop already does
    // this, the request may be refused, which is fine.
    if let Ok(cookie) = conn.composite_redirect_window(win, Redirect::AUTOMATIC) {
        let _ = cookie.check();
    }
    Ok(X11Capturer { conn, atoms, win })
}

impl Capturer for X11Capturer {
    fn grab(&mut self) -> Result<Frame, CaptureError> {
        let failed = |e: &dyn std::fmt::Display| CaptureError::Failed(e.to_string());
        let conn = &self.conn;
        let attrs = conn
            .get_window_attributes(self.win)
            .map_err(|e| failed(&e))?
            .reply()
            .map_err(|_| CaptureError::Gone)?;
        if attrs.map_state != MapState::VIEWABLE {
            return Err(CaptureError::Minimized);
        }
        let geometry = conn
            .get_geometry(self.win)
            .map_err(|e| failed(&e))?
            .reply()
            .map_err(|_| CaptureError::Gone)?;

        let pixmap = conn.generate_id().map_err(|e| failed(&e))?;
        conn.composite_name_window_pixmap(self.win, pixmap)
            .map_err(|e| failed(&e))?
            .check()
            .map_err(|e| failed(&e))?;
        let image = conn
            .get_image(
                ImageFormat::Z_PIXMAP,
                pixmap,
                0,
                0,
                geometry.width,
                geometry.height,
                !0,
            )
            .map_err(|e| failed(&e))
            .and_then(|c| c.reply().map_err(|e| failed(&e)));
        let _ = conn.free_pixmap(pixmap);
        let _ = conn.flush();
        let image = image?;

        let bits_per_pixel = conn
            .setup()
            .pixmap_formats
            .iter()
            .find(|f| f.depth == image.depth)
            .map(|f| f.bits_per_pixel);
        if bits_per_pixel != Some(32) {
            return Err(CaptureError::Failed(format!(
                "Unsupported window color depth ({} bits)",
                image.depth
            )));
        }

        let (width, height) = (u32::from(geometry.width), u32::from(geometry.height));
        let len = width as usize * height as usize * 4;
        let mut bgra = image.data;
        if bgra.len() < len {
            return Err(CaptureError::Failed(
                "The window image was incomplete".into(),
            ));
        }
        bgra.truncate(len);
        // Windows without transparency leave this byte undefined, so make every pixel opaque.
        for alpha in bgra.iter_mut().skip(3).step_by(4) {
            *alpha = 255;
        }
        Ok(Frame {
            width,
            height,
            bgra,
        })
    }

    fn title(&self) -> Option<String> {
        window_title(&self.conn, &self.atoms, self.win)
    }
}

impl Drop for X11Capturer {
    fn drop(&mut self) {
        let _ = self
            .conn
            .composite_unredirect_window(self.win, Redirect::AUTOMATIC);
        let _ = self.conn.flush();
    }
}
