//! The fly's pose and gait: where the body is, and where each of the six feet stands.
//! Drawing lives in `art.rs`. No Win32 in either, so the brain (milestone 3) can drive
//! `FlyPose` without touching rendering code.
//!
//! Body space (same as the reference drawing): the fly faces +x, y points down (so +y is the
//! fly's right), 1 unit = 1 px at 100% display scale *before* `FLY_SCALE`.

/// Reference `FLY_SCALE`: ~60 px across the legs at 100% display scale.
pub const FLY_SCALE: f32 = 1.3;

// --- Gait ---------------------------------------------------------------------------------
// One gait cycle per leg = STANCE (foot on the ground, moving backward relative to the body)
// followed by SWING (foot in the air, moving forward). Tripod gait: the two tripods run half a
// cycle apart, so three feet are always down.
const STANCE: f32 = 0.6;
const SWING: f32 = 1.0 - STANCE;
/// How far (body units) a planted foot travels backward relative to the body per step.
/// The largest round value at which no legs cross at ANY gait phase and every foot stays in
/// reach (`legs_never_cross_and_never_stretch` in art.rs, `feet_stay_within_reach`).
const STRIDE: f32 = 3.0;
/// Peak lift of a swinging foot, drawn as a sideways offset (we look from above).
const LIFT: f32 = 1.2;
/// Distance the body walks per full gait cycle. A foot stays put on the ground for the whole
/// stance (60% of the cycle), during which it moves back `STRIDE` relative to the body, so the
/// body must travel exactly `STRIDE` in 60% of the cycle => STRIDE / 0.6.
pub const CYCLE: f32 = STRIDE / STANCE;

/// Farthest any painted pixel can be from the fly's centre, in body units, at any heading and
/// gait phase: measured 26.0 (hind legs at full stretch, round feet and outline), plus a margin.
/// Sizes the window; `fits_in_window` proves it by rendering.
const RADIUS: f32 = 27.5;

/// Screen pixels per art pixel: whole pixels only, so 1 at 100-175%, 2 at 200-275%.
pub fn art_px(scale: f32) -> u32 {
    ((scale + 1e-6).floor() as u32).max(1)
}

/// Window edge length in physical pixels for a given DPI scale (dpi / 96): a whole, even number
/// of art pixels (so the fly's centre sits on an art-grid line), plus one art pixel of margin
/// each side.
pub fn window_size(scale: f32) -> i32 {
    let ap = art_px(scale);
    let art = ((2.0 * RADIUS * FLY_SCALE * scale / ap as f32).ceil() as u32 + 2 + 1) & !1;
    (art * ap) as i32
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

/// Reference `LEGS`: attach (x, |y|), rest foot (x, |y|), femur, tibia. Front, middle, hind.
pub const LEGS: [((f32, f32), (f32, f32), f32, f32); 3] = [
    ((2.8, 6.6), (9.8, 16.2), 7.2, 7.6),
    ((-0.8, 7.2), (-5.4, 19.2), 6.6, 7.4),
    ((-7.6, 3.6), (-20.2, 13.4), 8.4, 9.6),
];

/// Foot index `k` (0..6): 0..3 = left side (y < 0) front..back, 3..6 = right side.
pub fn side_of(k: usize) -> f32 {
    if k < 3 { -1.0 } else { 1.0 }
}

/// Static geometry of one leg, in body coordinates.
pub struct LegGeo {
    /// Where the leg joins the body.
    pub attach: (f32, f32),
    /// Femur + tibia: the farthest a foot can be from `attach`.
    pub full: f32,
    /// Where a swinging foot touches down (half a stride ahead of the neutral spot, so the
    /// stance carries it half a stride behind).
    pub touchdown: (f32, f32),
    /// Tripod membership: legs of one tripod share a phase, the other tripod is half a cycle
    /// away. Same alternation as the reference (front and back of one side + middle of the other).
    pub phase: f32,
}

pub fn leg_geo(k: usize) -> LegGeo {
    let (side, i) = (side_of(k), k % 3);
    let ((ax, ay), (fx, fy), f, t) = LEGS[i];
    let attach = (ax, ay * side);
    let full = f + t;
    // Neutral foot spot: the reference's rest foot. A stride around it can overshoot full reach
    // (legs here have FIXED segment lengths), so slide the stride window along x, only as far as
    // needed, until both ends of the stride are within 98% of full reach.
    let neutral = (fx, fy * side);
    let (dx, dy) = (fx - ax, fy - ay);
    let h = STRIDE / 2.0;
    let root = ((0.98 * full).powi(2) - dy * dy).max(0.0).sqrt(); // max |x| from the hip
    let (lo, hi) = (dx - h, dx + h);
    let shift = if hi > root { root - hi } else if lo < -root { -root - lo } else { 0.0 };
    let phase = if (i % 2 == 0) == (side > 0.0) { 0.0 } else { 0.5 };
    LegGeo { attach, full, touchdown: (neutral.0 + shift + h, neutral.1), phase }
}

/// Body coordinates (body units) -> screen pixels, and back. `unit` = px per body unit.
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
        let unit = FLY_SCALE * scale;
        let mut feet = [Foot { planted: true, anchor: (0.0, 0.0), from: (0.0, 0.0), pos: (0.0, 0.0) }; 6];
        for (k, f) in feet.iter_mut().enumerate() {
            let g = leg_geo(k);
            let ph = (pose.gait_phase + g.phase).rem_euclid(1.0);
            // Stance: foot has slid back `STRIDE * progress` from touchdown. Swing: hovering at
            // the lift-off end, and `update` will carry it forward.
            let back = if ph < SWING { STRIDE } else { STRIDE * (ph - SWING) / STANCE };
            let pos = (g.touchdown.0 - back, g.touchdown.1);
            *f = Foot { planted: ph >= SWING, anchor: to_world(pos, pose, unit), from: pos, pos };
        }
        Feet(feet)
    }

    /// Moves the feet to match `pose`. Call once per pose change, before `draw`.
    pub fn update(&mut self, pose: &FlyPose, scale: f32) {
        let unit = FLY_SCALE * scale;
        for (k, f) in self.0.iter_mut().enumerate() {
            let g = leg_geo(k);
            let ph = (pose.gait_phase + g.phase).rem_euclid(1.0);
            if ph < SWING {
                if f.planted {
                    f.planted = false; // lift-off: remember where the foot is
                    f.from = f.pos;
                }
                let u = ph / SWING;
                let e = u * u * (3.0 - 2.0 * u); // smoothstep: leaves and lands gently
                f.pos = (
                    f.from.0 + (g.touchdown.0 - f.from.0) * e,
                    f.from.1 + (g.touchdown.1 - f.from.1) * e,
                );
                // From above there is no "up", so a lifted foot is nudged outward instead.
                f.pos.1 += side_of(k) * LIFT * (std::f32::consts::PI * u).sin();
            } else {
                if !f.planted {
                    f.planted = true; // touchdown: pin this spot to the screen
                    f.anchor = to_world(g.touchdown, pose, unit);
                }
                f.pos = to_body(f.anchor, pose, unit);
            }
        }
    }

    #[cfg(test)]
    pub fn planted(&self, k: usize) -> bool {
        self.0[k].planted
    }

    /// Foot `k`'s position in body coordinates.
    pub fn pos(&self, k: usize) -> (f32, f32) {
        self.0[k].pos
    }
}

/// Test helper: walks a pose along a circle of the given radius (physical px; 1e9 = straight)
/// for `steps` steps, calling `each` after every step with the updated feet.
#[cfg(test)]
pub fn walk(scale: f32, radius: f32, steps: usize, mut each: impl FnMut(&FlyPose, &Feet)) {
    let unit = FLY_SCALE * scale;
    let step = 2.0 * scale; // px per step
    let mut pose = FlyPose { x: 500.0, y: 500.0, heading: 0.0, speed: 1.0, gait_phase: 0.45 };
    let mut feet = Feet::new(&pose, scale);
    for _ in 0..steps {
        pose.heading += step / radius;
        pose.x += pose.heading.cos() * step;
        pose.y += pose.heading.sin() * step;
        pose.gait_phase = (pose.gait_phase + step / (unit * CYCLE)).rem_euclid(1.0);
        feet.update(&pose, scale);
        each(&pose, &feet);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole point of the anchoring: a planted foot doesn't move on the screen, on
    /// straight or curved paths. And feet never teleport.
    #[test]
    fn planted_feet_stay_put() {
        for radius in [1e9, 60.0, -60.0] {
            let mut prev: Option<(Vec<bool>, Vec<(f32, f32)>, Vec<(f32, f32)>)> = None;
            walk(1.0, radius, 800, |pose, feet| {
                let planted: Vec<_> = (0..6).map(|i| feet.planted(i)).collect();
                let pos: Vec<_> = (0..6).map(|i| feet.pos(i)).collect();
                let world: Vec<_> = pos.iter().map(|&p| to_world(p, pose, FLY_SCALE)).collect();
                if let Some((pp, ppos, pw)) = &prev {
                    for i in 0..6 {
                        if pp[i] && planted[i] {
                            let d = (pw[i].0 - world[i].0).hypot(pw[i].1 - world[i].1);
                            assert!(d < 0.01, "foot {i} slid {d} px");
                        }
                        let j = (ppos[i].0 - pos[i].0).hypot(ppos[i].1 - pos[i].1);
                        // a swinging foot outruns the body by ~2.3x at mid-swing; walk() steps 2 px
                        let limit = 3.0 * 2.0 / FLY_SCALE;
                        assert!(j < limit, "foot {i} jumped {j} units (limit {limit})");
                    }
                }
                prev = Some((planted, pos, world));
            });
        }
    }

    /// Fixed segment lengths need every foot within reach of its hip. On straight walking and on
    /// the route's tightest turns (radius ~133 px) no foot may ask for more than full reach, and
    /// the drawing's 99.5% safety clamp may only ever nudge a foot by a hair (< 0.3 units).
    #[test]
    fn feet_stay_within_reach() {
        let mut worst_clamp = 0.0_f32;
        for radius in [1e9, 133.0, -133.0] {
            walk(1.0, radius, 800, |_, feet| {
                for k in 0..6 {
                    let g = leg_geo(k);
                    let p = feet.pos(k);
                    let d = (p.0 - g.attach.0).hypot(p.1 - g.attach.1);
                    assert!(d <= g.full, "foot {k} at {d} > full reach {}", g.full);
                    worst_clamp = worst_clamp.max(d - 0.995 * g.full);
                }
            });
        }
        println!("worst clamp nudge: {worst_clamp} body units");
        assert!(worst_clamp < 0.3, "clamp moves feet by {worst_clamp}");
    }
}
