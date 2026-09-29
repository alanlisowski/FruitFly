//! `--trace-map <png> [--minutes N]`: what the fly did, on one picture of the virtual screen at
//! 1/2 scale. Faint grey: every visible segment seen during the run; red: the last poll's;
//! magenta dots: edge points vision saw (every 5th poll); the path, blue (no contact) to yellow
//! (`FULL`); a legend line at the top. Pure: `main` feeds it.
//!
//! Contact here is raw combined (max of geometry and vision, per side, BEFORE adaptation): a fly
//! running alongside an edge feels at most ~0.44, and adaptation takes that to ~0.13 within
//! seconds, so adapted contact would paint a perfect border follow blue.

use crate::world::{Rect, Seg, World};
use std::collections::HashSet;
use tiny_skia::{Color, FillRule, Paint, PathBuilder, Pixmap, Stroke, Transform};

/// Contact above this counts as "in contact" for the legend.
const TOUCH: f32 = 0.3;
/// Contact painted full yellow: about the most a fly running alongside an edge feels.
const FULL: f32 = 0.5;

#[derive(Default)]
pub struct Trace {
    /// Per frame: x, y (screen px), max(contact L, R), dt in s.
    pub frames: Vec<(f32, f32, f32, f32)>,
    /// Every segment seen during the run (deduplicated), and the last poll's.
    pub ever: Vec<Seg>,
    keys: HashSet<[i32; 4]>,
    pub last: Vec<Seg>,
    pub work: Vec<Rect>,
    /// Vision edge points, snapped to 2 px so a long run doesn't pile up duplicates.
    pub dots: HashSet<(i32, i32)>,
    polls: u32,
}

impl Trace {
    pub fn frame(&mut self, x: f32, y: f32, contact: f32, dt: f32) {
        self.frames.push((x, y, contact, dt));
    }

    pub fn poll(&mut self, w: &World) {
        for s in &w.segs {
            if self.keys.insert([s.a.0, s.a.1, s.b.0, s.b.1].map(|v| v as i32)) {
                self.ever.push(*s);
            }
        }
        self.last = w.segs.clone();
        self.work = w.work.clone();
        if self.polls % 5 == 0 {
            self.dots.extend(w.look.pts.iter().map(|&(x, y)| ((x / 2.0) as i32 * 2, (y / 2.0) as i32 * 2)));
        }
        self.polls += 1;
    }

    /// Running time, s.
    pub fn secs(&self) -> f32 {
        self.frames.iter().map(|f| f.3).sum()
    }

    /// (minutes, % of time with contact > TOUCH, longest continuous contact > TOUCH in s).
    pub fn stats(&self) -> (f32, f32, f32) {
        let (mut on, mut run, mut longest) = (0.0, 0.0, 0.0_f32);
        for &(_, _, c, dt) in &self.frames {
            run = if c > TOUCH { run + dt } else { 0.0 };
            on += if c > TOUCH { dt } else { 0.0 };
            longest = longest.max(run);
        }
        let t = self.secs();
        (t / 60.0, if t > 0.0 { 100.0 * on / t } else { 0.0 }, longest)
    }

    pub fn legend(&self) -> String {
        let (min, pct, longest) = self.stats();
        format!("{min:.1} MIN   CONTACT>{TOUCH} {pct:.0}%   LONGEST {longest:.1} S")
    }

    pub fn draw(&self) -> Result<Pixmap, String> {
        let (l, t) = self.work.iter().fold((f32::MAX, f32::MAX), |(l, t), r| (l.min(r.l - 50.0), t.min(r.t - 50.0)));
        let (r, b) = self.work.iter().fold((f32::MIN, f32::MIN), |(x, y), q| (x.max(q.r + 50.0), y.max(q.b + 50.0)));
        let mut pm = Pixmap::new((((r - l) / 2.0).ceil() as u32).max(1), (((b - t) / 2.0).ceil() as u32).max(1)).ok_or("no work area")?;
        pm.fill(Color::from_rgba8(20, 20, 24, 255));
        let ts = Transform::from_scale(0.5, 0.5).pre_translate(-l, -t);
        let paint = |[r, g, b]: [u8; 3]| {
            let mut p = Paint::default();
            p.set_color_rgba8(r, g, b, 255);
            p
        };
        for w in &self.work {
            if let Some(rect) = tiny_skia::Rect::from_ltrb(w.l, w.t, w.r, w.b) {
                pm.fill_rect(rect, &paint([40, 40, 48]), ts, None);
            }
        }
        let stroke = Stroke { width: 4.0, ..Default::default() }; // 2 px on the map
        let mut lines = |segs: &[Seg], c| {
            let mut pb = PathBuilder::new();
            for s in segs {
                pb.move_to(s.a.0, s.a.1);
                pb.line_to(s.b.0, s.b.1);
            }
            if let Some(p) = pb.finish() {
                pm.stroke_path(&p, &paint(c), &stroke, ts, None);
            }
        };
        lines(&self.ever, [75, 75, 85]);
        lines(&self.last, [220, 40, 40]);
        let mut pb = PathBuilder::new();
        for &(x, y) in &self.dots {
            pb.push_circle(x as f32, y as f32, 2.0);
        }
        if let Some(p) = pb.finish() {
            pm.fill_path(&p, &paint([230, 0, 230]), FillRule::Winding, ts, None);
        }
        // ponytail: one stroke per frame (~11k for 3 min), fine for a one-off PNG
        for w in self.frames.windows(2) {
            let (a, b, c) = (w[0], w[1], (w[1].2 / FULL).clamp(0.0, 1.0));
            let mut pb = PathBuilder::new();
            pb.move_to(a.0, a.1);
            pb.line_to(b.0, b.1);
            if let Some(p) = pb.finish() {
                let col = [(255.0 * c) as u8, (255.0 * c) as u8, (255.0 * (1.0 - c)) as u8];
                pm.stroke_path(&p, &paint(col), &Stroke { width: 4.0, line_cap: tiny_skia::LineCap::Round, ..Default::default() }, ts, None);
            }
        }
        text(&mut pm, &self.legend());
        Ok(pm)
    }
}

/// 3 x 5 glyphs, one row per 3 bits, for the legend (no font dependency for one line).
fn glyph(c: char) -> [u8; 5] {
    match c {
        '0' | 'O' => [7, 5, 5, 5, 7],
        '1' => [2, 6, 2, 2, 7],
        '2' => [7, 1, 7, 4, 7],
        '3' => [7, 1, 7, 1, 7],
        '4' => [5, 5, 7, 1, 1],
        '5' | 'S' => [7, 4, 7, 1, 7],
        '6' => [7, 4, 7, 5, 7],
        '7' => [7, 1, 1, 1, 1],
        '8' => [7, 5, 7, 5, 7],
        '9' => [7, 5, 7, 1, 7],
        '.' => [0, 0, 0, 0, 2],
        '%' => [5, 1, 2, 4, 5],
        '>' => [4, 2, 1, 2, 4],
        'A' => [2, 5, 7, 5, 5],
        'C' => [7, 4, 4, 4, 7],
        'E' => [7, 4, 6, 4, 7],
        'G' => [7, 4, 5, 5, 7],
        'I' => [7, 2, 2, 2, 7],
        'L' => [4, 4, 4, 4, 7],
        'M' => [5, 7, 7, 5, 5],
        'N' => [6, 5, 5, 5, 5],
        'T' => [7, 2, 2, 2, 2],
        _ => [0; 5],
    }
}

/// White text, 3 px per font pixel, on a dark strip along the top.
fn text(pm: &mut Pixmap, s: &str) {
    const K: usize = 3;
    let w = pm.width() as usize;
    let rows = (7 * K).min(pm.height() as usize);
    pm.data_mut()[..rows * w * 4].chunks_exact_mut(4).for_each(|p| p.copy_from_slice(&[0, 0, 0, 255]));
    for (i, c) in s.chars().enumerate() {
        for (gy, bits) in glyph(c).into_iter().enumerate() {
            for gx in 0..3 {
                if bits >> (2 - gx) & 1 == 0 {
                    continue;
                }
                for (dy, dx) in (0..K * K).map(|k| (k / K, k % K)) {
                    let (x, y) = (K + (i * 4 + gx) * K + dx, K + gy * K + dy);
                    if x < w && y < rows {
                        pm.data_mut()[(y * w + x) * 4..][..4].copy_from_slice(&[255; 4]);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::contact;

    /// A synthetic run: a straight path 20 px beside a segment (yellow on the map), then one
    /// through open space (blue); contact computed as the fly would feel it.
    #[test]
    fn path_colours_follow_contact() {
        let seg = [Seg { a: (0.0, 200.0), b: (1600.0, 200.0) }];
        let world = World { segs: seg.to_vec(), work: vec![Rect { l: 0.0, t: 0.0, r: 1600.0, b: 1000.0 }], ..Default::default() };
        let mut tr = Trace::default();
        tr.poll(&world);
        let reach = crate::world::REACH * crate::fly::FLY_SCALE;
        for y in [220.0, 700.0] {
            for i in 0..600 {
                let x = 100.0 + i as f32 * 2.0;
                let (l, r) = contact((x, y), (x + 26.0, y), 0.0, reach, &seg);
                tr.frame(x, y, l.max(r), 1.0 / 60.0);
            }
        }
        let pm = tr.draw().unwrap();
        if let Ok(out) = std::env::var("FLIT_MAP") {
            pm.save_png(out).unwrap(); // to eyeball it
        }
        // Map px = (screen + 50) / 2.
        let px = |x: f32, y: f32| {
            let p = pm.pixel(((x + 50.0) / 2.0) as u32, ((y + 50.0) / 2.0) as u32).unwrap();
            (p.red(), p.green(), p.blue())
        };
        let (r, g, b) = px(700.0, 220.0);
        assert!(r > 200 && g > 200 && b < 60, "along the segment: {:?}", (r, g, b));
        let (r, g, b) = px(700.0, 700.0);
        assert!(r < 30 && g < 30 && b > 220, "open space: {:?}", (r, g, b));
        assert_eq!(px(700.0, 200.0), (220, 40, 40), "the segment, red");
        let (min, pct, longest) = tr.stats();
        assert!((min - 20.0 / 60.0).abs() < 1e-3 && (pct - 50.0).abs() < 2.0 && (longest - 10.0).abs() < 0.2, "{min} {pct} {longest}");
        // Legend strip: some white pixels up top.
        assert!(pm.data()[..pm.width() as usize * 4 * 21].chunks_exact(4).any(|p| p == [255; 4]));
    }
}
