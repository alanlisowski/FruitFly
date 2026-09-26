# flit

A procedural cartoon fruit fly that walks a figure-eight over your windows. Click-through,
always on top, quit from the tray. No brain yet: the route is hardcoded (`path.rs`).

```
cargo run --release
cargo test                              # gait, no-skating, no-clipping, colour, snapshot checks
cargo run --release -- --snapshot out   # writes ref_zoomed / ref_stride / ref_actual_size .png, no window
```

Tray menu: **Pause** / **Resume** (stops all work), **Quit**.

## Layout

| File | Job |
|---|---|
| `art.rs` | The drawing: a tiny-skia port of `reference/fly_reference.py` (geometry, "green" palette, line weights, draw order, leg IK). `draw(pose, feet, scale, pixmap, origin)`. No Win32. |
| `fly.rs` | `FlyPose` and the gait: where each of the six `Feet` stands. No Win32. |
| `path.rs` | Stand-in for the brain: figure-eight, varying speed, one stop per lap. |
| `snapshot.rs` | `--snapshot`: re-renders the three reference images for side-by-side comparison. |
| `main.rs` | Window, DIB blit, DPI handling, tray, main loop. |
| `reference/` | The spec: `fly_reference.py` plus the three target PNGs. Not built or shipped. |

## Window flags, and why

Style: `WS_POPUP` -- no title bar, border or menu. The window is exactly our pixels.

| Extended style | What it buys us |
|---|---|
| `WS_EX_LAYERED` | Per-pixel alpha via `UpdateLayeredWindow`. Also means `WM_PAINT` is ignored; all drawing goes through `UpdateLayeredWindow`. |
| `WS_EX_TRANSPARENT` | Mouse hit-testing skips the window, so clicks land on whatever is underneath. |
| `WS_EX_TOPMOST` | Stays above normal windows. |
| `WS_EX_NOACTIVATE` | Never becomes the active window, so it never steals keyboard focus. |
| `WS_EX_TOOLWINDOW` | Not listed in Alt-Tab or on the taskbar. |

## The drawing (cairo -> tiny-skia)

- **Gradients follow the body.** tiny-skia applies the per-call `Transform` to the paint's
  shader too, so every gradient is built in body space and drawn with the body transform
  (`shading_moves_with_the_fly` checks it).
- **The light is fixed on the screen.** `light_in_body(heading)` rotates it into body space;
  the gradient highlight, heavy-ink offset, yellow glint, grey rim and eye glints all use it,
  so they stay top-left however the fly turns.
- **Ellipses are ovals** (`PathBuilder::from_oval` in body space, rotated where the reference
  rotates), never scaled circles, so outline thickness is uniform.
- **Clips** are `Mask`s built from the part's own path.
- **Nothing flickers**: fuzz, spikes and the thorax tuft are generated once from a seeded RNG
  (`art()`), in body space.
- **The shadow** is drawn first, in screen space, offset down-right, and the window is sized to
  hold it (`fly::RADIUS`; `fits_in_window` checks every border pixel at 100/150/200%).
- **Legs**: feet come from the gait; knees from the reference's two-bone IK with fixed
  femur/tibia lengths (`legs_never_cross_and_never_stretch`).

## Things that fail silently

- **Channel order**: tiny-skia gives premultiplied RGBA, the DIB wants premultiplied BGRA.
  `rgba_to_bgra` swaps R and B (unit-tested). Skip it and the fly's colours are wrong.
- **Sub-pixel jitter**: the window sits at the *floored* fly position; the fractional part
  is applied when drawing inside the pixmap (`draw` takes the window origin for this).
- **DPI**: the process is per-monitor-DPI-aware (first line of `main`). The fly is drawn at
  `FLY_SCALE * dpi/96`; the window is sized from the fly's bounding radius times that;
  `WM_DPICHANGED` rebuilds the buffers.
- **Skating feet**: gait phase advances by *distance walked*, so legs cycle faster when the
  fly runs and freeze when it stops. A planted foot stores a fixed *screen* position and each
  frame we compute where that spot is relative to the body, so it cannot slide at any speed,
  on any curve, at any frame rate (`planted_feet_stay_put`, `no_skating_and_clean_stops`).
- **Reach**: the reference's stand-in stride (+-3) overshoots full leg reach on the front and
  back legs, and legs have fixed lengths. So each leg's stride window is slid along x just
  enough to stay reachable, and the stride is 5 units: the largest round value at which no
  legs cross at any gait phase (at 6, a same-side front and middle leg form an X for a third
  of the cycle).
- **Stopping**: stops only happen at gait phases where all six feet are on the ground
  (multiples of half a cycle), so no foot is left hanging in the air.
- **Pause**: `Clock` only accumulates time while running and re-bases on every Pause/Resume, so
  a 10 s pause costs the fly no time and Resume continues exactly where it stopped. While paused
  the loop only pumps messages: no stepping, drawing or `UpdateLayeredWindow`.
- **Frame-rate independence**: motion uses `Instant` deltas (capped at 100 ms, because the
  tray menu is modal and blocks the loop while open).
- **`windows_subsystem = "windows"`**: otherwise a console flashes up on every launch.

## Cost

One frame is about 110 small tiny-skia draw calls: ~2.5 ms at 100% display scale, ~5 ms at
200% (`cargo test --release draw_cost -- --ignored --nocapture`). At 60 Hz that is roughly
15-30% of one core while walking. Ideas if it matters: cache the stroked outlines of the
static shapes, skip redraws while the fly is stopped, or drop to 30 Hz at slow speeds.
