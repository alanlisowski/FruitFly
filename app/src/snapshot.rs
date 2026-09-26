//! `flit --snapshot <dir>`: renders the three reference images (no window) so they can be
//! compared side by side with `reference/ref_*.png`. Same tile sizes, layout, backgrounds,
//! headings and scales as the reference's `__main__`.

use crate::art;
use crate::fly::{FLY_SCALE, Feet, FlyPose};
use std::path::Path;
use tiny_skia::{Color, Pixmap, PixmapPaint, Transform};

const WHITE: (f32, f32, f32) = (1.0, 1.0, 1.0);
const DARK: (f32, f32, f32) = (0.12, 0.12, 0.13);
const GREY: (f32, f32, f32) = (0.94, 0.95, 0.97);

fn color((r, g, b): (f32, f32, f32)) -> Color {
    Color::from_rgba(r, g, b, 1.0).unwrap()
}

/// One fly on a background, centred. `total_scale` = px per body unit (the reference's `scale`).
/// `gait_phase` stands in for the reference's `stride`: see `main`.
fn tile(w: u32, h: u32, bg: (f32, f32, f32), heading: f32, total_scale: f32, gait_phase: f32) -> Pixmap {
    let mut pm = Pixmap::new(w, h).unwrap();
    pm.fill(color(bg));
    let scale = total_scale / FLY_SCALE; // `draw` takes dpi/96; FLY_SCALE is applied inside
    let pose = FlyPose { x: w as f32 / 2.0, y: h as f32 / 2.0, heading, speed: 0.0, gait_phase };
    art::draw(&pose, &Feet::new(&pose, scale), scale, &mut pm, (0, 0));
    pm
}

fn put(canvas: &mut Pixmap, tile: &Pixmap, x: i32, y: i32) {
    canvas.draw_pixmap(x, y, tile.as_ref(), &PixmapPaint::default(), Transform::identity(), None);
}

/// The three reference images, rendered: (file name, pixmap).
pub fn render() -> [(&'static str, Pixmap); 3] {
    // Gait phases that put the feet where the reference's stand-in `stride` does.
    // 0.70: one tripod is mid-stance (feet at their neutral spots) and the other mid-swing
    // (feet also at neutral, lifted 1.2 units outward) -- the closest a real gait gets to the
    // reference's "stride = 0" rest pose. (The in-app rest pose, 0.45, has all six feet planted
    // with the tripods half a step apart.)
    const NEUTRAL: f32 = 0.70;

    // zoomed 4.2x: rest, turned -35 deg, turned 140 deg; on white and dark
    let mut zoomed = Pixmap::new(1400, 870).unwrap();
    zoomed.fill(color((0.93, 0.93, 0.92)));
    for (i, deg) in [0.0_f32, -35.0, 140.0].into_iter().enumerate() {
        for (j, bg) in [WHITE, DARK].into_iter().enumerate() {
            put(&mut zoomed, &tile(440, 420, bg, deg.to_radians(), 4.2, NEUTRAL), 15 + i as i32 * 460, 15 + j as i32 * 435);
        }
    }

    // actual size at FLY_SCALE on white, dark, light grey
    let mut actual = Pixmap::new(960, 240).unwrap();
    for (k, bg) in [WHITE, DARK, GREY].into_iter().enumerate() {
        put(&mut actual, &tile(320, 240, bg, (-25.0_f32).to_radians(), FLY_SCALE, NEUTRAL), k as i32 * 320, 0);
    }

    // both tripod phases, zoomed: legs must never cross. 0.99 = tripod A at the back of its
    // stance, B forward; 0.40 = the other way round (the reference's stride -3 / +3).
    let mut stride = Pixmap::new(900, 420).unwrap();
    for (k, phase) in [0.99_f32, 0.40].into_iter().enumerate() {
        put(&mut stride, &tile(450, 420, WHITE, 0.0, 4.2, phase), k as i32 * 450, 0);
    }
    [("ref_zoomed.png", zoomed), ("ref_actual_size.png", actual), ("ref_stride.png", stride)]
}

pub fn run(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    for (name, pm) in render() {
        pm.save_png(dir.join(name)).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mean absolute per-channel difference (0..255) between two same-sized opaque images.
    fn mean_diff(a: &Pixmap, b: &Pixmap) -> f32 {
        assert_eq!((a.width(), a.height()), (b.width(), b.height()));
        let sum: u64 = a.data().iter().zip(b.data()).map(|(x, y)| x.abs_diff(*y) as u64).sum();
        sum as f32 / a.data().len() as f32
    }

    /// The port must stay close to the reference renders. Legs are placed by the gait, not by
    /// the reference's `stride`, so they differ a little; everything else is the same drawing.
    /// (Measured: ~1.0-2.2 of 255. A wrong palette, mis-transformed gradient or channel swap
    /// pushes this well past 5.)
    #[test]
    fn snapshots_match_reference() {
        for (name, pm) in render() {
            let reference = Pixmap::load_png(concat!(env!("CARGO_MANIFEST_DIR"), "/reference/").to_owned() + name).unwrap();
            let d = mean_diff(&pm, &reference);
            println!("{name}: mean abs diff {d:.2} / 255");
            assert!(d < 3.0, "{name} differs from the reference: {d}");
        }
    }
}
