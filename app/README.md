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

Only the legs change shape, so `art::Cache` keeps two bitmaps: the rigid body (wings, head,
eyes, antennae...) rendered at 2x in body space, and the screen-space shadow. A frame is the
shadow blit, the legs (9 batched draw calls), and the body blit rotated and scaled by 0.5
(2x so rotation doesn't soften the outlines). The body bitmap is lit for one heading and
re-rendered when the fly turns more than 8 deg away from it (~170 times a minute on the
route); a scale change rebuilds both. `art::draw` stays the all-vector reference, and
`cache_matches_vector` holds the two within 3/255. Frames where position, heading and gait
phase are unchanged (standing still) are skipped entirely, and the loop sleeps until the next
frame is due.

Per frame on the route (`cargo test --release draw_cost -- --ignored --nocapture`): 100%
0.75 ms (vector 2.0), 200% 1.7 ms (vector 3.6). Cache: 145 KiB at 100%, 560 KiB at 200%.
In the app at 125%: ~9% of one core walking, of which `UpdateLayeredWindow` is ~0.5 ms a
frame (~3%) on its own; ~0.1% standing still.
