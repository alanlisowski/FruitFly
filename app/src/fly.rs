//! The fly's body: a pose, the feet, and a pure function that draws them. No Win32 in here, so
//! the brain (milestone 3) can drive `FlyPose` without touching any rendering code.
//!
//! Proportions and colours are ported from `drawFly()` / `LEGS` in clients/web/index.html.
//! All "web units" below are that file's canvas units; `draw` multiplies them by
//! `BODY_SCALE * dpi_scale` to get physical pixels.

use tiny_skia::{
    Color, FillRule, LineCap, Paint, Path, PathBuilder, Pixmap, Rect, Stroke, Transform,
};

/// The web version does `ctx.scale(1.35, 1.35)` before drawing the fly.
pub const BODY_SCALE: f32 = 1.35;

// --- Gait ---------------------------------------------------------------------------------
// One gait cycle per leg = STANCE (foot on the ground, moving backward relative to the body)
// followed by SWING (foot in the air, moving forward). Tripod gait: the two tripods run half a
// cycle apart, so three feet are always down.
const STANCE: f32 = 0.6;
const SWING: f32 = 1.0 - STANCE;
/// How far (web units) a planted foot travels backward relative to the body per step.
const STRIDE: f32 = 12.0;
/// Peak lift of a swinging foot, drawn as a sideways offset (we look from above).
const LIFT: f32 = 2.5;
/// Distance the body walks per full gait cycle. Derivation: a foot stays put on the ground for
/// the whole stance (60% of the cycle), during which it moves back `STRIDE` relative to the
/// body, so the body must travel exactly `STRIDE` in 60% of the cycle => STRIDE / 0.6.
pub const CYCLE: f32 = STRIDE / STANCE;

/// Farthest any part of the fly can reach from its centre, in web units, at any heading, gait
/// phase or turn rate. Sizes the window. (Leg L3 at full stride is ~33; wings/abdomen ~29;
/// the rest is margin. `fits_in_window` below proves it by rendering.)
const RADIUS: f32 = 36.0;

/// Window edge length in physical pixels for a given DPI scale (dpi / 96).
/// Even, so the centre is a whole pixel, plus a small margin for anti-aliasing.
pub fn window_size(scale: f32) -> i32 {
    ((2.0 * RADIUS * BODY_SCALE * scale).ceil() as i32 + 4 + 1) & !1
}

/// Where the fly is and how its legs are. Everything the drawing needs, nothing it doesn't.
#[derive(Clone, Copy, Debug)]
pub struct FlyPose {
    /// Centre of the thorax in physical screen pixels (a float: sub-pixel positions matter).
    pub x: f32,
    pub y: f32,
    /// Radians, 0 = facing right, increasing clockwise on screen (y points down).
    pub heading: f32,
    /// Physical pixels per second. Not needed for drawing (legs are driven by `gait_phase`,
    /// which stands still when the fly does); it's here for the brain to read/write.
    #[allow(dead_code)]
    pub speed: f32,
    /// Position in the gait cycle, 0..1, advanced by distance walked (see `path.rs`).
    pub gait_phase: f32,
}

/// Web `LEGS` table. `side`: -1 = fly's left, +1 = right. `base`: where the leg joins the body
/// (fraction of 13 along the body). `ang`/`len`: neutral direction and reach of the leg.
/// `phase`: tripod membership (L1, R2, L3 = 0.0; R1, L2, R3 = 0.5).
struct LegSpec {
    side: f32,
    base: f32,
    ang: f32,
    len: f32,
    phase: f32,
}
const LEGS: [LegSpec; 6] = [
    LegSpec { side: -1.0, base: -0.30, ang: -1.05, len: 20.0, phase: 0.0 }, // L1
    LegSpec { side: -1.0, base: 0.02, ang: -1.55, len: 22.0, phase: 0.5 },  // L2
    LegSpec { side: -1.0, base: 0.34, ang: -2.05, len: 24.0, phase: 0.0 },  // L3
    LegSpec { side: 1.0, base: -0.30, ang: 1.05, len: 20.0, phase: 0.5 },   // R1
    LegSpec { side: 1.0, base: 0.02, ang: 1.55, len: 22.0, phase: 0.0 },    // R2
    LegSpec { side: 1.0, base: 0.34, ang: 2.05, len: 24.0, phase: 0.5 },    // R3
];

/// Centre-relative geometry of one leg, in body coordinates (web units, +x = forward,
/// +y = the fly's right): where it joins the body, its neutral foot spot, and where a swinging
/// foot touches down (half a stride ahead of neutral, so the stance carries it half a stride
/// behind).
fn geometry(l: &LegSpec) -> ((f32, f32), (f32, f32), (f32, f32)) {
    let base = (l.base * 13.0, l.side * 4.2);
    let neutral = (base.0 + l.ang.cos() * l.len, base.1 + l.ang.sin() * l.len);
    (base, neutral, (neutral.0 + STRIDE / 2.0, neutral.1))
}

/// Body coordinates (web units) -> screen pixels, and back.
pub fn to_world(p: (f32, f32), pose: &FlyPose, unit: f32) -> (f32, f32) {
    let (s, c) = pose.heading.sin_cos();
    (pose.x + (c * p.0 - s * p.1) * unit, pose.y + (s * p.0 + c * p.1) * unit)
}
fn to_body(w: (f32, f32), pose: &FlyPose, unit: f32) -> (f32, f32) {
    let (s, c) = pose.heading.sin_cos();
    let (dx, dy) = (w.0 - pose.x, w.1 - pose.y);
    ((c * dx + s * dy) / unit, (-s * dx + c * dy) / unit)
}

#[derive(Clone, Copy)]
struct Foot {
    planted: bool,
    /// While planted: the fixed spot on the *screen* the foot is standing on.
    anchor: (f32, f32),
    /// While swinging: where the foot was (body coordinates) when it left the ground.
    from: (f32, f32),
    /// Current position, body coordinates.
    pos: (f32, f32),
}

/// The six feet. This is the one piece of animation state, because "a planted foot stays put
/// on the screen" can't be computed from the current pose alone: it depends on where the foot
/// was set down. So a planted foot simply remembers its screen position, and each frame we ask
/// "where is that spot relative to the body now?". That is exact at any speed, on any curve,
/// at any frame rate, and needs no assumption about how the body got here.
pub struct Feet([Foot; 6]);

impl Feet {
    /// Feet as they'd be if the fly had been walking straight to reach `pose`.
    pub fn new(pose: &FlyPose, scale: f32) -> Feet {
        let unit = BODY_SCALE * scale;
        Feet(LEGS.each_ref().map(|l| {
            let (_, _, touchdown) = geometry(l);
            let ph = (pose.gait_phase + l.phase).rem_euclid(1.0);
            // Stance: foot has slid back `STRIDE * progress` from touchdown. Swing: hovering at
            // the lift-off end, and `update` will carry it forward.
            let back = if ph < SWING { STRIDE } else { STRIDE * (ph - SWING) / STANCE };
            let pos = (touchdown.0 - back, touchdown.1);
            Foot { planted: ph >= SWING, anchor: to_world(pos, pose, unit), from: pos, pos }
        }))
    }

    /// Moves the feet to match `pose`. Call once per pose change, before `draw`.
    pub fn update(&mut self, pose: &FlyPose, scale: f32) {
        let unit = BODY_SCALE * scale;
        for (f, l) in self.0.iter_mut().zip(&LEGS) {
            let (_, _, touchdown) = geometry(l);
            let ph = (pose.gait_phase + l.phase).rem_euclid(1.0);
            if ph < SWING {
                if f.planted {
                    f.planted = false; // lift-off: remember where the foot is
                    f.from = f.pos;
                }
                let u = ph / SWING;
                let e = u * u * (3.0 - 2.0 * u); // smoothstep: leaves and lands gently
                f.pos = (f.from.0 + (touchdown.0 - f.from.0) * e, f.from.1 + (touchdown.1 - f.from.1) * e);
                // From above there is no "up", so a lifted foot is nudged outward instead.
                f.pos.1 += l.side * LIFT * (std::f32::consts::PI * u).sin();
            } else {
                if !f.planted {
                    f.planted = true; // touchdown: pin this spot to the screen
                    f.anchor = to_world(touchdown, pose, unit);
                }
                f.pos = to_body(f.anchor, pose, unit);
            }
        }
    }

    #[cfg(test)]
    pub fn planted(&self, i: usize) -> bool {
        self.0[i].planted
    }

    /// Foot `i`'s position in body coordinates.
    pub fn pos(&self, i: usize) -> (f32, f32) {
        self.0[i].pos
    }
}

fn color(rgb: u32, alpha: f32) -> Color {
    Color::from_rgba8((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8, (alpha * 255.0).round() as u8)
}

fn oval(cx: f32, cy: f32, rx: f32, ry: f32) -> Path {
    // Built around (0,0) rotation-free; callers rotate/translate with a Transform.
    PathBuilder::from_oval(Rect::from_xywh(cx - rx, cy - ry, 2.0 * rx, 2.0 * ry).unwrap()).unwrap()
}

/// Draws `pose` into `pixmap` (cleared first). `scale` = dpi / 96. `origin` is the screen
/// position of the pixmap's top-left pixel, i.e. the window's position: the fly lands at
/// `pose.xy - origin`, so if the window sits at the *floored* position the fractional part
/// of `pose.xy` shifts the drawing inside the pixmap and slow motion doesn't jitter.
pub fn draw(pose: &FlyPose, feet: &Feet, scale: f32, pixmap: &mut Pixmap, origin: (i32, i32)) {
    pixmap.fill(Color::TRANSPARENT);
    let unit = BODY_SCALE * scale;
    // Transforms apply right-to-left to a point: scale web units -> px, rotate to heading,
    // move to the fly's position. (tiny-skia's `pre_*` means "applied before what's there".)
    let body = Transform::from_translate(pose.x - origin.0 as f32, pose.y - origin.1 as f32)
        .pre_rotate(pose.heading.to_degrees()) // tiny-skia rotates in degrees, not radians
        .pre_scale(unit, unit);
    let mut paint = Paint::default();
    paint.anti_alias = true;

    // Legs.
    paint.set_color(color(0x6f7987, 1.0));
    let stroke = Stroke { width: 1.6, line_cap: LineCap::Round, ..Default::default() };
    for (i, l) in LEGS.iter().enumerate() {
        let (base, neutral, _) = geometry(l);
        let foot = feet.pos(i);
        // Knee control point: the web version's, dragged 40% of the way with the foot.
        let knee_a = l.ang - l.side * 0.42;
        let knee = (
            base.0 + knee_a.cos() * l.len * 0.52 + (foot.0 - neutral.0) * 0.4,
            base.1 + knee_a.sin() * l.len * 0.52 + (foot.1 - neutral.1) * 0.4,
        );
        let mut pb = PathBuilder::new();
        pb.move_to(base.0, base.1);
        pb.quad_to(knee.0, knee.1, foot.0, foot.1);
        pixmap.stroke_path(&pb.finish().unwrap(), &paint, &stroke, body, None);
    }

    // Wings (folded back at rest; the web version only spreads them when buzzing).
    let wing_stroke = Stroke { width: 0.7, ..Default::default() };
    for w in [-1.0_f32, 1.0] {
        let t = body
            .pre_rotate((w * 0.34).to_degrees())
            .pre_translate(-13.0, w * 3.4)
            .pre_rotate((w * 0.24).to_degrees());
        let wing = oval(0.0, 0.0, 15.5, 5.2);
        paint.set_color(color(0xbed2e8, 0.17));
        pixmap.fill_path(&wing, &paint, FillRule::Winding, t, None);
        paint.set_color(color(0xbed2e8, 0.28));
        pixmap.stroke_path(&wing, &paint, &wing_stroke, t, None);
    }

    // Abdomen (two layers), thorax, head, then eyes on top.
    for (rgb, cx, rx, ry) in [
        (0x2c2418, -11.5, 12.5, 7.2),
        (0x3a301f, -10.5, 11.0, 6.2),
        (0x4a3c26, 1.0, 9.5, 7.0),
        (0x57462c, 11.0, 6.2, 6.0),
    ] {
        paint.set_color(color(rgb, 1.0));
        pixmap.fill_path(&oval(cx, 0.0, rx, ry), &paint, FillRule::Winding, body, None);
    }
    paint.set_color(color(0x8f2f28, 1.0));
    for (y, tilt) in [(-4.2_f32, -0.3_f32), (4.2, 0.3)] {
        let t = body.pre_translate(12.5, y).pre_rotate(tilt.to_degrees());
        pixmap.fill_path(&oval(0.0, 0.0, 4.1, 3.6), &paint, FillRule::Winding, t, None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Walks a pose along a circle of the given radius (physical px; 1e9 = straight) for a few
    /// laps, calling `each` after every step with the updated feet.
    fn walk(scale: f32, radius: f32, mut each: impl FnMut(&FlyPose, &Feet)) {
        let unit = BODY_SCALE * scale;
        let step = 2.0 * scale; // px per step
        let mut pose = FlyPose { x: 500.0, y: 500.0, heading: 0.0, speed: 1.0, gait_phase: 0.45 };
        let mut feet = Feet::new(&pose, scale);
        for _ in 0..800 {
            pose.heading += step / radius;
            pose.x += pose.heading.cos() * step;
            pose.y += pose.heading.sin() * step;
            pose.gait_phase = (pose.gait_phase + step / (unit * CYCLE)).rem_euclid(1.0);
            feet.update(&pose, scale);
            each(&pose, &feet);
        }
    }

    /// The window is sized from RADIUS, so nothing may ever be drawn on the pixmap's border
    /// pixels: try straight and tight left/right curves at 100%, 150% and 200% scaling, with
    /// the worst-case fractional position.
    #[test]
    fn fits_in_window() {
        for scale in [1.0, 1.5, 2.0] {
            let size = window_size(scale);
            let n = size as usize;
            let mut pm = Pixmap::new(size as u32, size as u32).unwrap();
            for radius in [1e9, 60.0 * scale, -60.0 * scale] {
                walk(scale, radius, |pose, feet| {
                    let mut p = *pose;
                    (p.x, p.y) = (p.x.floor() + 0.99, p.y.floor() + 0.99);
                    let origin = (p.x.floor() as i32 - size / 2, p.y.floor() as i32 - size / 2);
                    draw(&p, feet, scale, &mut pm, origin);
                    let edge = |i: usize| pm.data()[i * 4 + 3] != 0;
                    for i in 0..n {
                        assert!(!edge(i) && !edge((n - 1) * n + i), "top/bottom edge hit");
                        assert!(!edge(i * n) && !edge(i * n + n - 1), "left/right edge hit");
                    }
                });
            }
        }
    }

    /// The whole point of the anchoring: a planted foot doesn't move on the screen, on
    /// straight or curved paths. And feet never teleport.
    #[test]
    fn planted_feet_stay_put() {
        for radius in [1e9, 60.0, -60.0] {
            let mut prev: Option<(Vec<bool>, Vec<(f32, f32)>, Vec<(f32, f32)>)> = None;
            walk(1.0, radius, |pose, feet| {
                let planted: Vec<_> = (0..6).map(|i| feet.planted(i)).collect();
                let pos: Vec<_> = (0..6).map(|i| feet.pos(i)).collect();
                let world: Vec<_> = pos.iter().map(|&p| to_world(p, pose, BODY_SCALE)).collect();
                if let Some((pp, ppos, pw)) = &prev {
                    for i in 0..6 {
                        if pp[i] && planted[i] {
                            let d = (pw[i].0 - world[i].0).hypot(pw[i].1 - world[i].1);
                            assert!(d < 0.01, "foot {i} slid {d} px");
                        }
                        let j = (ppos[i].0 - pos[i].0).hypot(ppos[i].1 - pos[i].1);
                        assert!(j < 6.0, "foot {i} jumped {j} units");
                    }
                }
                prev = Some((planted, pos, world));
            });
        }
    }
}
