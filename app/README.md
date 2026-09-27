# flit

A procedural pixel-art fruit fly that walks over your windows. Click-through,
always on top, quit from the tray. A spiking brain (`brain.rs`) drives it; `--demo-path` swaps in
the old hardcoded route (`path.rs`).

```
cargo run --release
cargo test                              # gait, no-skating, no-clipping, colour, snapshot checks
cargo run --release -- --snapshot out   # writes pixel_closeup / _turning / _walking / _actual_size .png, no window
```

Tray menu: **Pause** / **Resume** (stops all work), **Quit**.

## Layout

| File | Job |
|---|---|
| `art.rs` | The drawing: a tiny-skia port of `reference/fly_pixel.py` (palette, shapes, draw order, leg IK). `draw(pose, feet, scale, pixmap)`, `window_origin`. No Win32. |
| `fly.rs` | `FlyPose` and the gait: where each of the six `Feet` stands. No Win32. |
| `path.rs` | Stand-in for the brain: figure-eight, varying speed, one stop per lap. |
| `snapshot.rs` | `--snapshot`: re-renders the four `pixel_*` reference images for side-by-side comparison. |
| `main.rs` | Window, DIB blit, DPI handling, tray, main loop. |
| `../reference/` | The spec: `fly_pixel.py` plus the `pixel_*.png` targets. `old_green/` keeps the previous cartoon spec. Not built or shipped. |

## Window flags, and why

Style: `WS_POPUP` -- no title bar, border or menu. The window is exactly our pixels.

| Extended style | What it buys us |
|---|---|
| `WS_EX_LAYERED` | Per-pixel alpha via `UpdateLayeredWindow`. Also means `WM_PAINT` is ignored; all drawing goes through `UpdateLayeredWindow`. |
| `WS_EX_TRANSPARENT` | Mouse hit-testing skips the window, so clicks land on whatever is underneath. |
| `WS_EX_TOPMOST` | Stays above normal windows. |
| `WS_EX_NOACTIVATE` | Never becomes the active window, so it never steals keyboard focus. |
| `WS_EX_TOOLWINDOW` | Not listed in Alt-Tab or on the taskbar. |

## The drawing (pixel art, cairo -> tiny-skia)

- **Two steps per frame.** `render_art` rasterises the fly into a small art pixmap (1 px =
  1 art px), then `upscale` copies each art pixel into an `ART_PX x ART_PX` block of the
  window pixmap (a plain nearest-neighbour loop, no filtering).
- **`ART_PX = max(1, floor(dpi scale))`**: 1 at 100-175%, 2 at 200-275%. The body is drawn at
  `FLY_SCALE * scale / ART_PX` art px per body unit, so the fly is the same size on screen at
  every scale (~60 px across the legs at 100%) and only the chunkiness changes.
- **No anti-aliasing, anywhere.** Every fill, stroke and clip `Mask` goes through `Ctx`, which
  switches it off. Strokes of 1 art px or less are drawn as hairlines, because tiny-skia fades
  a sub-pixel line's alpha. `pixels_are_all_or_nothing` checks that every pixel's alpha is 0
  or 255, apart from the shadow's single flat value. A soft edge would show as a grey fringe
  on a dark wallpaper.
- **Flat colours, no gradients.** Each part is a `blob`: a 2-art-px outline stroke with the
  fill on top (leaving 1 art px of outline), a dark base, then the same shape nudged toward
  the light and clipped to the original for the mid tone, then flat highlights.
- **Any heading.** The shapes are re-rasterised onto the fixed art grid every frame, so pixels
  rearrange instead of blurring. There's no sprite cache: a frame costs ~0.3 ms.
- **The light is fixed on the screen.** `light_in_body(heading)` rotates it into body space
  (`light_stays_top_left_on_screen` finds the thorax highlight and eye glints on screen at
  0/90/180/270 deg).
- **The wing** is built in wing space; the translate/rotate is applied to the path itself, and
  that one path is used for the outline, fill and vein clip.
- **The shadow** is drawn first, in screen space, as a flat ellipse offset down-right. The
  window is sized to hold the whole fly (`fly::RADIUS`; `fits_in_window` checks every border
  pixel at 100/125/150/200%).
- **Legs**: feet come from the gait; knees from the reference's two-bone IK with fixed
  femur/tibia lengths, and a round foot (`legs_never_cross_and_never_stretch`).

## Things that fail silently

- **Channel order**: tiny-skia gives premultiplied RGBA, the DIB wants premultiplied BGRA.
  `rgba_to_bgra` swaps R and B (unit-tested). Skip it and the fly's colours are wrong.
- **Sub-pixel flicker**: non-AA edges rasterised at a moving fractional offset flicker. So the
  fly's centre sits on a fixed art-grid point, and the window moves in whole art pixels
  (`window_origin`). Only drawing snaps; the simulated position stays a float.
- **DPI**: the process is per-monitor-DPI-aware (first line of `main`). The fly is drawn at
  `FLY_SCALE * dpi/96` in whole art pixels; the window is sized from the fly's bounding radius times that;
  `WM_DPICHANGED` rebuilds the buffers.
- **Skating feet**: gait phase advances by *distance walked*, so legs cycle faster when the
  fly runs and freeze when it stops. A planted foot stores a fixed *screen* position and each
  frame we compute where that spot is relative to the body, so it cannot slide at any speed,
  on any curve, at any frame rate (`planted_feet_stay_put`, `no_skating_and_clean_stops`).
- **Reach**: legs have fixed lengths, and a stride around the reference's rest foot can
  overshoot full reach. So each leg's stride window is slid along x just enough to stay
  reachable, and the stride is 3 units: the largest round value at which no legs cross at any
  gait phase or on tight turns, and no foot leaves reach (at 3.5, legs cross on a curved walk).
- **Stopping**: stops only happen at gait phases where all six feet are on the ground
  (multiples of half a cycle), so no foot is left hanging in the air.
- **Pause**: `Clock` only accumulates time while running and re-bases on every Pause/Resume, so
  a 10 s pause costs the fly no time and Resume continues exactly where it stopped. While paused
  the loop only pumps messages: no stepping, drawing or `UpdateLayeredWindow`.
- **Frame-rate independence**: motion uses `Instant` deltas (capped at 100 ms, because the
  tray menu is modal and blocks the loop while open).
- **`windows_subsystem = "windows"`**: otherwise a console flashes up on every launch.

## Cost

A frame is a clear, a full re-rasterisation of the ~74 x 74 art pixmap (shadow, 6 legs,
body parts), and the nearest-neighbour upscale. Frames where the window position, heading
and gait phase are unchanged (standing still) are skipped entirely, and the loop sleeps
until the next frame is due.

Per frame on the route (`cargo test --release draw_cost -- --ignored --nocapture`): 100%
0.30 ms, 125% 0.33 ms, 200% 0.34 ms. In the app at 125%: ~5.5% of one core walking (brain
driving), ~0% standing still.
