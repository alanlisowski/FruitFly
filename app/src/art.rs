//! The fly's drawing: a port of `reference/fly_reference.py` (pycairo) to tiny-skia.
//! Geometry, palette ("green"), line weights, draw order and leg IK are the reference's.
//!
//! cairo -> tiny-skia, the parts that differ:
//! * cairo transforms gradients along with the drawing; tiny-skia's `fill_path`/`stroke_path`
//!   take a `Transform` per call and apply it to the path AND the paint's shader. So every
//!   gradient below is built in body space and drawn with the body transform, and follows
//!   the fly. (Draw with the identity transform and the shading would stay on the screen.)
//! * cairo's `clip` becomes a `Mask` built from the part's own path.
//! * Ellipses are real oval paths in body space (`oval`), not scaled circles: scaling a unit
//!   circle non-uniformly would scale the outline stroke too, so its thickness would vary
//!   around the shape.
//! * The reference seeds Python's RNG so tufts are identical every frame. Here all random
//!   geometry (fuzz, spikes, tuft) is generated once (`ART`), in body space, from a seeded RNG.

use crate::fly::{FLY_SCALE, Feet, FlyPose, LEGS, leg_geo, side_of};
use std::f32::consts::{PI, TAU};
use std::sync::OnceLock;
use tiny_skia::{
    Color, FillRule, FilterQuality, GradientStop, LineCap, LineJoin, LinearGradient, Mask, Paint, Path,
    PathBuilder, Pixmap, PixmapPaint, Point, RadialGradient, Rect, Shader, SpreadMode, Stroke, Transform,
};

type Rgb = (f32, f32, f32);

// --- Palette ("green") --------------------------------------------------------------------
const INK: Rgb = (0.11, 0.08, 0.13); // warm dark ink, softer than pure black
// yellow-green highlight -> muted green -> grey-green shade -> grey rim
const BODY: [Rgb; 4] = [(0.86, 0.90, 0.48), (0.30, 0.58, 0.36), (0.20, 0.32, 0.27), (0.15, 0.17, 0.17)];
const EYE: [Rgb; 3] = [(1.00, 0.52, 0.58), (0.80, 0.16, 0.26), (0.40, 0.04, 0.12)];
const WING: (Rgb, f32) = ((0.92, 0.93, 0.92), 0.40);
const SMOKE: (Rgb, f32) = ((0.45, 0.47, 0.48), 0.24);
const GLINT: (Rgb, f32) = ((1.00, 0.88, 0.35), 0.70); // yellow glint where the light hits
const RIM: (Rgb, f32) = ((0.58, 0.60, 0.62), 0.55); // grey rim on the shadow side
const SEGMENT: Rgb = (0.92, 0.80, 0.30); // yellow abdomen stripes

/// The light, in SCREEN space (top-left).
const LIGHT: (f32, f32) = (-0.55, -0.83);

/// Rotates the screen light into body space, so highlights stay top-left on the screen as the
/// fly turns (otherwise the shading looks painted on).
fn light_in_body(heading: f32) -> (f32, f32) {
    let (s, c) = (-heading).sin_cos();
    (LIGHT.0 * c - LIGHT.1 * s, LIGHT.0 * s + LIGHT.1 * c)
}

// --- Small helpers ------------------------------------------------------------------------
fn rgba(c: Rgb, a: f32) -> Color {
    Color::from_rgba(c.0, c.1, c.2, a).unwrap()
}
fn solid(c: Rgb, a: f32) -> Shader<'static> {
    Shader::SolidColor(rgba(c, a))
}
fn stops(v: &[(f32, Color)]) -> Vec<GradientStop> {
    v.iter().map(|&(t, c)| GradientStop::new(t, c)).collect()
}
fn pt(x: f32, y: f32) -> Point {
    Point::from_xy(x, y)
}

/// An ellipse as a real path in body space, rotated by `ang` radians about its centre.
fn oval(cx: f32, cy: f32, rx: f32, ry: f32, ang: f32) -> Path {
    let p = PathBuilder::from_oval(Rect::from_xywh(cx - rx, cy - ry, 2.0 * rx, 2.0 * ry).unwrap()).unwrap();
    if ang == 0.0 { p } else { p.transform(Transform::from_rotate_at(ang.to_degrees(), cx, cy)).unwrap() }
}
fn circle(x: f32, y: f32, r: f32) -> Path {
    PathBuilder::from_circle(x, y, r).unwrap()
}

/// Everything the drawing paints goes through these two, so anti-aliasing is on for every paint.
fn fill(pm: &mut Pixmap, path: &Path, shader: Shader<'static>, ts: Transform, mask: Option<&Mask>) {
    let mut paint = Paint::default();
    paint.shader = shader;
    paint.anti_alias = true;
    pm.fill_path(path, &paint, FillRule::Winding, ts, mask);
}
fn stroke(pm: &mut Pixmap, path: &Path, shader: Shader<'static>, width: f32, round: bool, ts: Transform, mask: Option<&Mask>) {
    let mut paint = Paint::default();
    paint.shader = shader;
    paint.anti_alias = true;
    let st = Stroke {
        width,
        line_cap: if round { LineCap::Round } else { LineCap::Butt },
        line_join: if round { LineJoin::Round } else { LineJoin::Miter },
        ..Default::default()
    };
    pm.stroke_path(path, &paint, &st, ts, mask);
}

/// cairo's `clip`: coverage of `path` under `ts`.
fn set_clip(mask: &mut Mask, path: &Path, ts: Transform) {
    mask.clear();
    mask.fill_path(path, FillRule::Winding, true, ts);
}

// --- Static (generated once) geometry -----------------------------------------------------
/// splitmix64: tiny, deterministic. Doesn't need to match Python's RNG, just never change.
struct Rng(u64);
impl Rng {
    fn uniform(&mut self, a: f32, b: f32) -> f32 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        a + (b - a) * ((z >> 40) as f32 / (1u64 << 24) as f32)
    }
}

/// Soft curved tufts along an elliptical rim, curled backward like they've been combed.
fn fuzz(cx: f32, cy: f32, rx: f32, ry: f32, n: usize, seed: u64, length: f32, skip_front: bool) -> Path {
    let mut rnd = Rng(seed);
    let mut pb = PathBuilder::new();
    for i in 0..n {
        let a = TAU * i as f32 / n as f32 + rnd.uniform(-0.15, 0.15);
        if skip_front && a.sin().atan2(a.cos()).abs() < 0.8 {
            continue;
        }
        let (x, y) = (cx + a.cos() * rx, cy + a.sin() * ry);
        let (mut nx, mut ny) = (a.cos() / rx, a.sin() / ry);
        let nn = nx.hypot(ny);
        (nx, ny) = (nx / nn, ny / nn);
        let l = length * rnd.uniform(0.7, 1.2);
        let (ex, ey) = (x + nx * l - 0.5 * l, y + ny * l);
        pb.move_to(x, y);
        pb.cubic_to(x + nx * l * 0.6, y + ny * l * 0.6, ex + 0.3, ey, ex, ey);
    }
    pb.finish().unwrap()
}

struct Art {
    abdomen: Path,
    thorax: Path,
    head: Path,
    eyes: [Path; 2],
    stripes: Path,
    fuzz_abdomen: Path,
    fuzz_thorax: Path,
    tuft: Path,
    wing: Path,
    veins: Path,
    wing_sheen: Path,
    /// Two random spike-length factors per leg (index = foot index), fixed for good.
    spikes: [[f32; 2]; 6],
}

fn art() -> &'static Art {
    static ART: OnceLock<Art> = OnceLock::new();
    ART.get_or_init(|| {
        let mut stripes = PathBuilder::new();
        for x0 in [-7.0_f32, -11.5] {
            stripes.move_to(x0 + 1.0, -6.5);
            stripes.cubic_to(x0 - 1.6, -2.5, x0 - 1.6, 2.5, x0 + 1.0, 6.5);
        }
        let mut tuft = PathBuilder::new();
        for (dy, dl) in [(-1.5_f32, 2.8_f32), (0.0, 3.4), (1.5, 2.8)] {
            tuft.move_to(4.6, dy);
            tuft.cubic_to(3.2, dy * 1.3, 2.4, dy * 1.6, 4.6 - dl, dy * 2.1);
        }
        // Wing outline in wing space (origin at the wing root).
        let mut wing = PathBuilder::new();
        wing.move_to(0.0, 0.0);
        wing.cubic_to(4.0, -6.0, 17.0, -8.0, 23.5, -4.8);
        wing.cubic_to(28.0, -2.2, 27.5, 4.0, 22.0, 5.2);
        wing.cubic_to(13.0, 6.8, 4.0, 3.6, 0.0, 0.0);
        wing.close();
        let mut veins = PathBuilder::new();
        for end in [-3.6_f32, -0.6, 2.4] {
            veins.move_to(1.5, 0.0);
            veins.cubic_to(8.0, end * 0.35, 15.0, end * 0.8, 23.0, end);
        }
        let mut sheen = PathBuilder::new();
        sheen.move_to(6.0, -3.4);
        sheen.cubic_to(10.0, -5.2, 15.0, -5.6, 19.0, -5.0);

        let mut spikes = [[0.0; 2]; 6];
        for (k, s) in spikes.iter_mut().enumerate() {
            let mut rnd = Rng(31 * (k % 3) as u64 + (side_of(k) > 0.0) as u64);
            *s = [rnd.uniform(0.7, 1.2), rnd.uniform(0.7, 1.2)];
        }
        Art {
            abdomen: oval(-10.0, 0.0, 9.2, 8.0, 0.0),
            thorax: oval(2.8, 0.0, 7.2, 7.6, 0.0),
            head: oval(11.6, 0.0, 5.4, 6.2, 0.0),
            eyes: [-1.0_f32, 1.0].map(|s| oval(13.4, 3.75 * s, 4.1, 3.9, 0.15 * s)),
            stripes: stripes.finish().unwrap(),
            fuzz_abdomen: fuzz(-10.0, 0.0, 9.2, 8.0, 22, 5, 1.9, false),
            fuzz_thorax: fuzz(2.8, 0.0, 7.2, 7.6, 16, 11, 1.8, true),
            tuft: tuft.finish().unwrap(),
            wing: wing.finish().unwrap(),
            veins: veins.finish().unwrap(),
            wing_sheen: sheen.finish().unwrap(),
            spikes,
        }
    })
}

// --- Parts --------------------------------------------------------------------------------
struct Ctx<'a> {
    pm: &'a mut Pixmap,
    mask: Mask,
    /// Body space -> pixmap.
    body: Transform,
    /// Light direction in body space.
    l: (f32, f32),
}

/// Fill + ink the way an illustrator would: a dark offset silhouette first, so the outline is
/// heavier on the side away from the light. Draw order: light halo, offset ink, gradient fill,
/// yellow glint, grey shadow-side rim (clipped to the part), thin ink outline.
fn inked_part(c: &mut Ctx, path: &Path, (cx, cy): (f32, f32), size: f32, colors: [Rgb; 4], weight: f32, sheen: bool) {
    let (ts, l) = (c.body, c.l);
    // 0. faint light rim so a dark body still separates from a dark background
    stroke(c.pm, path, solid((1.0, 1.0, 1.0), 0.14), 2.4 * weight, false, ts, None);
    // 1. heavy ink on the shadow side: the same silhouette nudged away from the light
    fill(c.pm, path, solid(INK, 1.0), ts.pre_translate(-l.0 * 0.6 * weight, -l.1 * 0.6 * weight), None);
    // 2. shaded fill, highlight pushed toward the light
    let g = RadialGradient::new(
        pt(cx + l.0 * size * 0.45, cy + l.1 * size * 0.45),
        0.0,
        pt(cx, cy),
        size * 1.25,
        stops(&[
            (0.0, rgba(colors[0], 1.0)),
            (0.35, rgba(colors[1], 1.0)),
            (0.75, rgba(colors[2], 1.0)),
            (1.0, rgba(colors[3], 1.0)),
        ]),
        SpreadMode::Pad,
        Transform::identity(), // the draw call's `ts` is applied to this shader too
    )
    .unwrap();
    fill(c.pm, path, g, ts, None);
    if sheen {
        // 2b. a tight metallic glint where the light hits...
        let (hx, hy) = (cx + l.0 * size * 0.42, cy + l.1 * size * 0.42);
        let sg = RadialGradient::new(
            pt(hx, hy),
            0.0,
            pt(hx, hy),
            size * 0.36,
            stops(&[(0.0, rgba(GLINT.0, GLINT.1)), (1.0, rgba(GLINT.0, 0.0))]),
            SpreadMode::Pad,
            Transform::identity(),
        )
        .unwrap();
        fill(c.pm, path, sg, ts, None);
        // ...and a thin rim along the shadow-side edge only: a wide stroke, half of it clipped
        // away by the part's own outline.
        set_clip(&mut c.mask, path, ts);
        let lg = LinearGradient::new(
            pt(cx + l.0 * size, cy + l.1 * size),
            pt(cx - l.0 * size, cy - l.1 * size),
            stops(&[(0.0, rgba(RIM.0, 0.0)), (0.55, rgba(RIM.0, 0.0)), (1.0, rgba(RIM.0, RIM.1))]),
            SpreadMode::Pad,
            Transform::identity(),
        )
        .unwrap();
        stroke(c.pm, path, lg, size * 0.30, false, ts, Some(&c.mask));
    }
    // 3. thin ink line all round
    stroke(c.pm, path, solid(INK, 1.0), 1.15 * weight, false, ts, None);
}

/// A polyline in ink with a faint light halo under it (so it survives dark backgrounds).
fn ink_line(c: &mut Ctx, pts: &[(f32, f32)], w: f32) {
    let mut pb = PathBuilder::new();
    pb.move_to(pts[0].0, pts[0].1);
    for p in &pts[1..] {
        pb.line_to(p.0, p.1);
    }
    let path = pb.finish().unwrap();
    stroke(c.pm, &path, solid((1.0, 1.0, 1.0), 0.13), w + 1.2, true, c.body, None);
    stroke(c.pm, &path, solid(INK, 1.0), w, true, c.body, None);
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
    /// End of the tarsus, where the little round foot is drawn.
    tip: (f32, f32),
}

/// Leg `k`'s joints: the foot comes from the gait (`Feet`), the knee from the reference's
/// two-bone IK with FIXED femur and tibia lengths, choosing the knee farther from the body's
/// midline.
fn joints(k: usize, feet: &Feet) -> Joints {
    let (_, _, _, f, t, ts) = LEGS[k % 3];
    let side = side_of(k);
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
    let (ddx, ddy) = (foot.0 - knee.0, foot.1 - knee.1);
    let n = ddx.hypot(ddy).max(1e-6);
    let tip = (foot.0 + ddx / n * ts, foot.1 + ddy / n * ts + 0.8 * side);
    Joints { attach: g.attach, knee, foot, tip }
}

/// The six legs. All sit under the body, so strokes of one style are merged across legs (all
/// femur halos in one path, all femurs in another, ...): 9 draw calls instead of 54. Halos go
/// first, so a leg's halo never lightens another leg's ink.
fn draw_legs(pm: &mut Pixmap, body: Transform, feet: &Feet) {
    let art = art();
    let mut segs: [PathBuilder; 3] = Default::default(); // femurs, tibias, tarsi
    let (mut spikes, mut tips) = (PathBuilder::new(), Vec::with_capacity(6));
    for k in 0..6 {
        let side = side_of(k);
        let Joints { attach, knee, foot, tip } = joints(k, feet);
        for (pb, (a, b)) in segs.iter_mut().zip([(attach, knee), (knee, foot), (foot, tip)]) {
            pb.move_to(a.0, a.1);
            pb.line_to(b.0, b.1);
        }
        // Short bristles along the shin, pointing outward and toward the foot.
        let (sx, sy) = (foot.0 - knee.0, foot.1 - knee.1);
        let len = sx.hypot(sy).max(1e-6);
        let (ux, uy) = (sx / len, sy / len);
        let (nx, ny) = (-uy * side, ux * side);
        for (j, factor) in art.spikes[k].iter().enumerate() {
            let tt = (j + 1) as f32 / 3.0;
            let (x, y) = (knee.0 + sx * tt, knee.1 + sy * tt);
            let l = 1.4 * factor;
            spikes.move_to(x, y);
            spikes.line_to(x + (nx * 0.8 + ux * 0.6) * l, y + (ny * 0.8 + uy * 0.6) * l);
        }
        tips.push(tip);
    }
    let segs = segs.map(|pb| pb.finish().unwrap());
    let dots = |r: f32| {
        let mut pb = PathBuilder::new();
        tips.iter().for_each(|t| pb.push_circle(t.0, t.1, r));
        pb.finish().unwrap()
    };
    const W: [f32; 3] = [2.2, 1.8, 1.4];
    // faint light halo under the ink (so it survives dark backgrounds), little round feet
    for (p, w) in segs.iter().zip(W) {
        stroke(pm, p, solid((1.0, 1.0, 1.0), 0.13), w + 1.2, true, body, None);
    }
    fill(pm, &dots(1.6), solid((1.0, 1.0, 1.0), 0.13), body, None);
    for (p, w) in segs.iter().zip(W) {
        stroke(pm, p, solid(INK, 1.0), w, true, body, None);
    }
    stroke(pm, &spikes.finish().unwrap(), solid(INK, 1.0), 0.6, true, body, None);
    fill(pm, &dots(1.05), solid(INK, 1.0), body, None);
}

fn draw_wing(c: &mut Ctx, side: f32) {
    let art = art();
    // Wing space: root at (0.5, 3.6*side), pointing back in a wide V, mirrored so both wings
    // match. |scale| is equal on both axes, so stroke widths stay uniform.
    let ts = c.body.pre_translate(0.5, 3.6 * side).pre_rotate(180.0 - 33.0 * side).pre_scale(0.88, -0.88 * side);
    stroke(c.pm, &art.wing, solid((1.0, 1.0, 1.0), 0.22), 2.2, false, ts, None); // halo, for dark backgrounds
    fill(c.pm, &art.wing, solid(WING.0, WING.1), ts, None);
    // smoky root, fading out toward the tip
    let smoke = LinearGradient::new(
        pt(0.0, 0.0),
        pt(12.0, 0.0),
        stops(&[(0.0, rgba(SMOKE.0, SMOKE.1)), (1.0, rgba(SMOKE.0, 0.0))]),
        SpreadMode::Pad,
        Transform::identity(),
    )
    .unwrap();
    fill(c.pm, &art.wing, smoke, ts, None);
    // veins and one soft white streak along the leading edge, clipped to the wing
    set_clip(&mut c.mask, &art.wing, ts);
    stroke(c.pm, &art.veins, solid(INK, 0.6), 0.6, true, ts, Some(&c.mask));
    stroke(c.pm, &art.wing_sheen, solid((1.0, 1.0, 1.0), 0.65), 1.0, false, ts, Some(&c.mask));
    stroke(c.pm, &art.wing, solid(INK, 1.0), 1.0, false, ts, None);
}

/// Soft contact shadow: fixed down-right in SCREEN space, drawn before any rotation. It's an
/// ellipse 26 x 13 body units, offset (3, 5), fading from 30% black to nothing.
fn draw_shadow(pm: &mut Pixmap, centre: (f32, f32), unit: f32) {
    let ts = Transform::from_translate(centre.0 + 3.0 * unit, centre.1 + 5.0 * unit).pre_scale(26.0 * unit, 13.0 * unit);
    let g = RadialGradient::new(
        pt(0.0, 0.0),
        0.0,
        pt(0.0, 0.0),
        1.0,
        stops(&[(0.0, rgba((0.0, 0.0, 0.0), 0.30)), (1.0, rgba((0.0, 0.0, 0.0), 0.0))]),
        SpreadMode::Pad,
        Transform::identity(),
    )
    .unwrap();
    fill(pm, &circle(0.0, 0.0, 1.0), g, ts, None);
}

/// Transforms apply right-to-left to a point: scale body units -> px, rotate to heading, move to
/// `centre`. (tiny-skia's `pre_*` means "applied before what's there"; it rotates in degrees.)
fn body_ts(centre: (f32, f32), heading: f32, unit: f32) -> Transform {
    Transform::from_translate(centre.0, centre.1).pre_rotate(heading.to_degrees()).pre_scale(unit, unit)
}

/// Draws the fly onto `pixmap` (which the caller has cleared or filled with a background).
/// `scale` = dpi / 96. `origin` is the screen position of the pixmap's top-left pixel, i.e. the
/// window's position: the fly lands at `pose.xy - origin`, so if the window sits at the
/// *floored* position, the fractional part of `pose.xy` shifts the drawing inside the pixmap
/// and slow motion doesn't jitter.
///
/// The all-vector reference path; the app draws through `Cache::draw`, which must match it.
pub fn draw(pose: &FlyPose, feet: &Feet, scale: f32, pixmap: &mut Pixmap, origin: (i32, i32)) {
    let unit = FLY_SCALE * scale;
    let centre = (pose.x - origin.0 as f32, pose.y - origin.1 as f32);
    draw_shadow(pixmap, centre, unit);
    let body = body_ts(centre, pose.heading, unit);
    draw_legs(pixmap, body, feet);
    draw_rigid(pixmap, body, pose.heading);
}

/// Everything that never changes shape: abdomen, thorax, head, eyes, antennae, wings, lit for
/// `heading`.
fn draw_rigid(pm: &mut Pixmap, body: Transform, heading: f32) {
    let mask = Mask::new(pm.width(), pm.height()).unwrap();
    let mut c = Ctx { pm, mask, body, l: light_in_body(heading) };
    let art = art();

    // abdomen: chubby and round, with soft yellow segment arcs
    inked_part(&mut c, &art.abdomen, (-10.0, 0.0), 9.0, BODY, 1.0, true);
    set_clip(&mut c.mask, &art.abdomen, body);
    stroke(c.pm, &art.stripes, solid(SEGMENT, 0.9), 1.0, true, body, Some(&c.mask));
    stroke(c.pm, &art.fuzz_abdomen, solid(INK, 1.0), 0.6, true, body, None);

    // thorax: round, not boxy, with a little tuft on top
    inked_part(&mut c, &art.thorax, (2.8, 0.0), 7.2, BODY, 1.0, true);
    stroke(c.pm, &art.fuzz_thorax, solid(INK, 1.0), 0.6, true, body, None);
    stroke(c.pm, &art.tuft, solid(INK, 1.0), 0.7, true, body, None);

    // head: big and round (baby proportions read as friendly)
    inked_part(&mut c, &art.head, (11.6, 0.0), 5.8, BODY, 0.9, true);
    // goggle eyes: huge, touching in the middle
    let l = c.l;
    for (s, ep) in [-1.0_f32, 1.0].into_iter().zip(&art.eyes) {
        let (ecx, ecy) = (13.4, 3.75 * s);
        inked_part(&mut c, ep, (ecx, ecy), 4.0, [EYE[0], EYE[1], EYE[2], EYE[2]], 0.9, false);
        // two-point glint: a big soft oval toward the light, a small dot opposite
        let big = oval(ecx + l.0 * 1.5, ecy + l.1 * 1.5, 1.0, 1.45, l.1.atan2(l.0));
        fill(c.pm, &big, solid((1.0, 1.0, 1.0), 0.92), body, None);
        fill(c.pm, &circle(ecx - l.0 * 1.9, ecy - l.1 * 1.9, 0.5), solid((1.0, 1.0, 1.0), 0.70), body, None);
    }
    // bead-tipped antennae
    for s in [-1.0_f32, 1.0] {
        ink_line(&mut c, &[(16.6, 0.7 * s), (18.3, 1.5 * s), (19.3, 2.4 * s)], 0.8);
        fill(c.pm, &circle(19.4, 2.5 * s, 0.75), solid(INK, 1.0), body, None);
    }

    // wings on top
    for s in [-1.0_f32, 1.0] {
        draw_wing(&mut c, s);
    }
}

/// Body space covered by `draw_rigid`, in body units (x0, y0, w, h), with a margin
/// (`cache_matches_vector` checks nothing reaches the edge).
const RIGID: (f32, f32, f32, f32) = (-24.0, -22.0, 47.0, 44.0);
/// Re-render the rigid bitmap once the heading drifts this far from the one it was lit for.
/// Only the lighting goes stale; the rotation is exact every frame.
const RELIGHT: f32 = 8.0 * PI / 180.0;

/// Bitmaps of the parts that don't change shape, so a frame is two blits plus the legs.
/// The rigid body is rendered at 2x and drawn scaled by 0.5: a 1x bitmap drawn at an
/// arbitrary angle comes out soft; 2x keeps outlines as crisp as the vector path.
pub struct Cache {
    scale: f32,
    heading: f32,
    rigid: Pixmap,
    /// Screen space, never rotated.
    shadow: Pixmap,
}

impl Cache {
    pub fn new(scale: f32, heading: f32) -> Self {
        let unit = FLY_SCALE * scale;
        let px = |w: f32| (w * unit).ceil() as u32 + 4;
        let mut shadow = Pixmap::new(px(52.0), px(26.0)).unwrap();
        let mid = (shadow.width() as f32 / 2.0 - 3.0 * unit, shadow.height() as f32 / 2.0 - 5.0 * unit);
        draw_shadow(&mut shadow, mid, unit);
        let rigid = Pixmap::new(px(2.0 * RIGID.2), px(2.0 * RIGID.3)).unwrap();
        let mut cache = Cache { scale, heading, rigid, shadow };
        cache.relight(heading);
        cache
    }

    fn relight(&mut self, heading: f32) {
        let u2 = 2.0 * FLY_SCALE * self.scale;
        self.heading = heading;
        self.rigid.fill(Color::TRANSPARENT);
        draw_rigid(&mut self.rigid, Transform::from_scale(u2, u2).pre_translate(-RIGID.0, -RIGID.1), heading);
    }

    /// Same contract as `draw`. A different `scale` rebuilds the cache.
    pub fn draw(&mut self, pose: &FlyPose, feet: &Feet, scale: f32, pixmap: &mut Pixmap, origin: (i32, i32)) {
        if scale != self.scale {
            *self = Cache::new(scale, pose.heading);
        } else if ((pose.heading - self.heading + PI).rem_euclid(TAU) - PI).abs() > RELIGHT {
            self.relight(pose.heading);
        }
        let unit = FLY_SCALE * scale;
        let centre = (pose.x - origin.0 as f32, pose.y - origin.1 as f32);
        let paint = PixmapPaint { quality: FilterQuality::Bilinear, ..Default::default() };
        let (sw, sh) = (self.shadow.width() as f32, self.shadow.height() as f32);
        let shadow_at = Transform::from_translate(centre.0 + 3.0 * unit - sw / 2.0, centre.1 + 5.0 * unit - sh / 2.0);
        pixmap.draw_pixmap(0, 0, self.shadow.as_ref(), &paint, shadow_at, None);
        let body = body_ts(centre, pose.heading, unit);
        draw_legs(pixmap, body, feet);
        // bitmap px -> body units -> screen
        let rigid_at = body.pre_translate(RIGID.0, RIGID.1).pre_scale(0.5 / unit, 0.5 / unit);
        pixmap.draw_pixmap(0, 0, self.rigid.as_ref(), &paint, rigid_at, None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fly::{FlyPose, walk, window_size};

    /// The window is sized from `fly::RADIUS`, so nothing may ever be drawn on the pixmap's border
    /// pixels: try straight and turning walks at 100%, 150% and 200% scaling, with the
    /// worst-case fractional position. Legs at full stretch, wings and the shadow are all in.
    #[test]
    fn fits_in_window() {
        for scale in [1.0, 1.5, 2.0] {
            let size = window_size(scale);
            let n = size as usize;
            let mut pm = Pixmap::new(size as u32, size as u32).unwrap();
            let mut widest = 0.0_f32;
            for radius in [1e9, 60.0 * scale, -60.0 * scale] {
                let mut frame = 0;
                walk(scale, radius, 240, |pose, feet| {
                    let mut p = *pose;
                    (p.x, p.y) = (p.x.floor() + 0.99, p.y.floor() + 0.99);
                    let origin = (p.x.floor() as i32 - size / 2, p.y.floor() as i32 - size / 2);
                    pm.fill(Color::TRANSPARENT);
                    draw(&p, feet, scale, &mut pm, origin);
                    let edge = |i: usize| pm.data()[i * 4 + 3] != 0;
                    for i in 0..n {
                        assert!(!edge(i) && !edge((n - 1) * n + i), "top/bottom edge hit at {scale}");
                        assert!(!edge(i * n) && !edge(i * n + n - 1), "left/right edge hit at {scale}");
                    }
                    // farthest painted pixel from the fly, in body units (for tuning RADIUS)
                    frame += 1;
                    if frame % 8 != 0 {
                        return;
                    }
                    let (cx, cy) = (p.x - origin.0 as f32, p.y - origin.1 as f32);
                    for y in 0..n {
                        for x in 0..n {
                            if pm.data()[(y * n + x) * 4 + 3] > 8 {
                                let r = (x as f32 + 0.5 - cx).hypot(y as f32 + 0.5 - cy);
                                widest = widest.max(r / (FLY_SCALE * scale));
                            }
                        }
                    }
                });
            }
            println!("scale {scale}: window {size} px, widest painted pixel {widest:.1} body units");
        }
    }

    /// Only `pose.heading` changes the light; both eyes' glints must sit on the light side of
    /// their eye at any heading, i.e. the same top-left on the screen. Checks the maths that
    /// drives every highlight: body-space light rotated back to the screen is always LIGHT.
    #[test]
    fn light_stays_top_left_on_screen() {
        for deg in [0.0_f32, -35.0, 140.0, 90.0, 180.0, -170.0] {
            let h = deg.to_radians();
            let l = light_in_body(h);
            let (s, c) = h.sin_cos();
            let screen = (l.0 * c - l.1 * s, l.0 * s + l.1 * c);
            assert!((screen.0 - LIGHT.0).abs() < 1e-5 && (screen.1 - LIGHT.1).abs() < 1e-5, "{deg} deg");
        }
    }

    /// Fuzz, spikes and tuft are generated once: two calls return the same geometry.
    #[test]
    fn random_geometry_is_stable() {
        let a = fuzz(-10.0, 0.0, 9.2, 8.0, 22, 5, 1.9, false);
        let b = fuzz(-10.0, 0.0, 9.2, 8.0, 22, 5, 1.9, false);
        assert_eq!(a.points(), b.points());
        assert!(std::ptr::eq(art(), art()));
    }

    /// Gradients must travel with the body: draw the same fly 100 px apart and the two
    /// images must be identical (a shader left in screen space would shade differently).
    #[test]
    fn shading_moves_with_the_fly() {
        let (mut a, mut b) = (Pixmap::new(400, 300).unwrap(), Pixmap::new(400, 300).unwrap());
        for (pm, x) in [(&mut a, 100.0), (&mut b, 250.0)] {
            let pose = FlyPose { x, y: 150.0, heading: 0.6, speed: 0.0, gait_phase: 0.45 };
            draw(&pose, &Feet::new(&pose, 1.75), 1.75, pm, (0, 0));
        }
        let mut diff = 0u32;
        for y in 0..300 {
            for x in 0..150 {
                let (i, j) = ((y * 400 + x + 25) * 4, (y * 400 + x + 175) * 4);
                for ch in 0..4 {
                    diff = diff.max(a.data()[i + ch].abs_diff(b.data()[j + ch]) as u32);
                }
            }
        }
        assert!(diff <= 1, "shading didn't follow the fly: max channel diff {diff}");
    }

    /// The app's cached path must look like the vector path: at 0 deg (fresh cache), 7 deg
    /// (lit for 0), 45 and 140 deg (relit), and 147 deg (7 deg past the 140 relight), at 100%,
    /// 125% and 200%, on a window-sized pixmap. Same bar as the snapshot tests.
    #[test]
    fn cache_matches_vector() {
        for scale in [1.0_f32, 1.25, 2.0] {
            let size = window_size(scale) as u32;
            let mut cache = Cache::new(scale, 0.0);
            for deg in [0.0_f32, 7.0, 45.0, 140.0, 147.0] {
                let c = size as f32 / 2.0 + 0.37;
                let pose = FlyPose { x: c, y: c, heading: deg.to_radians(), speed: 0.0, gait_phase: 0.45 };
                let feet = Feet::new(&pose, scale);
                let (mut v, mut k) = (Pixmap::new(size, size).unwrap(), Pixmap::new(size, size).unwrap());
                draw(&pose, &feet, scale, &mut v, (0, 0));
                cache.draw(&pose, &feet, scale, &mut k, (0, 0));
                let d = v.data().iter().zip(k.data()).map(|(a, b)| a.abs_diff(*b) as u64).sum::<u64>() as f32
                    / v.data().len() as f32;
                println!("scale {scale}, {deg} deg (lit for {:.0}): mean diff {d:.2} / 255", cache.heading.to_degrees());
                assert!(d < 3.0, "cached fly differs at {scale}x, {deg} deg: {d}");
                // RIGID is big enough: nothing painted on the bitmap's border
                let (w, h) = (cache.rigid.width() as usize, cache.rigid.height() as usize);
                let a = |x: usize, y: usize| cache.rigid.data()[(y * w + x) * 4 + 3];
                assert!((0..w).all(|x| a(x, 0) == 0 && a(x, h - 1) == 0), "RIGID too small");
                assert!((0..h).all(|y| a(0, y) == 0 && a(w - 1, y) == 0), "RIGID too small");
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
        let segs = |q: &Joints| [[q.attach, q.knee], [q.knee, q.foot], [q.foot, q.tip]];
        for a in 0..6 {
            for b in a + 1..6 {
                if segs(&j[a]).iter().any(|sa| segs(&j[b]).iter().any(|sb| cross(*sa, *sb))) {
                    return Some((a, b));
                }
            }
        }
        None
    }

    /// Acceptance: legs never cross and segment lengths never change -- on the two reference
    /// stride frames, at every gait phase, and on straight and curved walks.
    #[test]
    fn legs_never_cross_and_never_stretch() {
        let check = |feet: &Feet, what: &str| {
            assert_eq!(crossing(feet), None, "{what}: legs cross");
            for k in 0..6 {
                let (_, _, _, f, t, ts) = LEGS[k % 3];
                let j = joints(k, feet);
                let len = |a: (f32, f32), b: (f32, f32)| (a.0 - b.0).hypot(a.1 - b.1);
                assert!((len(j.attach, j.knee) - f).abs() < 2e-3, "{what}: femur {k} is {}", len(j.attach, j.knee));
                assert!((len(j.knee, j.foot) - t).abs() < 2e-3, "{what}: tibia {k} is {}", len(j.knee, j.foot));
                // the tarsus is the reference's `ts`, plus its 0.8 sideways nudge
                assert!((len(j.foot, j.tip) - ts).abs() <= 0.8 + 1e-3, "{what}: tarsus {k}");
            }
        };
        for phase in [0.99_f32, 0.40] {
            let pose = FlyPose { x: 0.0, y: 0.0, heading: 0.0, speed: 0.0, gait_phase: phase };
            check(&Feet::new(&pose, 1.0), "stride frame");
        }
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
    /// Prints the cost of one frame's drawing at each display scale, vector vs cached, over one
    /// minute of the real route at 60 Hz (so the cached figure includes its relights), and the
    /// cache's memory. Run: `cargo test --release draw_cost -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn draw_cost() {
        for scale in [1.0_f32, 1.25, 2.0] {
            let size = window_size(scale);
            let mut pm = Pixmap::new(size as u32, size as u32).unwrap();
            let mut cache = Cache::new(scale, 0.0);
            let (mut relights, mut lit) = (0, cache.heading);
            let mut time = |cached: bool| {
                let mut walker = crate::path::Walker::new();
                let s = walker.step(0.0);
                let mut feet = Feet::new(&FlyPose { x: 0.0, y: 0.0, heading: s.heading, speed: 0.0, gait_phase: s.gait_phase }, scale);
                let (mut spent, mut frames) = (std::time::Duration::ZERO, 0);
                for _ in 0..60 * 60 {
                    let s = walker.step(1.0 / 60.0);
                    let pose = FlyPose { x: s.x * scale, y: s.y * scale, heading: s.heading, speed: s.speed, gait_phase: s.gait_phase };
                    feet.update(&pose, scale);
                    if s.speed == 0.0 {
                        continue; // stopped: the app skips these frames
                    }
                    let origin = (pose.x.floor() as i32 - size / 2, pose.y.floor() as i32 - size / 2);
                    let t = std::time::Instant::now();
                    pm.fill(Color::TRANSPARENT);
                    if cached {
                        cache.draw(&pose, &feet, scale, &mut pm, origin);
                        relights += (cache.heading != lit) as u32;
                        lit = cache.heading;
                    } else {
                        draw(&pose, &feet, scale, &mut pm, origin);
                    }
                    spent += t.elapsed();
                    frames += 1;
                }
                spent.as_secs_f64() * 1000.0 / frames as f64
            };
            let (v, c) = (time(false), time(true));
            let kb = (cache.rigid.data().len() + cache.shadow.data().len()) as f64 / 1024.0;
            println!("scale {scale}: {size}px window, vector {v:.2} ms, cached {c:.3} ms per frame ({relights} relights/min), cache {kb:.0} KiB");
        }
    }
}
