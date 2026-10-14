<p align="center">
  <img src="branding/out/snaplapse-banner.png" alt="SnapLapse: timed screenshots of any window, even when it's covered" width="100%">
</p>

**SnapLapse** saves a screenshot of one window on a timer, every few seconds or milliseconds, until you press Stop. It works even when other windows are on top of the one you're capturing.

> **Status:** early development. There's nothing to download yet.

## Features

- **Captures a covered window.** Pick a window and SnapLapse keeps capturing it while other windows cover it. A minimized window can't be captured.
- **Set the interval in seconds or milliseconds.**
- **Choose the image format:** PNG, JPG or WebP.
- **Sortable file names:** files are numbered in order, like `0001 - Foundry VTT.png`.
- **Stays out of your way.** It never steals focus or moves windows, and it does no work between shots.
- **One small file to run.** There's nothing else to install.

## Platforms

| Platform | Notes |
|---|---|
| Windows 11 | Captures with no visible border. |
| Windows 10 | Windows draws a thin yellow border around the window being captured. |
| Linux (X11) | Captures silently. |
| Linux (Wayland) | GNOME and KDE ask you to choose the window once, then SnapLapse remembers your choice. |

## License

[MIT](LICENSE)
