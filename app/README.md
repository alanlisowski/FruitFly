# flit

A procedural fruit fly that walks a figure-eight over your windows. Click-through, always on
top, quit from the tray. No brain yet: the route is hardcoded (`path.rs`).

```
cargo run --release
cargo test          # gait, no-skating, no-clipping, channel-order checks
```

Tray menu: **Pause** / **Resume** (stops all work), **Quit**.

## Layout

| File | Job |
|---|---|
| `fly.rs` | `FlyPose`, the six `Feet`, and a pure `draw(pose, feet, ..)` into a tiny-skia pixmap. No Win32. |
| `path.rs` | Milestone-2 stand-in for the brain: figure-eight, varying speed, one stop per lap. |
| `main.rs` | Window, DIB blit, DPI handling, tray, main loop. |

The fly's proportions and colours are ported from `drawFly()` / `LEGS` in `clients/web/index.html`.

## Window flags, and why

Style: `WS_POPUP` -- no title bar, border or menu. The window is exactly our pixels.

| Extended style | What it buys us |
|---|---|
| `WS_EX_LAYERED` | Per-pixel alpha via `UpdateLayeredWindow`. Also means `WM_PAINT` is ignored; all drawing goes through `UpdateLayeredWindow`. |
| `WS_EX_TRANSPARENT` | Mouse hit-testing skips the window, so clicks land on whatever is underneath. |
| `WS_EX_TOPMOST` | Stays above normal windows. |
| `WS_EX_NOACTIVATE` | Never becomes the active window, so it never steals keyboard focus. |
| `WS_EX_TOOLWINDOW` | Not listed in Alt-Tab or on the taskbar. |

## Things that fail silently

- **Channel order**: tiny-skia gives premultiplied RGBA, the DIB wants premultiplied BGRA.
  `rgba_to_bgra` swaps R and B (unit-tested). Skip it and the fly is blue-eyed.
- **Sub-pixel jitter**: the window sits at the *floored* fly position; the fractional part
  is applied when drawing inside the pixmap (`draw` takes the window origin for this).
- **DPI**: the process is per-monitor-DPI-aware (first line of `main`). The fly is drawn at
  `BODY_SCALE * dpi/96`; the window is sized from the fly's bounding radius (`fly::RADIUS`)
  times that; `WM_DPICHANGED` rebuilds the buffers. `fits_in_window` renders every pose at
  100/150/200% and fails if any border pixel is touched.
- **Skating feet**: gait phase advances by *distance walked*, so legs cycle faster when the
  fly runs and freeze when it stops. A planted foot stores a fixed *screen* position and each
  frame we compute where that spot is relative to the body, so it cannot slide at any speed,
  on any curve, at any frame rate. (`planted_feet_stay_put` and `no_skating_and_clean_stops`
  check this; measured slip is ~0.00002 px/frame.)
- **Stopping**: stops only happen at gait phases where all six feet are on the ground
  (multiples of half a cycle), so no foot is left hanging in the air.
- **Frame-rate independence**: motion uses `Instant` deltas (capped at 100 ms, because the
  tray menu is modal and blocks the loop while open).
- **`windows_subsystem = "windows"`**: otherwise a console flashes up on every launch.
