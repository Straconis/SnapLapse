//! The Windows side of auto-hide at a screen edge (see `dock.rs` for the rules).
//!
//! A background thread checks the mouse and window about ten times a second, asks `dock::step`
//! what to do, and does it: hides or shows the window, keeps it on top while docked, and draws
//! the red marker. The marker lets the mouse pass straight through it, so it never blocks clicks
//! on whatever is underneath.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows::Win32::Graphics::Gdi::{
    CreateSolidBrush, GetMonitorInfoW, MonitorFromPoint, MonitorFromWindow, MONITORINFO,
    MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetCursorPos,
    GetForegroundWindow, GetWindowRect, GetWindowThreadProcessId, IsIconic, IsWindow,
    IsWindowVisible, IsZoomed, PeekMessageW, RegisterClassW, SetLayeredWindowAttributes,
    SetWindowPos, ShowWindow, ShowWindowAsync, TranslateMessage, HWND_NOTOPMOST, HWND_TOPMOST,
    LWA_ALPHA, MSG, PM_REMOVE, SWP_ASYNCWINDOWPOS, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    SWP_SHOWWINDOW, SW_HIDE, SW_SHOWNOACTIVATE, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

use windows::Win32::UI::WindowsAndMessaging::{SWP_NOZORDER, SW_RESTORE};

use crate::dock::{self, Action, Inputs, Phase, Rect, MARKER_LENGTH, MARKER_THICKNESS};

/// SnapLapse red, #DC2626, in Windows' blue-green-red order.
const MARKER_COLOR: COLORREF = COLORREF(0x0026_26DC);
const CHECK_EVERY: Duration = Duration::from_millis(100);
const IDLE_CHECK_EVERY: Duration = Duration::from_millis(300);

/// Starts watching. `enabled` mirrors the app's "Auto-hide at screen edge" toggle.
pub fn start(main: isize, enabled: Arc<AtomicBool>) {
    let _ = std::thread::Builder::new()
        .name("snaplapse-dock".into())
        .spawn(move || watch(HWND(main as *mut c_void), &enabled));
}

fn watch(main: HWND, enabled: &AtomicBool) {
    let Some(marker) = (unsafe { create_marker() }) else {
        return;
    };
    let started = Instant::now();
    let mut phase = Phase::Free;
    loop {
        pump_messages();
        if unsafe { !IsWindow(main).as_bool() } {
            break;
        }
        let on = enabled.load(Ordering::Relaxed);
        if !on && phase == Phase::Free {
            // Nothing to do; check back now and then in case the toggle is switched on.
            std::thread::sleep(IDLE_CHECK_EVERY);
            continue;
        }
        if let Some(inputs) = unsafe { read_inputs(main, marker, on, started) } {
            let (next, actions) = dock::step(phase, &inputs);
            phase = next;
            for action in actions {
                unsafe { apply(action, main, marker) };
            }
        }
        std::thread::sleep(CHECK_EVERY);
    }
    unsafe {
        let _ = DestroyWindow(marker);
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

unsafe fn read_inputs(main: HWND, marker: HWND, enabled: bool, started: Instant) -> Option<Inputs> {
    // The visible frame, without the invisible resize borders Windows adds around windows.
    let mut window = RECT::default();
    let frame = DwmGetWindowAttribute(
        main,
        DWMWA_EXTENDED_FRAME_BOUNDS,
        &mut window as *mut RECT as *mut c_void,
        std::mem::size_of::<RECT>() as u32,
    );
    if frame.is_err() {
        GetWindowRect(main, &mut window).ok()?;
    }
    let mut monitor = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if !GetMonitorInfoW(
        MonitorFromWindow(main, MONITOR_DEFAULTTONEAREST),
        &mut monitor,
    )
    .as_bool()
    {
        return None;
    }
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
        window: to_rect(window),
        screen: to_rect(monitor.rcWork),
        cursor: (cursor.x, cursor.y),
        // The top bit is set while the button is held.
        mouse_down: GetAsyncKeyState(i32::from(VK_LBUTTON.0)) < 0,
        dialog_open,
    })
}

unsafe fn apply(action: Action, main: HWND, marker: HWND) {
    let keep_size = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS;
    match action {
        Action::SetTopmost(on) => {
            let order = if on { HWND_TOPMOST } else { HWND_NOTOPMOST };
            let _ = SetWindowPos(main, order, 0, 0, 0, 0, keep_size);
        }
        Action::Hide { marker: m } => {
            let _ = ShowWindowAsync(main, SW_HIDE);
            let _ = SetWindowPos(
                marker,
                HWND_TOPMOST,
                m.left,
                m.top,
                m.width(),
                m.height(),
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
        }
        Action::Show => {
            let _ = ShowWindow(marker, SW_HIDE);
            let _ = ShowWindowAsync(main, SW_SHOWNOACTIVATE);
        }
        Action::RemoveMarker => {
            let _ = ShowWindow(marker, SW_HIDE);
        }
        Action::DockAtTop { cursor } => dock_at_top(main, cursor),
    }
}

/// Un-maximizes the window and places it against the top of the screen the mouse is on,
/// centred under the mouse, at its normal size.
unsafe fn dock_at_top(main: HWND, (cursor_x, cursor_y): (i32, i32)) {
    let _ = ShowWindow(main, SW_RESTORE);
    let mut outer = RECT::default();
    if GetWindowRect(main, &mut outer).is_err() {
        return;
    }
    let mut visible = outer;
    let _ = DwmGetWindowAttribute(
        main,
        DWMWA_EXTENDED_FRAME_BOUNDS,
        &mut visible as *mut RECT as *mut c_void,
        std::mem::size_of::<RECT>() as u32,
    );
    let mut monitor = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    let screen = MonitorFromPoint(
        POINT {
            x: cursor_x,
            y: cursor_y,
        },
        MONITOR_DEFAULTTONEAREST,
    );
    if !GetMonitorInfoW(screen, &mut monitor).as_bool() {
        return;
    }
    let work = monitor.rcWork;
    let width = visible.right - visible.left;
    let left = (cursor_x - width / 2).clamp(work.left, (work.right - width).max(work.left));
    // Windows positions the outer frame, which includes invisible borders, so allow for them.
    let x = left - (visible.left - outer.left);
    let y = work.top - (visible.top - outer.top);
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
