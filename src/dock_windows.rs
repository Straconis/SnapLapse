//! The Windows side of auto-hide at a screen edge (see `dock.rs` for the rules).
//!
//! Three pieces make it feel smooth, like FastStone Capture:
//! - While the window is dragged, a magnet pulls it flush against a nearby screen edge. While
//!   docking is on, Windows' Snap can't maximize it, so it never jumps to full screen and back.
//! - A background thread checks the mouse about 30 times a second while docked, asks
//!   `dock::step` what to do, and does it.
//! - Hiding and showing glide the window off and back onto the edge instead of blinking.
//!
//! The red marker lets the mouse pass straight through it, so it never blocks clicks on whatever
//! is underneath.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::time::{Duration, Instant};

use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows::Win32::Graphics::Gdi::{
    CreateSolidBrush, GetMonitorInfoW, MonitorFromPoint, MonitorFromWindow, HMONITOR, MONITORINFO,
    MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
use windows::Win32::UI::WindowsAndMessaging::{
    CallWindowProcW, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    GetCursorPos, GetForegroundWindow, GetWindowLongPtrW, GetWindowRect, GetWindowThreadProcessId,
    IsIconic, IsWindow, IsWindowVisible, IsZoomed, PeekMessageW, RegisterClassW,
    SetLayeredWindowAttributes, SetWindowLongPtrW, SetWindowPos, ShowWindow, TranslateMessage,
    GWLP_WNDPROC, GWL_STYLE, HWND_NOTOPMOST, HWND_TOPMOST, LWA_ALPHA, MSG, PM_REMOVE,
    SWP_ASYNCWINDOWPOS, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
    SWP_SHOWWINDOW, SW_HIDE, SW_RESTORE, SW_SHOWNOACTIVATE, WM_MOVING, WNDCLASSW, WNDPROC,
    WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT,
    WS_MAXIMIZEBOX, WS_POPUP,
};

use crate::dock::{self, Action, Edge, Inputs, Phase, Rect, MARKER_LENGTH, MARKER_THICKNESS};

/// SnapLapse red, #DC2626, in Windows' blue-green-red order.
const MARKER_COLOR: COLORREF = COLORREF(0x0026_26DC);
/// How often to check the mouse while docked (fast, so it feels immediate) and otherwise.
const CHECK_DOCKED: Duration = Duration::from_millis(30);
const CHECK_FREE: Duration = Duration::from_millis(100);
const CHECK_OFF: Duration = Duration::from_millis(300);
/// The slide: 12 steps of 15 ms, about 0.18 seconds.
const SLIDE_STEPS: u32 = 12;
const SLIDE_STEP: Duration = Duration::from_millis(15);

/// Mirrors the app's "Auto-hide when docked" toggle.
static ENABLED: AtomicBool = AtomicBool::new(false);
/// The window's original message handler, which the drag magnet passes everything on to.
static ORIGINAL_HANDLER: AtomicIsize = AtomicIsize::new(0);

pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

/// Starts docking support. Must run on the thread that owns the window (the app's main thread),
/// because that's where the drag magnet has to be attached.
pub fn start(main: isize, on: bool) {
    set_enabled(on);
    let main = HWND(main as *mut c_void);
    unsafe {
        let original = SetWindowLongPtrW(main, GWLP_WNDPROC, drag_magnet as *const () as isize);
        ORIGINAL_HANDLER.store(original, Ordering::Relaxed);
    }
    let handle = main.0 as isize;
    let _ = std::thread::Builder::new()
        .name("snaplapse-dock".into())
        .spawn(move || watch(HWND(handle as *mut c_void)));
}

/// Runs for every message the window gets. While docking is on, it nudges the window's position
/// during a drag so it sticks to nearby screen edges, then hands the message on as usual.
unsafe extern "system" fn drag_magnet(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_MOVING && ENABLED.load(Ordering::Relaxed) {
        magnetize(hwnd, &mut *(lparam.0 as *mut RECT));
    }
    let original: WNDPROC = std::mem::transmute(ORIGINAL_HANDLER.load(Ordering::Relaxed));
    CallWindowProcW(original, hwnd, msg, wparam, lparam)
}

unsafe fn magnetize(hwnd: HWND, proposed: &mut RECT) {
    let Some((outer, visible)) = frame_rects(hwnd) else {
        return;
    };
    let mut cursor = POINT::default();
    if GetCursorPos(&mut cursor).is_err() {
        return;
    }
    // Use the monitor under the mouse, so the window can still be dragged to another screen.
    let Some(screen) = work_area(MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST)) else {
        return;
    };
    // Windows moves the outer frame, which includes invisible borders; the magnet works on the
    // part you can see.
    let shown = Rect {
        left: proposed.left + (visible.left - outer.left),
        top: proposed.top + (visible.top - outer.top),
        right: proposed.right - (outer.right - visible.right),
        bottom: proposed.bottom - (outer.bottom - visible.bottom),
    };
    let (dx, dy) = dock::magnet_offset(shown, screen);
    proposed.left += dx;
    proposed.right += dx;
    proposed.top += dy;
    proposed.bottom += dy;
}

fn watch(main: HWND) {
    let Some(marker) = (unsafe { create_marker() }) else {
        return;
    };
    let started = Instant::now();
    let mut phase = Phase::Free;
    let mut snap_blocked = false;
    loop {
        pump_messages();
        if unsafe { !IsWindow(main).as_bool() } {
            break;
        }
        let on = ENABLED.load(Ordering::Relaxed);
        if on != snap_blocked {
            unsafe { set_maximizable(main, !on) };
            snap_blocked = on;
        }
        if !on && phase == Phase::Free {
            // Nothing to do; check back now and then in case the toggle is switched on.
            std::thread::sleep(CHECK_OFF);
            continue;
        }
        if let Some(inputs) = unsafe { read_inputs(main, marker, on, started) } {
            let (next, actions) = dock::step(phase, &inputs);
            phase = next;
            for action in actions {
                unsafe { apply(action, main, marker) };
            }
        }
        std::thread::sleep(if phase == Phase::Free {
            CHECK_FREE
        } else {
            CHECK_DOCKED
        });
    }
    unsafe {
        let _ = DestroyWindow(marker);
    }
}

/// Windows' Snap only maximizes windows that have a maximize button, so removing it while
/// docking is on stops the window jumping to full screen when dragged to the top.
unsafe fn set_maximizable(main: HWND, maximizable: bool) {
    let style = GetWindowLongPtrW(main, GWL_STYLE);
    let flag = WS_MAXIMIZEBOX.0 as isize;
    let new_style = if maximizable {
        style | flag
    } else {
        style & !flag
    };
    if new_style != style {
        SetWindowLongPtrW(main, GWL_STYLE, new_style);
        let flags = SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE;
        let _ = SetWindowPos(main, None, 0, 0, 0, 0, flags | SWP_ASYNCWINDOWPOS);
    }
}

fn to_rect(r: RECT) -> Rect {
    Rect {
        left: r.left,
        top: r.top,
        right: r.right,
        bottom: r.bottom,
    }
}

/// The window's outer frame (what Windows moves) and the part you can see (without the
/// invisible resize borders Windows adds).
unsafe fn frame_rects(hwnd: HWND) -> Option<(RECT, RECT)> {
    let mut outer = RECT::default();
    GetWindowRect(hwnd, &mut outer).ok()?;
    let mut visible = outer;
    let _ = DwmGetWindowAttribute(
        hwnd,
        DWMWA_EXTENDED_FRAME_BOUNDS,
        &mut visible as *mut RECT as *mut c_void,
        std::mem::size_of::<RECT>() as u32,
    );
    Some((outer, visible))
}

/// The usable part of a monitor, not counting the taskbar.
unsafe fn work_area(monitor: HMONITOR) -> Option<Rect> {
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    GetMonitorInfoW(monitor, &mut info)
        .as_bool()
        .then(|| to_rect(info.rcWork))
}

unsafe fn read_inputs(main: HWND, marker: HWND, enabled: bool, started: Instant) -> Option<Inputs> {
    let (_, visible) = frame_rects(main)?;
    let screen = work_area(MonitorFromWindow(main, MONITOR_DEFAULTTONEAREST))?;
    let mut cursor = POINT::default();
    GetCursorPos(&mut cursor).ok()?;

    // Another SnapLapse window in front, such as the folder picker, means "don't hide".
    let foreground = GetForegroundWindow();
    let mut pid = 0u32;
    GetWindowThreadProcessId(foreground, Some(&mut pid));
    let dialog_open = pid == std::process::id() && foreground != main && foreground != marker;

    Some(Inputs {
        now_ms: started.elapsed().as_millis() as u64,
        enabled,
        window_visible: IsWindowVisible(main).as_bool() && !IsIconic(main).as_bool(),
        maximized: IsZoomed(main).as_bool(),
        window: to_rect(visible),
        screen,
        cursor: (cursor.x, cursor.y),
        // The top bit is set while the button is held.
        mouse_down: GetAsyncKeyState(i32::from(VK_LBUTTON.0)) < 0,
        dialog_open,
    })
}

unsafe fn apply(action: Action, main: HWND, marker: HWND) {
    match action {
        Action::SetTopmost(on) => {
            let order = if on { HWND_TOPMOST } else { HWND_NOTOPMOST };
            let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS;
            let _ = SetWindowPos(main, order, 0, 0, 0, 0, flags);
        }
        Action::Hide { marker: spot, edge } => {
            slide_out(main, edge);
            let _ = SetWindowPos(
                marker,
                HWND_TOPMOST,
                spot.left,
                spot.top,
                spot.width(),
                spot.height(),
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
        }
        Action::Show { edge } => {
            let _ = ShowWindow(marker, SW_HIDE);
            match edge {
                Some(edge) => slide_in(main, edge),
                None => {
                    let _ = ShowWindow(main, SW_SHOWNOACTIVATE);
                }
            }
        }
        Action::RemoveMarker => {
            let _ = ShowWindow(marker, SW_HIDE);
        }
        Action::DockAtTop { cursor } => dock_at_top(main, cursor),
    }
}

/// Where the window is now, and where it would be once it's slid completely past `edge`.
unsafe fn slide_path(main: HWND, edge: Edge) -> Option<((i32, i32), (i32, i32))> {
    let (outer, visible) = frame_rects(main)?;
    let screen = work_area(MonitorFromWindow(main, MONITOR_DEFAULTTONEAREST))?;
    let (dx, dy) = dock::off_screen_offset(edge, to_rect(visible), screen);
    Some(((outer.left, outer.top), (outer.left + dx, outer.top + dy)))
}

/// Glides the window off the screen past `edge`, hides it, then quietly puts it back in its
/// docked spot so that anything that shows it again (such as the tray icon) finds it there.
unsafe fn slide_out(main: HWND, edge: Edge) {
    let Some((home, away)) = slide_path(main, edge) else {
        let _ = ShowWindow(main, SW_HIDE);
        return;
    };
    glide(main, home, away, |t| t * t); // speeds up as it leaves
    let _ = ShowWindow(main, SW_HIDE);
    move_to(main, home);
}

/// Shows the window just past `edge` and glides it back into its docked spot.
unsafe fn slide_in(main: HWND, edge: Edge) {
    let Some((home, away)) = slide_path(main, edge) else {
        let _ = ShowWindow(main, SW_SHOWNOACTIVATE);
        return;
    };
    move_to(main, away);
    let _ = ShowWindow(main, SW_SHOWNOACTIVATE);
    glide(main, away, home, |t| 1.0 - (1.0 - t).powi(3)); // slows down as it settles
}

unsafe fn glide(main: HWND, from: (i32, i32), to: (i32, i32), ease: impl Fn(f32) -> f32) {
    for step in 1..=SLIDE_STEPS {
        let progress = ease(step as f32 / SLIDE_STEPS as f32);
        let x = from.0 + ((to.0 - from.0) as f32 * progress).round() as i32;
        let y = from.1 + ((to.1 - from.1) as f32 * progress).round() as i32;
        move_to(main, (x, y));
        std::thread::sleep(SLIDE_STEP);
    }
}

unsafe fn move_to(main: HWND, (x, y): (i32, i32)) {
    let _ = SetWindowPos(
        main,
        None,
        x,
        y,
        0,
        0,
        SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
    );
}

/// A backup in case Windows maximizes the window anyway: put it back to its normal size and
/// dock it at the top of the screen the mouse is on, centred under the mouse.
unsafe fn dock_at_top(main: HWND, (cursor_x, cursor_y): (i32, i32)) {
    let _ = ShowWindow(main, SW_RESTORE);
    let Some((outer, visible)) = frame_rects(main) else {
        return;
    };
    let monitor = MonitorFromPoint(
        POINT {
            x: cursor_x,
            y: cursor_y,
        },
        MONITOR_DEFAULTTONEAREST,
    );
    let Some(work) = work_area(monitor) else {
        return;
    };
    let width = visible.right - visible.left;
    let left = (cursor_x - width / 2).clamp(work.left, (work.right - width).max(work.left));
    let x = left - (visible.left - outer.left);
    let y = work.top - (visible.top - outer.top);
    move_to(main, (x, y));
}

/// The small red bar on the screen edge. It's see-through to the mouse, never takes focus, and
/// doesn't appear in the taskbar or Alt+Tab.
unsafe fn create_marker() -> Option<HWND> {
    let instance = GetModuleHandleW(None).ok()?;
    let class = w!("SnapLapseDockMarker");
    let window_class = WNDCLASSW {
        lpfnWndProc: Some(marker_proc),
        hInstance: instance.into(),
        lpszClassName: class,
        hbrBackground: CreateSolidBrush(MARKER_COLOR),
        ..Default::default()
    };
    RegisterClassW(&window_class);
    let marker = CreateWindowExW(
        WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_LAYERED | WS_EX_TRANSPARENT,
        class,
        w!("SnapLapse"),
        WS_POPUP,
        0,
        0,
        MARKER_LENGTH,
        MARKER_THICKNESS,
        None,
        None,
        instance,
        None,
    )
    .ok()?;
    let _ = SetLayeredWindowAttributes(marker, COLORREF(0), 235, LWA_ALPHA);
    Some(marker)
}

unsafe extern "system" fn marker_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

/// Lets Windows deliver this thread's window messages, so the marker gets drawn.
fn pump_messages() {
    unsafe {
        let mut msg = MSG::default();
        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
