//! The SnapLapse icon in the Windows notification area (the "system tray" by the clock).
//!
//! When the window is hidden, the app's own drawing loop is paused, so the tray handles its clicks
//! directly: it shows the window through Windows, stops the shared recording, or quits.

use std::cell::RefCell;
use std::ffi::c_void;

use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{
    IsIconic, SetForegroundWindow, ShowWindow, SW_RESTORE, SW_SHOW,
};

use crate::recorder::SharedRecorder;

thread_local! {
    // The icon must live on the thread that created it, which is also where its clicks arrive.
    static TRAY: RefCell<Option<TrayIcon>> = const { RefCell::new(None) };
}

pub fn install(
    hwnd: isize,
    recorder: SharedRecorder,
    rgba: Vec<u8>,
    width: u32,
    height: u32,
) -> Result<(), String> {
    let open = MenuItem::new("Open SnapLapse", true, None);
    let stop = MenuItem::new("Stop recording", true, None);
    let quit = MenuItem::new("Quit SnapLapse", true, None);
    let menu = Menu::new();
    menu.append_items(&[&open, &stop, &PredefinedMenuItem::separator(), &quit])
        .map_err(|e| e.to_string())?;

    let icon = Icon::from_rgba(rgba, width, height).map_err(|e| e.to_string())?;
    let tray = TrayIconBuilder::new()
        .with_icon(icon)
        .with_tooltip("SnapLapse")
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .build()
        .map_err(|e| e.to_string())?;
    TRAY.with(|t| *t.borrow_mut() = Some(tray));

    let (open_id, stop_id, quit_id) = (open.id().clone(), stop.id().clone(), quit.id().clone());
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        if event.id == open_id {
            show_window(hwnd);
        } else if event.id == stop_id {
            if let Some(recorder) = recorder.lock().unwrap().as_ref() {
                recorder.request_stop();
            }
        } else if event.id == quit_id {
            quit_app(&recorder);
        }
    }));
    TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
        if let TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } = event
        {
            show_window(hwnd);
        }
    }));
    Ok(())
}

fn show_window(hwnd: isize) {
    let hwnd = HWND(hwnd as *mut c_void);
    unsafe {
        let how = if IsIconic(hwnd).as_bool() {
            SW_RESTORE
        } else {
            SW_SHOW
        };
        let _ = ShowWindow(hwnd, how);
        let _ = SetForegroundWindow(hwnd);
    }
}

/// Finishes any recording (saving every screenshot and packing a ZIP if chosen), removes the
/// icon so no ghost icon is left by the clock, and exits. Settings are already saved, because
/// the app saves them as soon as they change.
fn quit_app(recorder: &SharedRecorder) {
    let current = recorder.lock().unwrap().take();
    if let Some(mut current) = current {
        current.stop();
    }
    TRAY.with(|t| t.borrow_mut().take());
    std::process::exit(0);
}
