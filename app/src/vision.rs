//! Milestone 4b: the fly sees edges near it (a cheap stand-in for the optic lobe). A small screen
//! patch around the head, in luminance, box-averaged into ~4 logical px cells (text smudges,
//! panel tone steps survive), then tone steps: a strong luminance step with flat tone on both
//! sides, holding for half a body length. Out come edge points in screen px, which
//! `world::seen` turns into contact like window segments.
//!
//! `see` is pure and unit-tested; Win32 is only in `capture`. The patch never leaves memory
//! (except `--dump-vision`).

use crate::fly::FLY_SCALE;
use crate::world::BODY_LENGTH;

/// Cell size, logical px: blurs glyphs, keeps panel boundaries.
pub const CELL: f32 = 4.0;
/// Patch radius around the head, body lengths.
const RADIUS_BL: f32 = 3.0;
/// Luminance step (0..1, sRGB-encoded) that counts: 0.05 catches #e6e6e6 | #ffffff (0.10).
const STEP: f32 = 0.05;
/// Tone range allowed within each side: a step between two flat tones, not text or a gradient.
const FLAT: f32 = 0.03;
/// Cells per side checked for flatness (12 logical px: more than a glyph band, less than a
/// sidebar's padding).
const SIDE: usize = 3;

/// One look: the patch (top-left in screen px, side in px, BGRA), its cells and the edges seen.
#[derive(Default)]
pub struct Look {
    pub origin: (i32, i32),
    pub size: usize,
    pub bgra: Vec<u8>,
    pub cell: usize,
    pub cells: Vec<f32>,
    /// Edge points, screen px.
    pub pts: Vec<(f32, f32)>,
    /// Capture + processing, ms.
    pub ms: f32,
}

/// Cell size in physical px at `scale`.
pub fn cell_px(scale: f32) -> usize {
    ((CELL * scale).round() as usize).max(1)
}

/// Cells an edge must hold for: half a body length.
pub fn run_cells(scale: f32, cell: usize) -> usize {
    (0.5 * BODY_LENGTH * FLY_SCALE * scale / cell as f32).ceil() as usize
}

/// Box-averages a `size` x `size` BGRA patch into luminance cells (`size / cell` per side), then
/// finds tone-step edges. Returns (cells, edge points in patch px).
pub fn see(bgra: &[u8], size: usize, cell: usize, run: usize) -> (Vec<f32>, Vec<(f32, f32)>) {
    let n = size / cell;
    let mut cells = vec![0.0_f32; n * n];
    for y in 0..n * cell {
        for (x, p) in bgra[y * size * 4..].chunks_exact(4).take(n * cell).enumerate() {
            cells[y / cell * n + x / cell] += 0.114 * p[0] as f32 + 0.587 * p[1] as f32 + 0.299 * p[2] as f32;
        }
    }
    let norm = 1.0 / (255.0 * (cell * cell) as f32);
    cells.iter_mut().for_each(|c| *c *= norm);

    let mut pts = vec![];
    let c = cell as f32;
    // Vertical edges (scan rows), then horizontal (scan columns).
    scan(n, run, |along, across| cells[along * n + across], &mut |a, x| pts.push((x * c, a * c)));
    scan(n, run, |along, across| cells[across * n + along], &mut |a, y| pts.push((a * c, y * c)));
    (cells, pts)
}

/// For every line `across` (with SIDE cells of room on both sides), walks `along` it; a cell whose
/// SIDE neighbours on each side are flat and differ by STEP is a candidate. Runs of `run` or more
/// same-signed candidates are edges: `emit(along, across)` in cells, `across` placed inside the
/// middle cell by its tone (sub-cell).
fn scan(n: usize, run: usize, at: impl Fn(usize, usize) -> f32, emit: &mut impl FnMut(f32, f32)) {
    let stats = |a: usize, r: std::ops::Range<usize>| {
        let (lo, hi, sum) = r.clone().fold((f32::MAX, f32::MIN, 0.0), |(lo, hi, s), i| {
            let v = at(a, i);
            (lo.min(v), hi.max(v), s + v)
        });
        (sum / r.len() as f32, hi - lo)
    };
    for x in SIDE..n.saturating_sub(SIDE) {
        let (mut sign, mut marks) = (0, vec![]);
        for a in 0..=n {
            let (s, pos) = if a == n {
                (0, 0.0)
            } else {
                let ((l, lr), (r, rr)) = (stats(a, x - SIDE..x), stats(a, x + 1..x + 1 + SIDE));
                let step = r - l;
                if step.abs() < STEP || lr > FLAT || rr > FLAT {
                    (0, 0.0)
                } else {
                    // Fraction of the middle cell already at the far tone = where the step sits.
                    let f = ((at(a, x) - l) / step).clamp(0.0, 1.0);
                    (step.signum() as i32, x as f32 + 1.0 - f)
                }
            };
            if s != sign {
                if sign != 0 && marks.len() >= run {
                    marks.iter().for_each(|&(a, p)| emit(a, p));
                }
                (sign, marks) = (s, vec![]);
            }
            if s != 0 {
                marks.push((a as f32 + 0.5, pos));
            }
        }
    }
}

/// Looks on a thread of its own: a screen BitBlt blocks ~15 ms waiting on the compositor (only
/// ~1 ms of it CPU), which would drop a frame every poll.
pub struct Eye {
    ask: std::sync::mpsc::Sender<((f32, f32), f32)>,
    seen: std::sync::mpsc::Receiver<Look>,
    busy: bool,
}

impl Eye {
    pub fn new() -> Eye {
        let (ask, asked) = std::sync::mpsc::channel::<((f32, f32), f32)>();
        let (tell, seen) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for (head, scale) in asked {
                if tell.send(look(head, scale)).is_err() {
                    break;
                }
            }
        });
        Eye { ask, seen, busy: false }
    }

    /// The newest look if one is ready, then asks for the next one around `head` (one at a
    /// time: the points are in screen px, so a look a poll old is still in the right place).
    pub fn poll(&mut self, head: (f32, f32), scale: f32) -> Option<Look> {
        let got = self.seen.try_recv().ok();
        self.busy &= got.is_none();
        if !self.busy {
            self.busy = self.ask.send((head, scale)).is_ok();
        }
        got
    }
}

/// Captures the patch around `head` (screen px) and looks at it. A failed capture (secure
/// desktop, UAC, lock screen) sees nothing.
pub fn look(head: (f32, f32), scale: f32) -> Look {
    let t = std::time::Instant::now();
    let cell = cell_px(scale);
    let size = ((2.0 * RADIUS_BL * BODY_LENGTH * FLY_SCALE * scale) as usize).div_ceil(cell) * cell;
    let origin = ((head.0 - size as f32 / 2.0).round() as i32, (head.1 - size as f32 / 2.0).round() as i32);
    let Some(bgra) = capture(origin, size, windows::Win32::Graphics::Gdi::SRCCOPY) else { return Look::default() };
    let (cells, pts) = see(&bgra, size, cell, run_cells(scale, cell));
    let pts = pts.into_iter().map(|(x, y)| (x + origin.0 as f32, y + origin.1 as f32)).collect();
    Look { origin, size, bgra, cell, cells, pts, ms: t.elapsed().as_secs_f32() * 1000.0 }
}

/// BGRA of the `size` x `size` screen square at `(x, y)` (virtual-screen physical px), via GDI
/// BitBlt from the screen DC. The app uses SRCCOPY; `main::self_exclusion` also tries CAPTUREBLT.
/// Our overlay stays out only via WDA_EXCLUDEFROMCAPTURE (set in `main`).
pub fn capture((x, y): (i32, i32), size: usize, rop: windows::Win32::Graphics::Gdi::ROP_CODE) -> Option<Vec<u8>> {
    use windows::Win32::Graphics::Gdi::*;
    let s = size as i32;
    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: s,
            biHeight: -s, // top-down
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    // SAFETY: every handle is created here and released before returning; `bits` points to
    // size*size*4 bytes owned by `bmp`, copied out before `bmp` is deleted.
    unsafe {
        let screen = GetDC(None);
        if screen.is_invalid() {
            return None;
        }
        let dc = CreateCompatibleDC(Some(screen));
        let mut bits = std::ptr::null_mut();
        let out = match CreateDIBSection(Some(dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
            Ok(bmp) => {
                let old = SelectObject(dc, bmp.into());
                let ok = BitBlt(dc, 0, 0, s, s, Some(screen), x, y, rop).is_ok();
                let out = ok.then(|| std::slice::from_raw_parts(bits as *const u8, size * size * 4).to_vec());
                SelectObject(dc, old);
                let _ = DeleteObject(bmp.into());
                out
            }
            Err(_) => None,
        };
        let _ = DeleteDC(dc);
        ReleaseDC(None, screen);
        out
    }
}

/// `--dump-vision`: `<i>_raw.png` (the patch), `<i>_cells.png` (cells, blown up to patch size) and
/// `<i>_edges.png` (cells with the edge points in red).
pub fn dump(l: &Look, dir: &std::path::Path, i: u32) -> Result<(), String> {
    let s = l.size as u32;
    let mut raw = tiny_skia::Pixmap::new(s, s).ok_or("no patch (capture failed)")?;
    for (d, p) in raw.data_mut().chunks_exact_mut(4).zip(l.bgra.chunks_exact(4)) {
        d.copy_from_slice(&[p[2], p[1], p[0], 255]);
    }
    let n = l.size / l.cell;
    let mut cells = tiny_skia::Pixmap::new(s, s).unwrap();
    for (k, d) in cells.data_mut().chunks_exact_mut(4).enumerate() {
        let (x, y) = ((k % l.size) / l.cell, (k / l.size) / l.cell);
        let v = if x < n && y < n { (l.cells[y * n + x] * 255.0) as u8 } else { 0 };
        d.copy_from_slice(&[v, v, v, 255]);
    }
    let mut edges = cells.clone();
    for &(x, y) in &l.pts {
        let (x, y) = ((x as i32 - l.origin.0) as usize, (y as i32 - l.origin.1) as usize);
        if x < l.size && y < l.size {
            edges.data_mut()[(y * l.size + x) * 4..][..4].copy_from_slice(&[255, 0, 0, 255]);
        }
    }
    for (name, pm) in [("raw", raw), ("cells", cells), ("edges", edges)] {
        pm.save_png(dir.join(format!("{i}_{name}.png"))).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::body::SENSE_AHEAD;
    use crate::world::{REACH, Seg, contact, seen};

    const SCALE: f32 = 1.25;

    /// A grey BGRA patch, `f(x, y)` = grey level 0..255.
    fn patch(size: usize, f: impl Fn(usize, usize) -> u8) -> Vec<u8> {
        (0..size * size).flat_map(|k| [f(k % size, k / size); 4]).collect()
    }

    /// (geometric, visual) contact for a fly at the patch centre heading up (-y), with the
    /// patch drawn by `f` and `segs` its geometric equivalent.
    fn both(f: impl Fn(usize, usize) -> u8, segs: &[Seg]) -> ((f32, f32), (f32, f32)) {
        let cell = cell_px(SCALE);
        let size = 72 * cell;
        let (_, pts) = see(&patch(size, f), size, cell, run_cells(SCALE, cell));
        let unit = FLY_SCALE * SCALE;
        let thorax = (size as f32 / 2.0, size as f32 / 2.0 + 10.0);
        let head = (thorax.0, thorax.1 - SENSE_AHEAD * unit);
        let h = -std::f32::consts::FRAC_PI_2;
        (contact(thorax, head, h, REACH * unit, segs), seen(thorax, head, h, REACH * unit, &pts))
    }

    fn vline(x: f32) -> [Seg; 1] {
        [Seg { a: (x, -1e4), b: (x, 1e4) }]
    }

    fn close(geo: f32, vis: f32) -> bool {
        (vis - geo).abs() <= 0.15 * geo
    }

    #[test]
    fn step_edge_matches_geometry_both_sides() {
        // Off-grid positions, both polarities, strong and sidebar-faint (#e6e6e6 | #ffffff).
        for (dark, light) in [(30, 220), (0xe6, 0xff)] {
            for x in [150.0, 153.0, 157.5] {
                let e = x as usize;
                let (geo, vis) = both(|px, _| if px < e { dark } else { light }, &vline(e as f32));
                assert!(geo.0 > 0.2 && close(geo.0, vis.0) && vis.1 < 0.02, "left x={e} {dark}|{light}: geo {geo:?} vis {vis:?}");
                let e = 360 - e;
                let (geo, vis) = both(|px, _| if px < e { light } else { dark }, &vline(e as f32));
                assert!(geo.1 > 0.2 && close(geo.1, vis.1) && vis.0 < 0.02, "right x={e} {dark}|{light}: geo {geo:?} vis {vis:?}");
            }
        }
        // Ahead: a step across the path feeds both sides a little (the nearest seen points sit
        // half a cell off the heading) and equally, so it doesn't steer, like a segment dead ahead.
        let (geo, vis) = both(|_, py| if py < 150 { 30 } else { 220 }, &[Seg { a: (-1e4, 150.0), b: (1e4, 150.0) }]);
        assert!(vis.0 < geo.0 + 0.1 && (vis.0 - vis.1).abs() < 0.01, "ahead: geo {geo:?} vis {vis:?}");
    }

    #[test]
    fn text_is_not_an_edge() {
        // 125%: glyphs 6 x 9 px, 2 px apart, a 7 px word gap every 5, 20 px lines, from x = 20.
        let glyph = |x: usize, y: usize| {
            let (x, line) = (x.wrapping_sub(20), y % 20);
            let (word, g) = (x % 47, x % 8);
            x < 400 && line < 9 && word < 40 && g < 6
        };
        let (_, vis) = both(|x, y| if glyph(x, y) { 20 } else { 255 }, &[]);
        assert!(vis.0 < 0.1 && vis.1 < 0.1, "text: {vis:?}");
        // Text right next to the fly, on a panel: the panel's edge counts, the text doesn't.
        let (geo, vis) = both(|x, y| if x < 150 { 0xe6 } else if x > 170 && glyph(x, y) { 20 } else { 255 }, &vline(150.0));
        assert!(close(geo.0, vis.0) && vis.1 < 0.1, "panel + text: geo {geo:?} vis {vis:?}");
    }

    /// Capture + processing per poll, on this machine's desktop at its centre (capture only, no
    /// window). Run: `cargo test --release vision_cost -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn vision_cost() {
        use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
        use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};
        // SAFETY: plain FFI calls.
        let c = unsafe {
            let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            (GetSystemMetrics(SM_CXSCREEN) as f32 / 2.0, GetSystemMetrics(SM_CYSCREEN) as f32 / 2.0)
        };
        // This thread's CPU time (kernel + user), ms: BitBlt mostly waits on the compositor.
        let cpu = || {
            use windows::Win32::System::Threading::{GetCurrentThread, GetThreadTimes};
            let mut t = [Default::default(); 4];
            // SAFETY: four FILETIME out-params, all locals.
            let _ = unsafe { GetThreadTimes(GetCurrentThread(), &mut t[0], &mut t[1], &mut t[2], &mut t[3]) };
            let ms = |f: windows::Win32::Foundation::FILETIME| ((f.dwHighDateTime as u64) << 32 | f.dwLowDateTime as u64) as f32 / 1e4;
            ms(t[2]) + ms(t[3])
        };
        let n = 100;
        let (mut ms, mut see_ms, mut pts, cpu0) = (0.0, 0.0, 0, cpu());
        for _ in 0..n {
            let l = look(c, SCALE);
            assert!(l.size > 0, "capture failed");
            let t = std::time::Instant::now();
            see(&l.bgra, l.size, l.cell, run_cells(SCALE, l.cell));
            (ms, see_ms, pts) = (ms + l.ms, see_ms + t.elapsed().as_secs_f32() * 1000.0, l.pts.len());
        }
        let cpu_ms = (cpu() - cpu0) / n as f32 - see_ms / n as f32; // minus the extra `see`
        println!("125%: {0} x {0} px patch, look {1:.2} ms/poll wall, {cpu_ms:.2} ms CPU (processing {2:.2} ms), {pts} edge points", look(c, SCALE).size, ms / n as f32, see_ms / n as f32);
    }
}
