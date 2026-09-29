//! The world the fly walks on: visible window edges and the work-area border, as segments in
//! PHYSICAL virtual-screen pixels (the same space as `FlyPose`), and what the fly feels of them.
//!
//! The geometry (`visible_edges`, `border`) and the sense (`contact`) are pure Rust and unit-
//! tested. Win32 lives only in `poll` and below.

use std::time::Instant;
use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::core::BOOL;
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GW_OWNER, GWL_EXSTYLE, GetClassNameW, GetWindow, GetWindowLongW, IsIconic, IsWindowVisible,
    WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
};

/// Nose to abdomen tip, in body units (see `fly.rs`).
pub const BODY_LENGTH: f32 = 37.0;
/// How far the fly feels, from its sensing point, in body units: ~1.2 body lengths.
pub const REACH: f32 = 1.2 * BODY_LENGTH;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub l: f32,
    pub t: f32,
    pub r: f32,
    pub b: f32,
}

/// An axis-aligned segment from `a` to `b`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Seg {
    pub a: (f32, f32),
    pub b: (f32, f32),
}

impl Seg {
    fn shifted(self, (dx, dy): (f32, f32)) -> Seg {
        Seg { a: (self.a.0 + dx, self.a.1 + dy), b: (self.b.0 + dx, self.b.1 + dy) }
    }
}

/// Top, bottom, left, right, each with its outward normal.
fn sides(r: &Rect) -> [(Seg, (f32, f32)); 4] {
    [
        (Seg { a: (r.l, r.t), b: (r.r, r.t) }, (0.0, -1.0)),
        (Seg { a: (r.l, r.b), b: (r.r, r.b) }, (0.0, 1.0)),
        (Seg { a: (r.l, r.t), b: (r.l, r.b) }, (-1.0, 0.0)),
        (Seg { a: (r.r, r.t), b: (r.r, r.b) }, (1.0, 0.0)),
    ]
}

/// The parts of `s` outside every rect in `cover`. Rects are closed: an edge lying exactly on
/// a cover's boundary is covered (the cover's own edge is there instead, so it counts once).
fn uncovered(s: Seg, cover: &[Rect]) -> Vec<Seg> {
    let horizontal = s.a.1 == s.b.1;
    let (fixed, lo, hi) =
        if horizontal { (s.a.1, s.a.0.min(s.b.0), s.a.0.max(s.b.0)) } else { (s.a.0, s.a.1.min(s.b.1), s.a.1.max(s.b.1)) };
    let mut parts = vec![(lo, hi)];
    for r in cover {
        let (across, along) = if horizontal { ((r.t, r.b), (r.l, r.r)) } else { ((r.l, r.r), (r.t, r.b)) };
        if fixed < across.0 || fixed > across.1 {
            continue;
        }
        parts = parts
            .into_iter()
            .flat_map(|(a, b)| [(a, b.min(along.0)), (a.max(along.1), b)])
            .filter(|(a, b)| b > a)
            .collect();
    }
    parts
        .into_iter()
        .map(|(a, b)| if horizontal { Seg { a: (a, fixed), b: (b, fixed) } } else { Seg { a: (fixed, a), b: (fixed, b) } })
        .collect()
}

/// The parts of `s` inside the union of `rects` (closed), merged.
fn covered(s: Seg, rects: &[Rect]) -> Vec<Seg> {
    let horizontal = s.a.1 == s.b.1;
    let (fixed, lo, hi) =
        if horizontal { (s.a.1, s.a.0.min(s.b.0), s.a.0.max(s.b.0)) } else { (s.a.0, s.a.1.min(s.b.1), s.a.1.max(s.b.1)) };
    let mut parts: Vec<(f32, f32)> = rects
        .iter()
        .filter_map(|r| {
            let (across, along) = if horizontal { ((r.t, r.b), (r.l, r.r)) } else { ((r.l, r.r), (r.t, r.b)) };
            let (a, b) = (lo.max(along.0), hi.min(along.1));
            (fixed >= across.0 && fixed <= across.1 && b > a).then_some((a, b))
        })
        .collect();
    parts.sort_by(|x, y| x.0.total_cmp(&y.0));
    let mut merged: Vec<(f32, f32)> = vec![];
    for (a, b) in parts {
        match merged.last_mut() {
            Some(m) if a <= m.1 => m.1 = m.1.max(b),
            _ => merged.push((a, b)),
        }
    }
    merged
        .into_iter()
        .map(|(a, b)| if horizontal { Seg { a: (a, fixed), b: (b, fixed) } } else { Seg { a: (fixed, a), b: (fixed, b) } })
        .collect()
}

/// Everything the fly can feel: visible window edges, clipped to the work areas (a frame poking
/// into the taskbar strip doesn't count), plus the work-area border.
pub fn feelable(rects: &[Rect], work: &[Rect]) -> Vec<Seg> {
    let mut segs: Vec<Seg> = visible_edges(rects).into_iter().flat_map(|s| covered(s, work)).collect();
    segs.extend(border(work));
    segs
}

/// Visible window edges. `rects` are in z-order, topmost first (as EnumWindows returns them):
/// each window's sides are clipped against every window above it.
pub fn visible_edges(rects: &[Rect]) -> Vec<Seg> {
    visible_edges_by_window(rects).into_iter().flatten().collect()
}

/// `visible_edges`, per window.
pub fn visible_edges_by_window(rects: &[Rect]) -> Vec<Vec<Seg>> {
    rects.iter().enumerate().map(|(i, r)| sides(r).into_iter().flat_map(|(s, _)| uncovered(s, &rects[..i])).collect()).collect()
}

/// Distance from `p` to the nearest of `segs` (infinite if none).
#[cfg(test)]
pub fn distance(p: (f32, f32), segs: &[Seg]) -> f32 {
    segs.iter().map(|s| {
        let q = nearest(s, p);
        (q.0 - p.0).hypot(q.1 - p.1)
    }).fold(f32::INFINITY, f32::min)
}

/// The outer border of the union of the work areas: each side, minus the parts where another
/// work area continues on the far side (monitors that touch).
pub fn border(work: &[Rect]) -> Vec<Seg> {
    let mut out = vec![];
    for (i, r) in work.iter().enumerate() {
        let others: Vec<Rect> = work.iter().enumerate().filter(|&(j, _)| j != i).map(|(_, r)| *r).collect();
        for (s, n) in sides(r) {
            // Probe half a pixel outside the side: covered there = not a border.
            let probe = s.shifted((n.0 * 0.5, n.1 * 0.5));
            out.extend(uncovered(probe, &others).into_iter().map(|p| p.shifted((-n.0 * 0.5, -n.1 * 0.5))));
        }
    }
    out
}

fn nearest(s: &Seg, p: (f32, f32)) -> (f32, f32) {
    let (dx, dy) = (s.b.0 - s.a.0, s.b.1 - s.a.1);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 { (((p.0 - s.a.0) * dx + (p.1 - s.a.1) * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
    (s.a.0 + t * dx, s.a.1 + t * dy)
}

/// Smooth 0..1: 1 at distance 0, 0 at `reach` and beyond (smoothstep, no hard edge to twitch at).
fn falloff(d: f32, reach: f32) -> f32 {
    let u = (1.0 - d / reach).max(0.0);
    u * u * (3.0 - 2.0 * u)
}

/// What the fly feels: (left, right), each 0..1. Per segment, the point nearest the `head`
/// (the sensing point, ahead of the thorax): strength falls off smoothly with its distance
/// from the head, 0 at `reach`. Side and "behind"
/// are judged from the `thorax`: the point's bearing from the heading splits the strength by
/// its sine, so an edge dead ahead feeds neither side much and nothing flips as the bearing
/// sweeps round; full ahead and beside, fading to 0 directly behind. Summed per side, clamped.
///
/// Sensing ahead of the thorax is what damps edge following: angled toward an edge, the head
/// is nearer than the thorax, so the fly corrects before it gets there. Judging side from the
/// thorax keeps an edge the head has just crossed from vanishing "behind" the head. With the
/// edge on its left the fly turns left (CONTACT -> ipsilateral DNa02); crossing puts it on the
/// right, and the fly turns back: it ends up running along the line.
pub fn contact(thorax: (f32, f32), head: (f32, f32), heading: f32, reach: f32, segs: &[Seg]) -> (f32, f32) {
    let (mut l, mut r) = (0.0_f32, 0.0_f32);
    for s in segs {
        if let Some((_, pl, pr)) = feel(thorax, head, heading, reach, nearest(s, head)) {
            (l, r) = (l + pl, r + pr);
        }
    }
    (l.min(1.0), r.min(1.0))
}

/// One felt point `p`: (distance from the head, left, right), or None beyond reach.
fn feel(thorax: (f32, f32), head: (f32, f32), heading: f32, reach: f32, p: (f32, f32)) -> Option<(f32, f32, f32)> {
    let (hx, hy) = (heading.cos(), heading.sin());
    let d = (p.0 - head.0).hypot(p.1 - head.1);
    let (dx, dy) = (p.0 - thorax.0, p.1 - thorax.1);
    let b = dx.hypot(dy);
    if d >= reach || b < 1e-6 {
        return None;
    }
    let (cos, sin) = ((hx * dx + hy * dy) / b, (hx * dy - hy * dx) / b); // y down: sin > 0 = right
    let w = falloff(d, reach) * (1.0 + cos).min(1.0);
    Some((d, w * (-sin).max(0.0), w * sin.max(0.0)))
}

/// `contact` for seen edge points (`vision`): per side, the point on that side nearest the
/// head, weighted as a segment's nearest point is. A seen straight edge is a row of points, so
/// this matches the segment it stands for.
// ponytail: one edge per side; two seen edges on one side don't add up like segments do
pub fn seen(thorax: (f32, f32), head: (f32, f32), heading: f32, reach: f32, pts: &[(f32, f32)]) -> (f32, f32) {
    let (mut l, mut r) = ((f32::MAX, 0.0), (f32::MAX, 0.0));
    for &p in pts {
        if let Some((d, pl, pr)) = feel(thorax, head, heading, reach, p) {
            if pl > 0.0 && d < l.0 {
                l = (d, pl);
            }
            if pr > 0.0 && d < r.0 {
                r = (d, pr);
            }
        }
    }
    (l.1.min(1.0), r.1.min(1.0))
}

/// Mechanosensory adaptation, per side: under contact a receptor's gain sinks toward
/// `ADAPT_FLOOR` (tau `ADAPT_MS`), without contact it recovers toward 1 (tau `RECOVER_MS`). So
/// the fly follows an edge for a while, then its grip on it fades and it drifts off.
pub const ADAPT_MS: f32 = 3000.0;
pub const RECOVER_MS: f32 = 10_000.0;
pub const ADAPT_FLOOR: f32 = 0.3;
/// Raw contact above this counts as "under contact".
const TOUCHING: f32 = 0.02;

pub struct Adapt {
    /// Current gain, (left, right), `ADAPT_FLOOR..=1`.
    pub gain: (f32, f32),
}

impl Adapt {
    pub fn new() -> Adapt {
        Adapt { gain: (1.0, 1.0) }
    }

    /// Advances `dt_ms` with this `raw` contact held; returns the adapted contact.
    pub fn step(&mut self, raw: (f32, f32), dt_ms: f32) -> (f32, f32) {
        let relax = |g: f32, c: f32| {
            let (target, tau) = if c > TOUCHING { (ADAPT_FLOOR, ADAPT_MS) } else { (1.0, RECOVER_MS) };
            target + (g - target) * (-dt_ms / tau).exp()
        };
        self.gain = (relax(self.gain.0, raw.0), relax(self.gain.1, raw.1));
        (raw.0 * self.gain.0, raw.1 * self.gain.1)
    }
}

// --- Win32 ----------------------------------------------------------------------------------

/// One poll of the desktop.
#[derive(Default)]
pub struct World {
    /// Kept windows, topmost first, and their handles (for `--dump-world`).
    pub rects: Vec<Rect>,
    pub hwnds: Vec<HWND>,
    pub work: Vec<Rect>,
    /// What the fly can feel: visible window edges plus the work-area border.
    pub segs: Vec<Seg>,
    pub poll_ms: f32,
    /// What the fly saw around its head (`vision`), polled alongside.
    pub look: crate::vision::Look,
}

/// EnumWindows callback: collect only. No filtering and nothing that can panic, because a
/// panic unwinding across the FFI boundary aborts the process. (The push can't fail short of
/// running out of memory, which aborts anyway.)
unsafe extern "system" fn collect_window(hwnd: HWND, list: LPARAM) -> BOOL {
    // SAFETY: `list` is the `&mut Vec<HWND>` passed by `poll`, alive for the whole EnumWindows.
    unsafe { (*(list.0 as *mut Vec<HWND>)).push(hwnd) };
    BOOL(1)
}

unsafe extern "system" fn collect_monitor(m: HMONITOR, _: HDC, _: *mut RECT, list: LPARAM) -> BOOL {
    // SAFETY: as `collect_window`, with `&mut Vec<HMONITOR>`.
    unsafe { (*(list.0 as *mut Vec<HMONITOR>)).push(m) };
    BOOL(1)
}

fn to_rect(r: RECT) -> Rect {
    Rect { l: r.left as f32, t: r.top as f32, r: r.right as f32, b: r.bottom as f32 }
}

/// The visible frame of a window the fly should feel, or None to skip it.
fn frame_of(hwnd: HWND, own: HWND) -> Option<Rect> {
    // SAFETY: plain Win32 getters on a handle EnumWindows just gave us (if the window died
    // since, they fail, and we skip it). Out-params are locals of the exact size we pass.
    unsafe {
        if hwnd == own || !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
            return None;
        }
        let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        if ex & WS_EX_TOOLWINDOW.0 != 0 {
            return None;
        }
        let owned = GetWindow(hwnd, GW_OWNER).is_ok_and(|o| !o.is_invalid());
        if owned && ex & WS_EX_APPWINDOW.0 == 0 {
            return None;
        }
        let mut class = [0u16; 64];
        let n = GetClassNameW(hwnd, &mut class).max(0) as usize;
        if matches!(String::from_utf16_lossy(&class[..n]).as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd") {
            return None;
        }
        // Cloaked windows report visible but aren't on screen (UWP parking, other desktops).
        let mut cloaked = 0u32;
        DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, &mut cloaked as *mut u32 as *mut _, 4).ok()?;
        if cloaked != 0 {
            return None;
        }
        // The visible frame. GetWindowRect would include the invisible resize border.
        let mut r = RECT::default();
        DwmGetWindowAttribute(hwnd, DWMWA_EXTENDED_FRAME_BOUNDS, &mut r as *mut RECT as *mut _, size_of::<RECT>() as u32)
            .ok()?;
        (r.right > r.left && r.bottom > r.top).then(|| to_rect(r))
    }
}

/// Every monitor's work area (monitor minus taskbar).
pub fn work_areas() -> Vec<Rect> {
    let mut monitors: Vec<HMONITOR> = vec![];
    // SAFETY: the callback only pushes into `monitors`, which outlives the call.
    let _ = unsafe { EnumDisplayMonitors(None, None, Some(collect_monitor), LPARAM(&mut monitors as *mut _ as isize)) };
    monitors
        .into_iter()
        .filter_map(|m| {
            let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
            // SAFETY: `mi` is a valid, sized out-param; `m` came from EnumDisplayMonitors.
            unsafe { GetMonitorInfoW(m, &mut mi) }.as_bool().then(|| to_rect(mi.rcWork))
        })
        .collect()
}

/// Polls the desktop (the app calls this at 10 Hz). `own` is our overlay, never an obstacle.
pub fn poll(own: HWND) -> World {
    let t = Instant::now();
    let mut hwnds: Vec<HWND> = Vec::with_capacity(512);
    // SAFETY: the callback only pushes into `hwnds`, which outlives the call.
    let _ = unsafe { EnumWindows(Some(collect_window), LPARAM(&mut hwnds as *mut _ as isize)) };
    let (hwnds, rects): (Vec<HWND>, Vec<Rect>) = hwnds.into_iter().filter_map(|h| Some((h, frame_of(h, own)?))).unzip();
    let work = work_areas();
    let segs = feelable(&rects, &work);
    World { rects, hwnds, work, segs, poll_ms: t.elapsed().as_secs_f32() * 1000.0, ..Default::default() }
}

/// "class: title", for `--dump-world`'s listing.
pub fn describe(hwnd: HWND) -> String {
    let (mut class, mut title) = ([0u16; 64], [0u16; 80]);
    // SAFETY: plain getters into sized local buffers.
    let (c, t) = unsafe {
        (GetClassNameW(hwnd, &mut class).max(0) as usize, windows::Win32::UI::WindowsAndMessaging::GetWindowTextW(hwnd, &mut title).max(0) as usize)
    };
    format!("{}: {}", String::from_utf16_lossy(&class[..c]), String::from_utf16_lossy(&title[..t]))
}

/// `--dump-world <png>`: the virtual screen at 1/4 scale. Work areas dark grey, every kept
/// window's full outline light grey (so occluded parts stay grey), and what the fly can feel
/// (visible window edges, work-area border) red on top.
pub fn dump(world: &World, path: &std::path::Path) -> Result<(), String> {
    use tiny_skia::{Color, Paint, PathBuilder, Pixmap, Stroke, Transform};
    // The work areas plus a margin: windows can hang far off-screen, and don't matter there.
    let (l, t) = world.work.iter().fold((f32::MAX, f32::MAX), |(l, t), r| (l.min(r.l - 100.0), t.min(r.t - 100.0)));
    let (r, b) = world.work.iter().fold((f32::MIN, f32::MIN), |(x, y), q| (x.max(q.r + 100.0), y.max(q.b + 100.0)));
    let mut pm = Pixmap::new((((r - l) / 4.0).ceil() as u32).max(1), (((b - t) / 4.0).ceil() as u32).max(1)).ok_or("empty world")?;
    pm.fill(Color::from_rgba8(20, 20, 24, 255));
    let ts = Transform::from_scale(0.25, 0.25).pre_translate(-l, -t);
    let paint = |c: [u8; 3]| {
        let mut p = Paint::default();
        p.set_color_rgba8(c[0], c[1], c[2], 255);
        p
    };
    for w in &world.work {
        pm.fill_rect(tiny_skia::Rect::from_ltrb(w.l, w.t, w.r, w.b).unwrap(), &paint([48, 48, 56]), ts, None);
    }
    let lines = |pm: &mut Pixmap, segs: &mut dyn Iterator<Item = Seg>, c: [u8; 3]| {
        let mut pb = PathBuilder::new();
        for s in segs {
            pb.move_to(s.a.0, s.a.1);
            pb.line_to(s.b.0, s.b.1);
        }
        if let Some(p) = pb.finish() {
            pm.stroke_path(&p, &paint(c), &Stroke { width: 8.0, ..Default::default() }, ts, None);
        }
    };
    lines(&mut pm, &mut world.rects.iter().flat_map(|r| sides(r).map(|(s, _)| s)), [150, 150, 150]);
    lines(&mut pm, &mut world.segs.iter().copied(), [230, 40, 40]);
    pm.save_png(path).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(l: f32, t: f32, r: f32, b: f32) -> Rect {
        Rect { l, t, r, b }
    }
    fn total(segs: &[Seg]) -> f32 {
        segs.iter().map(|s| (s.b.0 - s.a.0).abs() + (s.b.1 - s.a.1).abs()).sum()
    }

    #[test]
    fn occlusion() {
        // A lone window: all four sides.
        assert_eq!(total(&visible_edges(&[rect(0.0, 0.0, 100.0, 50.0)])), 300.0);

        // Partial cover: the top window hides the middle 40 px of the lower one's top side and
        // part of its left side; its own four sides are all visible.
        let top = rect(-20.0, -10.0, 40.0, 30.0);
        let low = rect(0.0, 0.0, 100.0, 50.0);
        let v = visible_edges(&[top, low]);
        assert_eq!(total(&v), 200.0 + (100.0 - 40.0) + 100.0 + 50.0 + (50.0 - 30.0));
        assert!(v.contains(&Seg { a: (40.0, 0.0), b: (100.0, 0.0) }), "{v:?}");

        // Full cover: nothing of the lower window.
        let v = visible_edges(&[rect(-10.0, -10.0, 200.0, 200.0), low]);
        assert_eq!(total(&v), 2.0 * 210.0 + 2.0 * 210.0);

        // Touching: the lower window's right edge lies on the upper's left edge, so it counts
        // once (as the upper's); its other sides are untouched.
        let v = visible_edges(&[rect(100.0, 0.0, 200.0, 50.0), low]);
        assert_eq!(total(&v), 300.0 + 300.0 - 50.0);

        // Overlapping, three deep: the middle window loses what the top covers, the bottom
        // what either covers.
        let v = visible_edges(&[rect(0.0, 0.0, 10.0, 10.0), rect(5.0, 5.0, 15.0, 15.0), rect(10.0, 10.0, 20.0, 20.0)]);
        assert_eq!(total(&v), 40.0 + (40.0 - 10.0) + (40.0 - 10.0));

        // Maximised: it covers the work area; the windows below it contribute nothing, and its
        // own sides lie exactly on the border.
        let work = rect(0.0, 0.0, 1920.0, 1040.0);
        let v = visible_edges(&[work, rect(100.0, 100.0, 500.0, 400.0), rect(0.0, 500.0, 1920.0, 1040.0)]);
        assert_eq!(total(&v), 2.0 * 1920.0 + 2.0 * 1040.0);
    }

    #[test]
    fn border_skips_where_monitors_touch() {
        let one = [rect(0.0, 0.0, 1920.0, 1040.0)];
        assert_eq!(total(&border(&one)), 2.0 * 1920.0 + 2.0 * 1040.0);
        // Side by side, the second one taller: the shared stretch of x = 1920 isn't a border,
        // the part of the second monitor's left side below the first one is.
        let two = [rect(0.0, 0.0, 1920.0, 1040.0), rect(1920.0, 0.0, 3840.0, 1200.0)];
        let b = border(&two);
        assert_eq!(total(&b), 2.0 * 3840.0 + 1040.0 + 1200.0 + (1200.0 - 1040.0));
    }

    /// Adaptation curve: one tau of contact takes the gain 63% of the way to the floor, ten all
    /// but settle it there; one recovery tau without contact brings it 63% of the way back.
    #[test]
    fn adaptation_curve() {
        let mut a = Adapt::new();
        let run = |a: &mut Adapt, raw: (f32, f32), ms: u32| (0..ms / 16).for_each(|_| _ = a.step(raw, 16.0));
        run(&mut a, (0.5, 0.0), ADAPT_MS as u32);
        let expect = ADAPT_FLOOR + (1.0 - ADAPT_FLOOR) * (-1.0f32).exp();
        assert!((a.gain.0 - expect).abs() < 0.01, "after one tau: {} vs {expect}", a.gain.0);
        assert_eq!(a.gain.1, 1.0, "the untouched side doesn't adapt");
        run(&mut a, (0.5, 0.0), 9 * ADAPT_MS as u32);
        assert!((a.gain.0 - ADAPT_FLOOR).abs() < 0.01, "after ten tau: {}", a.gain.0);
        assert!((a.step((0.5, 0.0), 0.0).0 - 0.5 * a.gain.0).abs() < 1e-6, "output = raw x gain");
        run(&mut a, (0.0, 0.0), RECOVER_MS as u32);
        let back = 1.0 - (1.0 - ADAPT_FLOOR) * (-1.0f32).exp();
        assert!((a.gain.0 - back).abs() < 0.02, "one recovery tau later: {} vs {back}", a.gain.0);
    }

    #[test]
    fn window_edges_clip_to_work_area() {
        // A frame poking 2 px below the work area into the taskbar strip: its bottom side goes,
        // its left and right sides stop at the work area's edge.
        let work = [rect(0.0, 0.0, 1920.0, 1020.0)];
        let segs = feelable(&[rect(229.0, 500.0, 1772.0, 1022.0)], &work);
        let windows = &segs[..segs.len() - 4]; // the rest is the border
        assert_eq!(total(windows), (1772.0 - 229.0) + 2.0 * 520.0, "{windows:?}");
        assert!(windows.iter().all(|s| s.a.1 <= 1020.0 && s.b.1 <= 1020.0));
    }

    #[test]
    fn contact_sides_reach_and_behind() {
        let reach = 50.0;
        let feel = |heading: f32, segs: &[Seg]| contact((0.0, 0.0), (0.0, 0.0), heading, reach, segs);
        // Fly at the origin facing +x; an edge along y = -20 is on its left (y down).
        let edge = [Seg { a: (-500.0, -20.0), b: (500.0, -20.0) }];
        let (l, r) = feel(0.0, &edge);
        assert!(l > 0.3 && r == 0.0, "left edge: ({l}, {r})");
        let (l, r) = feel(std::f32::consts::PI, &edge);
        assert!(r > 0.3 && l == 0.0, "turned around, the edge is on the right: ({l}, {r})");
        // Beyond reach: nothing.
        assert_eq!(feel(0.0, &[Seg { a: (-500.0, -60.0), b: (500.0, -60.0) }]), (0.0, 0.0));
        // The same short wall ahead-left vs behind-left: weaker behind.
        let ahead = feel(0.0, &[Seg { a: (35.0, -10.0), b: (45.0, -10.0) }]).0;
        let behind = feel(0.0, &[Seg { a: (-45.0, -10.0), b: (-35.0, -10.0) }]).0;
        assert!(behind < 0.5 * ahead && behind > 0.0, "ahead {ahead}, behind {behind}");
        // Continuous sides: a wall dead ahead feeds neither side much, and turning a little
        // either way changes the split smoothly instead of flipping it.
        let wall = [Seg { a: (20.0, -500.0), b: (20.0, 500.0) }];
        let (l0, r0) = feel(0.0, &wall);
        assert!(l0 < 0.05 && r0 < 0.05, "dead ahead: ({l0}, {r0})");
        let (l1, r1) = feel(0.05, &wall);
        let (l2, r2) = feel(-0.05, &wall);
        assert!((l1 - r2).abs() < 1e-5 && (r1 - l2).abs() < 1e-5 && l1.max(r1) < 0.1);
        // Smooth: no jump near the edge of reach.
        let at = |d: f32| feel(0.0, &[Seg { a: (-500.0, -d), b: (500.0, -d) }]).0;
        assert!(at(49.0) < 0.01 && (at(30.0) - at(30.5)).abs() < 0.02);
        // Head ahead of the thorax, already across a line the thorax hasn't reached (heading
        // -80 deg, line at y = -5): judged from the head the line would be straight behind it
        // and vanish; judged from the thorax it's ahead and a little left, so it still counts,
        // and being nearly straight across the path, it feeds that side only a little.
        let line = [Seg { a: (-500.0, -5.0), b: (500.0, -5.0) }];
        let (l, r) = contact((0.0, 0.0), (0.0, -10.0), -1.4, reach, &line);
        assert!(l > 0.05 && l < 0.3 && r == 0.0, "({l}, {r})");
    }
}
