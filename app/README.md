# flit

Milestone 1: a soft-edged red square that floats over every window, ignores the mouse and
keyboard, and circles the screen centre. Quit from the tray icon. No fly, no brain yet.

```
cargo run --release
cargo test          # checks the pixel buffer is premultiplied
```

## Window flags, and why

Style: `WS_POPUP` -- no title bar, border or menu. The window is exactly our pixels.
(`WS_OVERLAPPEDWINDOW` would add a frame we would then have to fight.)

| Extended style | What it buys us |
|---|---|
| `WS_EX_LAYERED` | Per-pixel alpha via `UpdateLayeredWindow`: soft edges and any shape. Also means `WM_PAINT` is ignored; all drawing goes through `UpdateLayeredWindow`. |
| `WS_EX_TRANSPARENT` | Mouse hit-testing skips the window, so clicks land on whatever is underneath. |
| `WS_EX_TOPMOST` | Stays above normal windows. |
| `WS_EX_NOACTIVATE` | Never becomes the active window, so it never steals keyboard focus. |
| `WS_EX_TOOLWINDOW` | Not listed in Alt-Tab or on the taskbar. |

Other things that fail silently:

- **DPI awareness** (`SetProcessDpiAwarenessContext`, first line of `main`): without it Windows
  scales our coordinates on any display not at 100%.
- **`SW_SHOWNOACTIVATE`**: plain `SW_SHOW` would activate the window and steal focus once.
- **Premultiplied alpha**: `AC_SRC_ALPHA` expects colour channels already multiplied by
  alpha/255. Straight alpha gives a dark halo, not an error.
- **160x160 window**, moved with `UpdateLayeredWindow`'s position argument: never a fullscreen
  layered window, so per-frame cost stays tiny.
- **`windows_subsystem = "windows"`**: otherwise a console flashes up on every launch.
- **Tray icon** (`tray-icon` crate): the window is click-through, so the tray is the only way
  to quit. It shares the main thread's `PeekMessage` loop; no fallback to `Shell_NotifyIcon` was needed.
