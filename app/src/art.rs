//! The fly's drawing: a port of `reference/fly_pixel.py` (pycairo) to tiny-skia. Palette, shapes,
//! draw order and leg IK are the reference's.
//!
//! How a frame is made (the reference's docstring has the why):
//! 1. `render_art` rasterises the fly into a small "art" pixmap, 1 pixel = 1 art pixel, with
//!    anti-aliasing OFF on every fill, stroke and mask, in flat colours (no gradients).
//! 2. `upscale` copies each art pixel into an `art_px x art_px` block of the window pixmap.
//!    The fly keeps its size at every display scale; only the chunkiness changes.
//!
//! The fly rotates to any angle: the shapes are re-rasterised onto the fixed, screen-aligned art
//! grid every frame, so pixels rearrange and never blur. The fly's centre always sits on the same
//! art-grid point (the window moves in whole art pixels, see `window_origin`); a moving
//! fractional offset would make non-AA edges flicker while walking straight.
//!
//! cairo -> tiny-skia, the parts that differ:
//! * cairo's `clip` becomes a `Mask` built from the part's own path (non-AA too).
//! * The wing is built under a translate/rotate in the reference; here that transform is applied
//!   to the path itself, and the one transformed path is used for outline, fill and clip.
//! * A stroke under 1 px wide is a hairline in tiny-skia, and tiny-skia fades a thin line's
//!   alpha by its width. So anything <= 1 art px is drawn as a plain 1-px hairline.

use crate::fly::{FLY_SCALE, Feet, FlyPose, LEGS, art_px, leg_geo};
use tiny_skia::{
    Color, FillRule, LineCap, LineJoin, Mask, Paint, Path, PathBuilder, Pixmap, Rect, Stroke, Transform,
};

type Rgb = (f32, f32, f32);

// --- Palette (exact reference RGB; there is no colour conversion anywhere) ------------------
const OUTLINE: Rgb = (0.27, 0.08, 0.17); // dark maroon, never pure black
const GREEN: [Rgb; 3] = [(0.33, 0.49, 0.32), (0.47, 0.65, 0.40), (0.62, 0.77, 0.49)]; // dark, mid, light
const YELLOW: [Rgb; 3] = [(0.80, 0.72, 0.30), (0.95, 0.89, 0.50), (1.00, 0.97, 0.74)];
const GREY: [Rgb; 3] = [(0.24, 0.25, 0.27), (0.36, 0.38, 0.40), (0.50, 0.52, 0.54)];
const WING: [Rgb; 3] = [(0.78, 0.79, 0.84), (0.88, 0.89, 0.92), (0.96, 0.96, 0.98)];
const VEIN: Rgb = (0.66, 0.67, 0.73);
pub const EYE: [Rgb; 3] = [(0.50, 0.04, 0.13), (0.78, 0.10, 0.19), (0.94, 0.34, 0.37)];
const WHITE: Rgb = (1.0, 1.0, 1.0);
/// Soft pinkish grey, flat: the one colour that isn't opaque.
const SHADOW: (Rgb, f32) = ((0.45, 0.30, 0.38), 0.28);

/// The light, in SCREEN space (top-left).
const LIGHT: (f32, f32) = (-0.55, -0.83);

/// Rotates the screen light into body space, so highlights stay top-left on the screen as the
/// fly turns.
fn light_in_body(heading: f32) -> (f32, f32) {
    let (s, c) = (-heading).sin_cos();
    (LIGHT.0 * c - LIGHT.1 * s, LIGHT.0 * s + LIGHT.1 * c)
}

fn ellipse(cx: f32, cy: f32, rx: f32, ry: f32) -> Path {
    PathBuilder::from_oval(Rect::from_xywh(cx - rx, cy - ry, 2.0 * rx, 2.0 * ry).unwrap()).unwrap()
}

fn circle(x: f32, y: f32, r: f32) -> Path {
    PathBuilder::from_circle(x, y, r).unwrap()
}

fn polyline(pts: &[(f32, f32)]) -> Path {
    let mut pb = PathBuilder::new();
    pb.move_to(pts[0].0, pts[0].1);
    pts[1..].iter().for_each(|p| pb.line_to(p.0, p.1));
    pb.finish().unwrap()
}

/// Drawing in body units on the art pixmap. Every paint goes through `fill` / `stroke` /
/// `clip_to`, which is where anti-aliasing is switched off.
struct Ctx<'a> {
    pm: &'a mut Pixmap,
    /// The current clip: the last `blob`'s shape.
    mask: Mask,
    body: Transform,
    /// Light direction, body space.
    l: (f32, f32),
    /// Art pixels per body unit.
    k: f32,
}

impl Ctx<'_> {
    /// `n` art pixels, in body units.
    fn ap(&self, n: f32) -> f32 {
        n / self.k
    }

    fn paint(c: Rgb) -> Paint<'static> {
        let mut p = Paint::default();
        p.set_color(Color::from_rgba(c.0, c.1, c.2, 1.0).unwrap());
        p.anti_alias = false;
        p
    }

    fn fill_at(&mut self, path: &Path, c: Rgb, ts: Transform, clipped: bool) {
        self.pm.fill_path(path, &Self::paint(c), FillRule::Winding, ts, clipped.then_some(&self.mask));
    }

    fn fill(&mut self, path: &Path, c: Rgb, clipped: bool) {
        self.fill_at(path, c, self.body, clipped);
    }

    /// `width` in art pixels. Round caps and joins for lines; cairo's defaults (butt, miter 10)
    /// for outlines.
    fn stroke(&mut self, path: &Path, c: Rgb, width: f32, round: bool, clipped: bool) {
        let st = Stroke {
            width: if width <= 1.0 { 0.0 } else { self.ap(width) }, // hairline: see module doc
            line_cap: if round { LineCap::Round } else { LineCap::Butt },
            line_join: if round { LineJoin::Round } else { LineJoin::Miter },
            miter_limit: 10.0,
            ..Default::default()
        };
        self.pm.stroke_path(path, &Self::paint(c), &st, self.body, clipped.then_some(&self.mask));
    }

    fn clip_to(&mut self, path: &Path) {
        self.mask.clear();
        self.mask.fill_path(path, FillRule::Winding, false, self.body);
    }

    /// A flat-shaded pixel blob: 1-art-px outline (a 2-px stroke whose inner half the fill then
    /// covers), dark base, mid tone = the same shape nudged toward the light and clipped by the
    /// original. Leaves the clip set to `path`, for the caller's highlights.
    fn blob(&mut self, path: &Path, [dark, mid, _]: [Rgb; 3]) {
        self.stroke(path, OUTLINE, 2.0, false, false);
        self.clip_to(path);
        self.fill(path, dark, true);
        let nudge = (self.l.0 * self.ap(1.5), self.l.1 * self.ap(1.5));
        self.fill_at(path, mid, self.body.pre_translate(nudge.0, nudge.1), true);
    }

    /// `blob` for an ellipse, with an optional flat highlight toward the light.
    fn shaded_ellipse(&mut self, (cx, cy): (f32, f32), (rx, ry): (f32, f32), ramp: [Rgb; 3], highlight: Option<(Rgb, f32)>) {
        self.blob(&ellipse(cx, cy, rx, ry), ramp);
        if let Some((c, size)) = highlight {
            let (hx, hy) = (cx + self.l.0 * rx * 0.45, cy + self.l.1 * ry * 0.45);
            self.fill(&ellipse(hx, hy, rx * size, ry * size), c, true);
        }
    }

    /// A polyline `width` art px wide with a 1-art-px outline.
    fn leg_line(&mut self, pts: &[(f32, f32)], c: Rgb, width: f32) {
        let path = polyline(pts);
        self.stroke(&path, OUTLINE, width + 2.0, true, false);
        self.stroke(&path, c, width, true, false);
    }

    /// A round dot: outline disc, then a smaller coloured one. Radii in art pixels.
    fn dot(&mut self, (x, y): (f32, f32), outer: f32, inner: f32, c: Rgb) {
        self.fill(&circle(x, y, self.ap(outer)), OUTLINE, false);
        self.fill(&circle(x, y, self.ap(inner)), c, false);
    }
}

/// Knee for a leg of fixed segment lengths; returns both solutions.
fn two_bone(a: (f32, f32), f: (f32, f32), l1: f32, l2: f32) -> ((f32, f32), (f32, f32)) {
    let (dx, dy) = (f.0 - a.0, f.1 - a.1);
    let d = dx.hypot(dy).min(l1 + l2 - 1e-3);
    let along = (l1 * l1 - l2 * l2 + d * d) / (2.0 * d);
    let h = (l1 * l1 - along * along).max(0.0).sqrt();
    let (ux, uy) = (dx / d.max(1e-6), dy / d.max(1e-6));
    let (mx, my) = (a.0 + ux * along, a.1 + uy * along);
    ((mx - uy * h, my + ux * h), (mx + uy * h, my - ux * h))
}

/// One leg's joints in body coordinates.
struct Joints {
    attach: (f32, f32),
    knee: (f32, f32),
    foot: (f32, f32),
}

/// Leg `k`'s joints: the foot comes from the gait (`Feet`), the knee from the reference's
/// two-bone IK with FIXED femur and tibia lengths, choosing the knee farther from the body's
/// midline.
fn joints(k: usize, feet: &Feet) -> Joints {
    let (_, _, f, t) = LEGS[k % 3];
    let g = leg_geo(k);
    // A foot within a hair of full reach (turns push planted feet a little outward, see
    // `feet_stay_within_reach`) is pulled back onto the reach circle, so lengths never change.
    let mut foot = feet.pos(k);
    let (dx, dy) = (foot.0 - g.attach.0, foot.1 - g.attach.1);
    let d = dx.hypot(dy);
    if d > 0.995 * g.full {
        let s = 0.995 * g.full / d;
        foot = (g.attach.0 + dx * s, g.attach.1 + dy * s);
    }
    let (k1, k2) = two_bone(g.attach, foot, f, t);
    let knee = if k1.1.abs() > k2.1.abs() { k1 } else { k2 };
    Joints { attach: g.attach, knee, foot }
}

fn draw_legs(c: &mut Ctx, feet: &Feet) {
    for k in 0..6 {
        let Joints { attach, knee, foot } = joints(k, feet);
        c.leg_line(&[attach, knee, foot], GREY[1], 1.2);
        c.dot(foot, 1.6, 1.0, GREY[1]);
    }
}

/// Wing space -> body space: root at (-2.6, 2.4*side), pointing back in a V, mirrored per side.
fn wing_ts(side: f32) -> Transform {
    Transform::from_translate(-2.6, 2.4 * side).pre_rotate(180.0 - 33.0 * side).pre_scale(1.0, side)
}

fn draw_wing(c: &mut Ctx, side: f32) {
    // a broad, slightly squared paddle
    let mut pb = PathBuilder::new();
    pb.move_to(0.0, -1.2);
    pb.line_to(12.5, -3.4);
    pb.cubic_to(15.2, -3.6, 15.6, 3.6, 12.8, 3.8);
    pb.line_to(0.6, 3.0);
    pb.close();
    let wing = pb.finish().unwrap().transform(wing_ts(side)).unwrap();
    c.blob(&wing, WING);
    // two veins, 1 art px, clipped inside the wing
    let mut pb = PathBuilder::new();
    for (y0, y1) in [(-0.4, -1.4), (1.2, 1.6)] {
        pb.move_to(1.5, y0);
        pb.line_to(13.5, y1);
    }
    let veins = pb.finish().unwrap().transform(wing_ts(side)).unwrap();
    c.stroke(&veins, VEIN, 1.0, false, true);
}

fn draw_fly(c: &mut Ctx, feet: &Feet) {
    draw_legs(c, feet);

    // abdomen: a rounded shield pointing backward, two yellow stripes, a glint on the lit side
    let mut pb = PathBuilder::new();
    pb.move_to(-9.6, -8.2);
    pb.cubic_to(-9.0, -3.0, -9.0, 3.0, -9.6, 8.2);
    pb.cubic_to(-14.0, 8.0, -19.5, 3.5, -21.4, 0.0);
    pb.cubic_to(-19.5, -3.5, -14.0, -8.0, -9.6, -8.2);
    pb.close();
    c.blob(&pb.finish().unwrap(), GREEN);
    for (x0, w) in [(-12.6, 1.8), (-16.0, 1.6)] {
        c.fill(&PathBuilder::from_rect(Rect::from_xywh(x0 - w / 2.0, -9.0, w, 18.0).unwrap()), YELLOW[1], true);
    }
    c.fill(&circle(-13.5 + c.l.0 * 3.5, c.l.1 * 4.0, 1.4), GREEN[2], true);

    for side in [-1.0, 1.0] {
        draw_wing(c, side);
    }

    c.shaded_ellipse((-7.6, 0.0), (2.2, 2.4), GREEN, None); // waist
    c.shaded_ellipse((0.0, 0.0), (5.6, 7.6), GREEN, Some((YELLOW[1], 0.5))); // thorax

    // the grey moustache tuft between thorax and head
    let tuft = [(3.4, -2.2), (4.4, -1.2), (2.6, -0.4), (3.8, 0.4), (2.4, 1.2), (4.2, 1.8), (3.2, 2.6), (5.6, 2.8)];
    let mut pb = PathBuilder::new();
    pb.move_to(5.6, -2.8);
    tuft.iter().for_each(|p| pb.line_to(p.0, p.1));
    pb.close();
    c.blob(&pb.finish().unwrap(), GREY);

    // head, with a yellow cap at the front
    c.blob(&ellipse(10.6, 0.0, 5.4, 3.6), GREEN);
    c.fill(&circle(14.2, 0.0, 2.2), YELLOW[1], true);

    // antennae with round tips
    for s in [-1.0, 1.0] {
        c.leg_line(&[(15.0, 1.2 * s), (18.4, 2.6 * s), (20.6, 4.4 * s)], GREEN[1], 0.6);
        c.dot((20.8, 4.6 * s), 1.7, 1.0, YELLOW[1]);
    }

    // the eyes: huge, on either side of the head, two white glints each
    let l = c.l;
    for s in [-1.0, 1.0] {
        let (ex, ey) = (11.0, 5.9 * s);
        c.blob(&ellipse(ex, ey, 4.4, 4.4), EYE);
        c.fill(&circle(ex + l.0 * 1.4, ey + l.1 * 1.4, 2.6), EYE[2], true);
        c.fill(&circle(ex + l.0 * 1.9, ey + l.1 * 1.9, 1.35), WHITE, true);
        c.fill(&circle(ex - l.0 * 0.4 + l.1 * 1.6, ey - l.1 * 0.4 - l.0 * 1.6, 0.7), WHITE, true);
    }
}

/// Rasterises the fly (and its shadow) into `art`, 1 px = 1 art px, centred. `scale` = dpi / 96.
pub fn render_art(heading: f32, feet: &Feet, scale: f32, art: &mut Pixmap) {
    let k = FLY_SCALE * scale / art_px(scale) as f32;
    let (cx, cy) = (art.width() as f32 / 2.0, art.height() as f32 / 2.0);
    let mask = Mask::new(art.width(), art.height()).unwrap();
    // shadow: screen space (never rotated), flat, offset down-right
    let shadow_ts = Transform::from_translate(cx + 3.0 * k, cy + 6.0 * k).pre_scale(17.0 * k, 8.0 * k);
    let mut paint = Ctx::paint(SHADOW.0);
    paint.set_color(Color::from_rgba(SHADOW.0.0, SHADOW.0.1, SHADOW.0.2, SHADOW.1).unwrap());
    art.fill_path(&circle(0.0, 0.0, 1.0), &paint, FillRule::Winding, shadow_ts, None);

    let body = Transform::from_translate(cx, cy).pre_rotate(heading.to_degrees()).pre_scale(k, k);
    draw_fly(&mut Ctx { pm: art, mask, body, l: light_in_body(heading), k }, feet);
}

/// Nearest-neighbour upscale: each `src` pixel becomes an `f x f` block of `dst`.
pub fn upscale(src: &Pixmap, f: u32, dst: &mut Pixmap) {
    assert_eq!((src.width() * f, src.height() * f), (dst.width(), dst.height()));
    let (f, sw, dw) = (f as usize, src.width() as usize, dst.width() as usize);
    let (s, d) = (src.data(), dst.data_mut());
    for y in 0..d.len() / 4 / dw {
        for x in 0..dw {
            let (i, j) = (((y / f) * sw + x / f) * 4, (y * dw + x) * 4);
            d[j..j + 4].copy_from_slice(&s[i..i + 4]);
        }
    }
}

/// Screen position of the window's top-left for a `size`-px window: the fly's centre snapped to
/// whole art pixels. The simulation position stays a float; only drawing snaps.
pub fn window_origin(pose: &FlyPose, scale: f32, size: i32) -> (i32, i32) {
    let ap = art_px(scale) as f32;
    let snap = |v: f32| (v / ap).round() as i32 * ap as i32;
    (snap(pose.x) - size / 2, snap(pose.y) - size / 2)
}

/// Draws the fly centred on `out` (a window pixmap, a whole number of art pixels wide), at
/// `pose`'s heading. Position is the window's job (`window_origin`).
pub fn draw(pose: &FlyPose, feet: &Feet, scale: f32, out: &mut Pixmap) {
    let ap = art_px(scale);
    let mut art = Pixmap::new(out.width() / ap, out.height() / ap).unwrap();
    render_art(pose.heading, feet, scale, &mut art);
    upscale(&art, ap, out);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fly::{to_world, walk, window_size};

    /// The window is sized from `fly::RADIUS`, so nothing may ever be drawn on the pixmap's
    /// border pixels: straight and turning walks at 100%, 125%, 150% and 200%.
    #[test]
    fn fits_in_window() {
        for scale in [1.0, 1.25, 1.5, 2.0] {
            let size = window_size(scale);
            let n = size as usize;
            let mut pm = Pixmap::new(size as u32, size as u32).unwrap();
            let mut widest = 0.0_f32;
            for radius in [1e9, 60.0 * scale, -60.0 * scale] {
                walk(scale, radius, 240, |pose, feet| {
                    pm.fill(Color::TRANSPARENT);
                    draw(pose, feet, scale, &mut pm);
                    let a = |i: usize| pm.data()[i * 4 + 3] != 0;
                    for i in 0..n {
                        assert!(!a(i) && !a((n - 1) * n + i), "top/bottom edge hit at {scale}");
                        assert!(!a(i * n) && !a(i * n + n - 1), "left/right edge hit at {scale}");
                    }
                    // farthest painted pixel from the fly, in body units (for tuning RADIUS)
                    let c = n as f32 / 2.0;
                    for (i, px) in pm.data().chunks_exact(4).enumerate() {
                        if px[3] != 0 {
                            let r = ((i % n) as f32 + 0.5 - c).hypot((i / n) as f32 + 0.5 - c);
                            widest = widest.max(r / (FLY_SCALE * scale));
                        }
                    }
                });
            }
            println!("scale {scale}: window {size} px, widest painted pixel {widest:.1} body units");
        }
    }

    /// No soft edges: a single anti-aliased paint shows as a grey fringe on a dark wallpaper.
    /// Every pixel is fully transparent or opaque, except the shadow's one flat alpha.
    #[test]
    fn pixels_are_all_or_nothing() {
        let mut other = std::collections::BTreeSet::new();
        for scale in [1.0, 1.25, 2.0] {
            let size = window_size(scale) as u32;
            let mut pm = Pixmap::new(size, size).unwrap();
            for i in 0..60 {
                let pose = FlyPose { x: 0.0, y: 0.0, heading: (i as f32 * 7.3).to_radians(), speed: 0.0, gait_phase: i as f32 / 60.0 };
                pm.fill(Color::TRANSPARENT);
                draw(&pose, &Feet::new(&pose, scale), scale, &mut pm);
                other.extend(pm.data().chunks_exact(4).map(|p| p[3]).filter(|&a| a != 0 && a != 255));
            }
        }
        assert!(other.len() == 1, "alpha values other than 0 and 255: {other:?}");
        let shadow = *other.first().unwrap() as f32;
        assert!((shadow - SHADOW.1 * 255.0).abs() <= 1.0, "shadow alpha {shadow}");
    }

    /// The light is fixed on the screen: the thorax's yellow highlight and both eyes' big white
    /// glints sit top-left of their part on screen, whichever way the fly faces.
    #[test]
    fn light_stays_top_left_on_screen() {
        let scale = 1.75; // 1 art px = 1 screen px, and the most art pixels per body unit
        let size = window_size(scale) as u32;
        let unit = FLY_SCALE * scale;
        let rgb = |c: Rgb| {
            let c = Color::from_rgba(c.0, c.1, c.2, 1.0).unwrap().to_color_u8();
            [c.red(), c.green(), c.blue(), 255]
        };
        for deg in [0.0_f32, 90.0, 180.0, 270.0] {
            let pose = FlyPose { x: size as f32 / 2.0, y: size as f32 / 2.0, heading: deg.to_radians(), speed: 0.0, gait_phase: 0.45 };
            let mut pm = Pixmap::new(size, size).unwrap();
            draw(&pose, &Feet::new(&pose, scale), scale, &mut pm);
            // Centroid of `colour` pixels within `r` body units of body point `at`, relative to
            // that point, in screen px.
            let centroid = |colour: Rgb, at: (f32, f32), r: f32| {
                let p = to_world(at, &pose, unit);
                let (mut sx, mut sy, mut n) = (0.0, 0.0, 0);
                for (i, px) in pm.data().chunks_exact(4).enumerate() {
                    let (dx, dy) = ((i % size as usize) as f32 + 0.5 - p.0, (i / size as usize) as f32 + 0.5 - p.1);
                    if px == rgb(colour) && dx.hypot(dy) < r * unit {
                        (sx, sy, n) = (sx + dx, sy + dy, n + 1);
                    }
                }
                assert!(n > 0, "no pixels of {colour:?} near {at:?} at {deg} deg");
                (sx / n as f32, sy / n as f32)
            };
            let (x, y) = centroid(YELLOW[1], (0.0, 0.0), 6.0);
            assert!(x < 0.0 && y < 0.0, "thorax highlight at {deg} deg: ({x:.1}, {y:.1})");
            for s in [-1.0, 1.0] {
                let (x, y) = centroid(WHITE, (11.0, 5.9 * s), 4.4);
                assert!(x < 0.0 && y < 0.0, "eye glint at {deg} deg: ({x:.1}, {y:.1})");
            }
        }
    }

    fn ccw(a: (f32, f32), b: (f32, f32), c: (f32, f32)) -> f32 {
        (b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0)
    }
    /// Proper crossing of two segments (touching at an end doesn't count).
    fn cross(p: [(f32, f32); 2], q: [(f32, f32); 2]) -> bool {
        ccw(p[0], p[1], q[0]) * ccw(p[0], p[1], q[1]) < 0.0 && ccw(q[0], q[1], p[0]) * ccw(q[0], q[1], p[1]) < 0.0
    }

    /// Do any two legs cross? Returns the first crossing pair.
    fn crossing(feet: &Feet) -> Option<(usize, usize)> {
        let j: Vec<Joints> = (0..6).map(|k| joints(k, feet)).collect();
        let segs = |q: &Joints| [[q.attach, q.knee], [q.knee, q.foot]];
        for a in 0..6 {
            for b in a + 1..6 {
                if segs(&j[a]).iter().any(|sa| segs(&j[b]).iter().any(|sb| cross(*sa, *sb))) {
                    return Some((a, b));
                }
            }
        }
        None
    }

    /// Acceptance: legs never cross and segment lengths never change, at every gait phase and on
    /// straight and curved walks.
    #[test]
    fn legs_never_cross_and_never_stretch() {
        let check = |feet: &Feet, what: &str| {
            assert_eq!(crossing(feet), None, "{what}: legs cross");
            for k in 0..6 {
                let (_, _, f, t) = LEGS[k % 3];
                let j = joints(k, feet);
                let len = |a: (f32, f32), b: (f32, f32)| (a.0 - b.0).hypot(a.1 - b.1);
                assert!((len(j.attach, j.knee) - f).abs() < 2e-3, "{what}: femur {k} is {}", len(j.attach, j.knee));
                assert!((len(j.knee, j.foot) - t).abs() < 2e-3, "{what}: tibia {k} is {}", len(j.knee, j.foot));
            }
        };
        for i in 0..200 {
            let pose = FlyPose { x: 0.0, y: 0.0, heading: 0.0, speed: 0.0, gait_phase: i as f32 / 200.0 };
            check(&Feet::new(&pose, 1.0), "phase sweep");
        }
        for radius in [1e9, 133.0, -133.0] {
            walk(1.0, radius, 400, |_, feet| check(feet, "walk"));
        }
    }
}

#[cfg(test)]
mod bench {
    use super::*;
    use crate::fly::window_size;
    /// Cost of one frame's drawing (clear, rasterise, upscale) over one minute of the demo route
    /// at 60 Hz. Run: `cargo test --release draw_cost -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn draw_cost() {
        for scale in [1.0_f32, 1.25, 2.0] {
            let size = window_size(scale);
            let mut pm = Pixmap::new(size as u32, size as u32).unwrap();
            let mut walker = crate::path::Walker::new();
            let s = walker.step(0.0);
            let mut feet = Feet::new(&FlyPose { x: 0.0, y: 0.0, heading: s.heading, speed: 0.0, gait_phase: s.gait_phase }, scale);
            let (mut spent, mut frames) = (std::time::Duration::ZERO, 0);
            for _ in 0..60 * 60 {
                let s = walker.step(1.0 / 60.0);
                let pose = FlyPose { x: s.x * scale, y: s.y * scale, heading: s.heading, speed: s.speed, gait_phase: s.gait_phase };
                feet.update(&pose, scale);
                let t = std::time::Instant::now();
                pm.fill(Color::TRANSPARENT);
                draw(&pose, &feet, scale, &mut pm);
                spent += t.elapsed();
                frames += 1;
            }
            let art = size as u32 / art_px(scale);
            println!("scale {scale}: {size} px window, {art} x {art} art px, {:.3} ms per frame", spent.as_secs_f64() * 1000.0 / frames as f64);
        }
    }
}
