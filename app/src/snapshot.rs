//! `flit --snapshot <dir>`: renders the pixel-art reference images (no window) so they can be
//! compared side by side with `reference/pixel_*.png`. Same tile sizes, layout, backgrounds,
//! headings and zooms as `reference/fly_pixel.py`'s `__main__`, at 100% display scale.

use crate::art;
use crate::fly::{FLY_SCALE, Feet, FlyPose};
use std::path::Path;
use tiny_skia::{Color, Paint, Pixmap, PixmapPaint, Rect, Transform};

const WHITE: (f32, f32, f32) = (1.0, 1.0, 1.0);
const DARK: (f32, f32, f32) = (0.12, 0.12, 0.13);
const GREY: (f32, f32, f32) = (0.94, 0.95, 0.97);
const UP: f32 = -90.0;
/// The reference's `stride` moves the feet by hand; here the gait places them. 0.45 is the rest
/// pose (all six feet down), standing in for small strides.
const REST: f32 = 0.45;

fn color((r, g, b): (f32, f32, f32)) -> Color {
    Color::from_rgba(r, g, b, 1.0).unwrap()
}

/// The reference's `render` + `zoom`: the fly on a 111 x 111 art canvas (`86 * FLY_SCALE`),
/// upscaled nearest-neighbour by `zoom`, over `bg` if given.
fn tile(deg: f32, gait_phase: f32, zoom: u32, bg: Option<(f32, f32, f32)>) -> Pixmap {
    let size = (86.0 * FLY_SCALE) as u32;
    let pose = FlyPose { x: 0.0, y: 0.0, heading: deg.to_radians(), speed: 0.0, gait_phase };
    let mut small = Pixmap::new(size, size).unwrap();
    art::render_art(pose.heading, &Feet::new(&pose, 1.0), 1.0, &mut small);
    let mut out = Pixmap::new(size * zoom, size * zoom).unwrap();
    art::upscale(&small, zoom, &mut out);
    let Some(bg) = bg else { return out };
    let mut filled = Pixmap::new(out.width(), out.height()).unwrap();
    filled.fill(color(bg));
    put(&mut filled, &out, 0, 0);
    filled
}

/// Source-over at a whole-pixel offset: no resampling.
fn put(canvas: &mut Pixmap, tile: &Pixmap, x: i32, y: i32) {
    canvas.draw_pixmap(x, y, tile.as_ref(), &PixmapPaint::default(), Transform::identity(), None);
}

/// The four reference images, rendered: (file name, pixmap).
pub fn render() -> [(&'static str, Pixmap); 4] {
    // 1. close-up, facing up, zoomed 5x
    let closeup = tile(UP, REST, 5, Some(WHITE));

    // 2. turning: several headings, zoomed 3x
    let heads = [-90.0, -75.0, -60.0, -45.0, -20.0, 0.0, 30.0, 135.0];
    let mut turning = Pixmap::new(heads.len() as u32 * 340, 340).unwrap();
    for (i, d) in heads.into_iter().enumerate() {
        put(&mut turning, &tile(d, REST, 3, Some(WHITE)), i as i32 * 340 + 2, 2);
    }

    // 3. walking: four points of the gait cycle, facing up, zoomed 3x
    let mut walking = Pixmap::new(4 * 340, 340).unwrap();
    for (i, phase) in [0.0, 0.25, 0.5, 0.75].into_iter().enumerate() {
        put(&mut walking, &tile(UP, phase, 3, Some(WHITE)), i as i32 * 340 + 2, 2);
    }

    // 4. actual size at 100% on white, dark, light grey (no zoom)
    let mut actual = Pixmap::new(3 * 160, 140).unwrap();
    for (i, bg) in [WHITE, DARK, GREY].into_iter().enumerate() {
        let x = i as f32 * 160.0;
        let mut paint = Paint::default();
        paint.set_color(color(bg));
        actual.fill_rect(Rect::from_xywh(x, 0.0, 160.0, 140.0).unwrap(), &paint, Transform::identity(), None);
        put(&mut actual, &tile(-60.0, REST, 1, None), x as i32 + 24, 14);
    }

    [
        ("pixel_closeup.png", closeup),
        ("pixel_turning.png", turning),
        ("pixel_walking.png", walking),
        ("pixel_actual_size.png", actual),
    ]
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

    /// Mean absolute per-channel difference (0..255) between two same-sized images.
    fn mean_diff(a: &Pixmap, b: &Pixmap) -> f32 {
        assert_eq!((a.width(), a.height()), (b.width(), b.height()));
        let sum: u64 = a.data().iter().zip(b.data()).map(|(x, y)| x.abs_diff(*y) as u64).sum();
        sum as f32 / a.data().len() as f32
    }

    /// The port must look like the reference renders. Not pixel-exact: cairo and tiny-skia put
    /// non-AA edges up to a pixel apart, and the gait places the legs, not the reference's
    /// `stride`. A wrong palette, a misplaced part or a channel swap pushes this well past the bar.
    #[test]
    fn snapshots_look_like_reference() {
        for (name, pm) in render() {
            let reference = Pixmap::load_png(concat!(env!("CARGO_MANIFEST_DIR"), "/../reference/").to_owned() + name).unwrap();
            let d = mean_diff(&pm, &reference);
            println!("{name}: mean abs diff {d:.2} / 255");
            assert!(d < 6.0, "{name} differs from the reference: {d}");
        }
    }
}
