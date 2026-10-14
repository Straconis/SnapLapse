# SnapLapse

Takes a screenshot of one chosen window every N seconds or milliseconds and saves the images to a folder, until the user clicks Stop. The main use is capturing a D&D VTT browser window. It's the first step toward the separate vtt-cameraman project, which adds video.

## Requirements

- **Capture a covered window.** It must work when other windows are on top. A minimized window can't be captured, so the app should say that clearly.
- **Controls:** pick a window, a save folder, an interval in seconds or milliseconds, an image format (PNG by default, also JPG and WebP), and Start and Stop buttons.
- **File names:** `<zero-padded number> - <window title>.<ext>`, for example `000001 - Foundry VTT.png`. Remove characters that filenames can't contain. Track the window itself, not its title, because browser titles change when you switch tabs.
- **Very light and fast. It must never interfere with the user.** Don't steal focus or move or raise windows. Do no work between shots. Encode and save images on a low-priority background thread. No popups or sounds while running.
- **Platforms:** Windows 10, Windows 11 and Linux. Development happens on Debian 13 with GNOME (an X11 session today, but Wayland must be supported too).

## Tech decisions

- **Language:** Rust, producing a single small executable with no runtime to install.
- **Windows capture:** the Windows.Graphics.Capture API. Turn off the yellow capture border where Windows 11 allows it. Windows 10 always shows it.
- **Linux capture:** XComposite on X11. On Wayland, the xdg-desktop-portal ScreenCast portal with PipeWire, saving the portal's restore token so the window picker isn't shown every time.
- **Builds:** Windows binaries are cross-compiled on Linux, and CI builds both platforms on GitHub Actions.

## Working with the owner

The owner can't read Rust. Claude is responsible for code quality, so cover behaviour with automated tests, keep CI green, and explain changes in plain language rather than through code.
