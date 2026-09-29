// Without this, Windows gives a GUI-less process a console window that flashes up on every launch.
#![windows_subsystem = "windows"]

mod art;
mod body;
mod fly;
mod path;
mod snapshot;
mod trace;
mod vision;
mod world;

use body::Body;
use flit::brain::{Brain, Pack};
use fly::{Feet, FlyPose};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tiny_skia::{Color, Pixmap};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem};
use tray_icon::{Icon, TrayIconBuilder};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION,
    CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, HBITMAP, HDC,
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTONULL, MONITORINFO, MonitorFromPoint,
    SelectObject,
};
use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetSystemMetrics,
    PM_REMOVE, PeekMessageW, RegisterClassW, SM_CXSCREEN, SM_CYSCREEN, SW_SHOWNOACTIVATE,
    SetWindowDisplayAffinity, ShowWindow, TranslateMessage, ULW_ALPHA, WDA_EXCLUDEFROMCAPTURE, WDA_NONE, UpdateLayeredWindow, WM_DPICHANGED, WM_QUIT,
    WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_EX_TRANSPARENT, WS_POPUP,
};
use windows::core::w;

const FRAME: Duration = Duration::from_micros(16_667); // ~60 Hz

/// Set by the window procedure when Windows says our monitor's scaling changed; the main loop
/// notices and rebuilds the window-sized buffers. (A window procedure is a bare `extern fn`
/// with no access to our locals, so a global flag is the simplest way to talk to the loop.)
static DPI_CHANGED: AtomicBool = AtomicBool::new(false);
/// Recordable (`--recordable`, tray): the overlay shows up in captures, so vision is off. Read by
/// the `--debug` line.
static RECORDABLE: AtomicBool = AtomicBool::new(false);

/// This process's CPU time (all threads, kernel + user), s.
fn process_cpu_s() -> f32 {
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    let mut t = [Default::default(); 4];
    // SAFETY: four FILETIME out-params, all locals; the pseudo-handle needs no closing.
    let _ = unsafe { GetProcessTimes(GetCurrentProcess(), &mut t[0], &mut t[1], &mut t[2], &mut t[3]) };
    let s = |f: windows::Win32::Foundation::FILETIME| ((f.dwHighDateTime as u64) << 32 | f.dwLowDateTime as u64) as f64 / 1e7;
    (s(t[2]) + s(t[3])) as f32
}

/// `--debug`: process CPU time / wall time over the first `CpuMeter::SECS` of running (pauses
/// skipped), as % of ONE core. Task Manager shows % of the whole machine.
#[derive(Default)]
struct CpuMeter {
    /// (CPU s, wall s) so far, and the current running stretch's start.
    acc: (f32, f32),
    base: Option<(f32, Instant)>,
}

impl CpuMeter {
    const SECS: f32 = 60.0;

    fn start(&mut self) {
        if self.acc.1 < Self::SECS && self.base.is_none() {
            self.base = Some((process_cpu_s(), Instant::now()));
        }
    }

    fn stop(&mut self) {
        if let Some((c, t)) = self.base.take() {
            self.acc = (self.acc.0 + process_cpu_s() - c, self.acc.1 + t.elapsed().as_secs_f32());
        }
    }

    /// Stops once the window is full.
    fn check(&mut self) {
        if self.base.is_some_and(|(_, t)| self.acc.1 + t.elapsed().as_secs_f32() >= Self::SECS) {
            self.stop();
        }
    }

    fn report(&mut self) -> String {
        self.stop();
        let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
        let one = 100.0 * self.acc.0 / self.acc.1.max(1e-3);
        format!("cpu: {one:.1}% of one core over {:.0} s running; {cores} logical cores = {:.2}% of the machine", self.acc.1, one / cores as f32)
    }
}

/// Excluded from capture (the default: hidden from recorders and screen sharing, and vision can
/// work) or recordable. Returns whether vision may run.
fn set_recordable(hwnd: HWND, on: bool) -> bool {
    RECORDABLE.store(on, Ordering::Relaxed);
    // SAFETY: plain FFI call on our own live window.
    match unsafe { SetWindowDisplayAffinity(hwnd, if on { WDA_NONE } else { WDA_EXCLUDEFROMCAPTURE }) } {
        Ok(()) => !on,
        Err(e) => {
            // Refused exclusion: no fallback, a screen BitBlt shows layered windows with or
            // without CAPTUREBLT (see `self_exclusion`), so the fly goes blind rather than
            // follow its own outline.
            eprintln!("flit: display affinity refused ({e}); vision off");
            false
        }
    }
}

/// Every window needs a window procedure. We draw with `UpdateLayeredWindow`, so WM_PAINT never
/// arrives; quitting comes from the tray. The only message we care about is WM_DPICHANGED.
unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_DPICHANGED {
        DPI_CHANGED.store(true, Ordering::Relaxed);
        return LRESULT(0);
    }
    // SAFETY: forwards Windows' own arguments untouched to Windows' default handler.
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

/// The fly's own clock: seconds of *running* time only. `tick` returns the time since the
/// previous tick, but 0 while paused, and `set_paused` re-bases the reference point, so
/// however long a pause lasts, Resume continues from exactly where the fly stopped.
struct Clock {
    last: Instant,
    paused: bool,
}

impl Clock {
    /// Longest step we'll ever report. The tray menu is modal and blocks the loop while it's
    /// open, so a stall must not teleport the fly.
    const MAX_DT: f32 = 0.1;

    fn tick(&mut self, now: Instant) -> f32 {
        let dt = if self.paused { 0.0 } else { now.duration_since(self.last).as_secs_f32() };
        self.last = now;
        dt.min(Self::MAX_DT)
    }

    fn set_paused(&mut self, paused: bool, now: Instant) {
        self.paused = paused;
        self.last = now; // time up to here is settled; nothing before it is counted later
    }
}

/// tiny-skia produces premultiplied RGBA bytes; a Windows DIB wants premultiplied BGRA.
/// Same numbers, red and blue swapped. Skip this and the fly comes out blue-eyed and bluish.
fn rgba_to_bgra(src: &[u8], dst: &mut [u8]) {
    for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
        d.copy_from_slice(&[s[2], s[1], s[0], s[3]]);
    }
}

/// The window's pixels: a `size` x `size` 32-bit top-down DIB section (a bitmap whose memory we
/// can write directly) selected into a memory DC, plus a tiny-skia pixmap we draw into first.
struct Canvas {
    size: i32,
    pixmap: Pixmap,
    dc: HDC,
    bmp: HBITMAP,
    bits: *mut u8,
    /// (window x, window y, heading, gait phase, whiskers) of the frame on screen. Same again =
    /// nothing to do.
    shown: Option<[f32; 8]>,
}

impl Canvas {
    fn new(scale: f32) -> Self {
        let size = fly::window_size(scale);
        // Negative biHeight = top-down (row 0 is the top), matching tiny-skia's row order.
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: size,
                biHeight: -size,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        // SAFETY: `bmi` and `bits` outlive the call. On success Windows sets `bits` to
        // size*size*4 bytes of pixel memory that lives until we `DeleteObject` the bitmap
        // (in `Drop`), and we never touch it after that.
        let (dc, bmp) = unsafe {
            let dc = CreateCompatibleDC(None); // a memory DC to select the bitmap into
            let bmp = CreateDIBSection(Some(dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0).unwrap();
            SelectObject(dc, bmp.into());
            (dc, bmp)
        };
        Canvas {
            size,
            pixmap: Pixmap::new(size as u32, size as u32).unwrap(),
            dc,
            bmp,
            bits: bits as *mut u8,
            shown: None,
        }
    }

    /// Draws `pose` and pushes it to the screen, moving and (if needed) resizing the window.
    /// Skips both if the fly looks exactly as it did last time (e.g. while it stands still).
    /// `whiskers`: `--debug`'s contact dots, (contact, vision won) per side.
    fn present(&mut self, hwnd: HWND, pose: &FlyPose, feet: &Feet, scale: f32, whiskers: Option<[(f32, bool); 2]>) {
        // The window moves in whole art pixels and the fly is drawn at its centre: a fractional
        // offset inside the pixmap would make non-AA edges flicker (see `art`).
        let origin = art::window_origin(pose, scale, self.size);
        // Feet follow from these: they only move when the body moves or the gait advances.
        let w = whiskers.unwrap_or_default();
        let key = [origin.0 as f32, origin.1 as f32, pose.heading, pose.gait_phase, w[0].0, w[1].0, w[0].1 as u8 as f32, w[1].1 as u8 as f32];
        if self.shown == Some(key) {
            return;
        }
        self.shown = Some(key);
        self.pixmap.fill(Color::TRANSPARENT);
        art::draw(pose, feet, scale, &mut self.pixmap);
        if let Some(w) = whiskers {
            // Cyan: geometry won that side; magenta: vision did. Neither is in the fly's palette.
            let tint = |(c, vision): (f32, bool)| (c, if vision { [255, 0, 255] } else { [0, 255, 255] });
            art::whiskers(pose.heading, scale, w.map(tint), &mut self.pixmap);
        }

        // SAFETY: `bits` points to size*size*4 bytes (see `new`), and `&mut self` means nothing
        // else is touching them. Every pointer handed to UpdateLayeredWindow refers to a local
        // that outlives the call; hwnd and dc are live.
        unsafe {
            let dib = std::slice::from_raw_parts_mut(self.bits, (self.size * self.size * 4) as usize);
            rgba_to_bgra(self.pixmap.data(), dib);

            // Use the bitmap's per-pixel alpha (AC_SRC_ALPHA, which expects premultiplied
            // colour), with no extra global fade (255 = fully opaque overall).
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: 255,
                AlphaFormat: AC_SRC_ALPHA as u8,
            };
            // Draws the bitmap AND positions/sizes the window in one call (the only way to
            // show a layered window's content; WM_PAINT is ignored for it). A failure here
            // (e.g. while the session is locked) shouldn't kill the pet: skip this frame.
            let _ = UpdateLayeredWindow(
                hwnd,
                None,                                              // destination DC: the screen
                Some(&POINT { x: origin.0, y: origin.1 }),         // window's top-left
                Some(&SIZE { cx: self.size, cy: self.size }),      // window size
                Some(self.dc),                                     // source bitmap
                Some(&POINT { x: 0, y: 0 }),                       // from the bitmap's top-left
                COLORREF(0),                                       // colour key: unused here
                Some(&blend),
                ULW_ALPHA,
            );
        }
    }
}

impl Drop for Canvas {
    fn drop(&mut self) {
        // SAFETY: we own both handles and nothing uses them afterwards. The bitmap can only
        // be deleted once it is no longer selected into a DC, so delete the DC first.
        unsafe {
            let _ = DeleteDC(self.dc);
            let _ = DeleteObject(self.bmp.into());
        }
    }
}

/// Display scale of the monitor the window is on: 1.0 at 100%, 1.5 at 150%, 2.0 at 200%.
fn dpi_scale(hwnd: HWND) -> f32 {
    // SAFETY: plain FFI getter on our own window. Returns 0 on failure; fall back to 96 (100%).
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    (if dpi == 0 { 96 } else { dpi }) as f32 / 96.0
}

/// Centre of the primary monitor in physical pixels (valid because we're DPI aware).
fn screen_center() -> (f32, f32) {
    // SAFETY: trivial FFI getters.
    unsafe { (GetSystemMetrics(SM_CXSCREEN) as f32 / 2.0, GetSystemMetrics(SM_CYSCREEN) as f32 / 2.0) }
}

/// Work area (monitor minus taskbar) of the monitor under `(x, y)`, or with `nearest`, of the
/// monitor closest to it.
fn work_area(x: f32, y: f32, nearest: bool) -> Option<RECT> {
    let flags = if nearest { MONITOR_DEFAULTTONEAREST } else { MONITOR_DEFAULTTONULL };
    let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
    // SAFETY: plain FFI getters; `mi` is a valid, sized out-param.
    let ok = unsafe {
        let m = MonitorFromPoint(POINT { x: x.floor() as i32, y: y.floor() as i32 }, flags);
        !m.is_invalid() && GetMonitorInfoW(m, &mut mi).as_bool()
    };
    ok.then_some(mi.rcWork)
}

/// Is this screen point on some monitor's work area?
fn on_work_area(x: f32, y: f32) -> bool {
    work_area(x, y, false)
        .is_some_and(|r| x >= r.left as f32 && x < r.right as f32 && y >= r.top as f32 && y < r.bottom as f32)
}

/// Centre of the work area nearest to `(x, y)`.
fn work_area_centre(x: f32, y: f32) -> (f32, f32) {
    let r = work_area(x, y, true).unwrap_or_default();
    ((r.left + r.right) as f32 / 2.0, (r.top + r.bottom) as f32 / 2.0)
}

/// What moves the fly: the brain, or (`--demo-path`) milestone 2's figure-eight.
enum Driver {
    Brain {
        brain: Brain,
        body: Body,
        /// Brain time owed to the clock, ms (< 1 after each frame).
        owed_ms: f32,
        /// `--debug`: (brain ms, spikes) since the last print, prints so far.
        debug: Option<(u32, usize, u32)>,
        /// Window edges and the work-area border, as of the last poll.
        world: world::World,
    },
    Demo(path::Walker, path::Step),
}

impl Driver {
    /// Longest brain catch-up in one frame. A long stall (sleep, a stuck frame) must not make
    /// one frame simulate seconds of brain; the excess is dropped.
    const MAX_STEPS: u32 = 50;

    fn advance(&mut self, dt: f32, scale: f32) {
        match self {
            Driver::Brain { brain, body, owed_ms, debug, world } => {
                *owed_ms += dt * 1000.0;
                let steps = (*owed_ms as u32).min(Self::MAX_STEPS);
                *owed_ms = (*owed_ms - steps as f32).min(0.999);
                let spikes = body.tick(brain, steps, scale, &world.segs, &world.look.pts, on_work_area, work_area_centre);
                if let Some((ms, n, prints)) = debug {
                    (*ms, *n) = (*ms + steps, *n + spikes);
                    if *ms >= 500 {
                        let (angle, mag) = brain.bump();
                        println!(
                            "turn {:+7.1}  fwd {:6.1}  bump {:+5.0} deg  mag {:.2}  {:5.0} spikes/s  at {:.0} {:.0} hd {:+.0}",
                            brain.turn_command(),
                            brain.forward_command(),
                            angle.to_degrees(),
                            mag,
                            *n as f32 * 1000.0 / *ms as f32,
                            body.x,
                            body.y,
                            body.heading.to_degrees()
                        );
                        (*ms, *n, *prints) = (0, 0, *prints + 1);
                        if *prints % 2 == 0 {
                            println!(
                                "  world: {} windows, {} segments, poll {:.2} ms, vision: {}, vis L {:.2} R {:.2}, contact L {:.2} R {:.2}, adapt L {:.2} R {:.2}",
                                world.rects.len(),
                                world.segs.len(),
                                world.poll_ms,
                                if RECORDABLE.load(Ordering::Relaxed) { "off (recordable)".to_owned() } else { format!("{:.2} ms", world.look.ms) },
                                body.vis.0,
                                body.vis.1,
                                body.contact.0,
                                body.contact.1,
                                body.adapt.gain.0,
                                body.adapt.gain.1
                            );
                        }
                    }
                }
            }
            Driver::Demo(walker, step) => *step = walker.step(dt),
        }
    }

    /// `--debug` whiskers: adapted contact per side, and whether vision beat geometry there.
    fn whiskers(&self) -> Option<[(f32, bool); 2]> {
        match self {
            Driver::Brain { body: b, debug: Some(_), .. } => Some([(b.contact.0, b.vis.0 > b.geo.0), (b.contact.1, b.vis.1 > b.geo.1)]),
            _ => None,
        }
    }

    fn pose(&self, scale: f32) -> FlyPose {
        match self {
            Driver::Brain { body, .. } => body.pose(scale),
            Driver::Demo(_, step) => to_pose(step, scale, screen_center()),
        }
    }
}

/// Converts the walker's logical-pixel output (96 dpi, relative to the route's centre) into a
/// pose in physical screen pixels.
fn to_pose(s: &path::Step, scale: f32, center: (f32, f32)) -> FlyPose {
    FlyPose {
        x: center.0 + s.x * scale,
        y: center.1 + s.y * scale,
        heading: s.heading,
        speed: s.speed * scale,
        gait_phase: s.gait_phase,
    }
}

/// The overlay: a click-through, topmost, layered popup, not yet shown.
fn overlay() -> HWND {
    // SAFETY: every call below is Win32 FFI. Handles come from the calls that create them and
    // are used only on this thread, while still alive.
    unsafe {
        let hinstance = GetModuleHandleW(None).unwrap().into();
        let class = w!("flit");
        RegisterClassW(&WNDCLASSW {
            lpfnWndProc: Some(wnd_proc),
            hInstance: hinstance,
            lpszClassName: class,
            ..Default::default()
        });
        CreateWindowExW(
            // Extended styles, one job each:
            WS_EX_LAYERED       // per-pixel alpha via UpdateLayeredWindow (soft edges, any shape)
            | WS_EX_TRANSPARENT // mouse hit-testing skips us: clicks fall through to the window below
            | WS_EX_TOPMOST     // sits above normal windows
            | WS_EX_NOACTIVATE  // clicking/showing us never takes keyboard focus
            | WS_EX_TOOLWINDOW, // hidden from Alt-Tab and the taskbar
            class,
            w!("flit"),
            WS_POPUP, // no title bar, border or menu: the window is exactly our pixels
            0,
            0,
            1,
            1, // real size is set by UpdateLayeredWindow once we know the DPI
            None,
            None,
            Some(hinstance),
            None,
        )
        .unwrap()
    }
}

fn main() {
    // `flit --snapshot <dir>`: render the reference images and exit, no window. A windows-
    // subsystem exe has no console of its own, so borrow the parent's to be able to print.
    let args: Vec<String> = std::env::args().collect();
    let flag = |f: &str| args.iter().any(|a| a == f);
    if flag("--debug") || flag("--pack") || flag("--walk-speed") || flag("--dump-world") || flag("--dump-vision") || flag("--trace-map") || flag("--recordable") {
        // SAFETY: plain FFI call; failing just means there is no parent console to print to.
        let _ = unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
    }
    if args.get(1).map(String::as_str) == Some("--snapshot") {
        // SAFETY: plain FFI call; failing just means there is no parent console to print to.
        let _ = unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
        match snapshot::run(std::path::Path::new(args.get(2).map(String::as_str).unwrap_or("."))) {
            Ok(()) => println!("snapshots written"),
            Err(e) => println!("snapshot failed: {e}"),
        }
        return;
    }

    let pack = match args.iter().position(|a| a == "--pack") {
        Some(i) => match args.get(i + 1).ok_or("--pack needs a path".to_owned()).and_then(|p| Pack::load(p.as_ref())) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("flit: {e}");
                std::process::exit(1);
            }
        },
        None => Pack::parse(flit::brain::STUB).expect("embedded pack"),
    };

    let walk = match args.iter().position(|a| a == "--walk-speed") {
        None => fly::WALK_SPEED,
        Some(i) => match args.get(i + 1).and_then(|v| v.parse::<f32>().ok()) {
            Some(v) if (0.3..=1.5).contains(&v) => v,
            _ => {
                eprintln!("flit: --walk-speed needs a number from 0.3 to 1.5 (default {})", fly::WALK_SPEED);
                std::process::exit(1);
            }
        },
    };

    // SAFETY: plain FFI call with a constant. Must be the FIRST thing we do, before any window
    // exists; otherwise Windows "virtualises" our coordinates on scaled displays (125%, 150%...)
    // and the window lands in the wrong place. PER_MONITOR_AWARE_V2 = we get real pixels and are
    // told (WM_DPICHANGED) when the window crosses to a monitor with a different scale.
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) }
        .expect("SetProcessDpiAwarenessContext");

    // `--dump-world <png>`: what the fly would feel right now, drawn, then exit. After the DPI
    // call, so rects are in physical pixels like everything else.
    if let Some(i) = args.iter().position(|a| a == "--dump-world") {
        let Some(path) = args.get(i + 1) else {
            eprintln!("flit: --dump-world needs a .png path");
            std::process::exit(1);
        };
        let w = world::poll(HWND::default());
        println!("{} windows, {} segments, poll {:.2} ms", w.rects.len(), w.segs.len(), w.poll_ms);
        for (r, h) in w.rects.iter().zip(&w.hwnds) {
            println!("  window {:6} {:6} {:6} {:6}  {}", r.l, r.t, r.r, r.b, world::describe(*h));
        }
        for s in &w.segs {
            println!("  seg ({:6}, {:6}) - ({:6}, {:6})", s.a.0, s.a.1, s.b.0, s.b.1);
        }
        if let Err(e) = world::dump(&w, path.as_ref()) {
            eprintln!("flit: {e}");
            std::process::exit(1);
        }
        return;
    }

    // Tray icon first: it's our only way out. tray-icon makes its own hidden window on this
    // thread, so it works as long as our loop below keeps dispatching messages.
    let pause = MenuItem::new("Pause", true, None);
    let recordable = CheckMenuItem::new("Recordable", true, flag("--recordable"), None);
    let quit = MenuItem::new("Quit", true, None);
    let menu = Menu::new();
    menu.append(&pause).unwrap();
    menu.append(&recordable).unwrap();
    menu.append(&quit).unwrap();
    let icon = Icon::from_rgba([0x57, 0x46, 0x2c, 255].repeat(32 * 32), 32, 32).unwrap();
    let _tray = TrayIconBuilder::new() // dropped at end of main => icon removed, no ghost icon
        .with_menu(Box::new(menu))
        .with_tooltip("flit")
        .with_icon(icon)
        .build()
        .unwrap();

    let hwnd = overlay();
    // The fly must not see itself (`vision`): excluded from capture unless recordable.
    let mut sees = set_recordable(hwnd, recordable.is_checked());
    // `--trace-map <png> [--minutes N]`: run N minutes (running time), then draw the map and exit.
    let trace_map = args.iter().position(|a| a == "--trace-map").map(|i| args.get(i + 1).cloned().unwrap_or_else(|| {
        eprintln!("flit: --trace-map needs a .png path");
        std::process::exit(1);
    }));
    let minutes = match args.iter().position(|a| a == "--minutes") {
        None => 3.0,
        Some(i) => match args.get(i + 1).and_then(|v| v.parse::<f32>().ok()) {
            Some(v) if v > 0.0 => v,
            _ => {
                eprintln!("flit: --minutes needs a positive number");
                std::process::exit(1);
            }
        },
    };
    let mut trace = trace_map.is_some().then(trace::Trace::default);
    let dump_vision = args.iter().position(|a| a == "--dump-vision").map(|i| {
        let dir = std::path::PathBuf::from(args.get(i + 1).map(String::as_str).unwrap_or("."));
        let _ = std::fs::create_dir_all(&dir);
        dir
    });

    let mut scale = dpi_scale(hwnd);
    let mut canvas = Canvas::new(scale);
    let mut driver = if flag("--demo-path") {
        let mut walker = path::Walker::new();
        walker.walk = walk as f64;
        let step = walker.step(0.0);
        Driver::Demo(walker, step)
    } else {
        let mut brain = Brain::new(pack, 1);
        brain.warmup(3, 16);
        let debug = flag("--debug").then_some((0, 0, 0));
        let mut body = Body::new(work_area_centre(0.0, 0.0));
        body.walk = walk;
        Driver::Brain { brain, body, owed_ms: 0.0, debug, world: world::poll(hwnd) }
    };
    let mut pose = driver.pose(scale);
    let mut feet = Feet::new(&pose, scale);
    canvas.present(hwnd, &pose, &feet, scale, driver.whiskers());
    // SW_SHOWNOACTIVATE: show without activating. Plain SW_SHOW would steal focus.
    // SAFETY: hwnd is a live window we created.
    let _ = unsafe { ShowWindow(hwnd, SW_SHOWNOACTIVATE) };

    // Fixed-step loop: drain messages, render when a frame is due, otherwise sleep until it is.
    // No WM_TIMER. Messages (the tray) wait at most one frame. The 1 kHz brain (milestone 3)
    // steps in catch-up batches inside the frame (~16 steps at 60 Hz, sized from elapsed time),
    // so it needs no extra wake-ups: a 1 ms sleep here would cost ~1% of a core at rest.
    let mut clock = Clock { last: Instant::now(), paused: false };
    let mut next_frame = clock.last;
    // The desktop is polled at 10 Hz, not every frame: windows move slowly next to a fly.
    const POLL: Duration = Duration::from_millis(100);
    let mut next_poll = clock.last + POLL;
    let (started, mut dumped) = (clock.last, 0);
    let mut eye = sees.then(vision::Eye::new);
    // After exclusion is restored, vision waits this long (the compositor takes a frame or so).
    let mut see_from = clock.last;
    let mut cpu = CpuMeter::default();
    cpu.start();
    let mut msg = Default::default();
    'main: loop {
        // SAFETY: `msg` is a valid MSG out-param; PeekMessage (non-blocking) fills it.
        while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) }.as_bool() {
            if msg.message == WM_QUIT {
                break 'main;
            }
            // SAFETY: msg was just produced by PeekMessage. Dispatching is what lets the
            // tray icon's hidden window receive its messages.
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        for e in MenuEvent::receiver().try_iter() {
            if e.id == *quit.id() {
                break 'main;
            }
            if e.id == *pause.id() {
                let paused = !clock.paused;
                clock.set_paused(paused, Instant::now());
                pause.set_text(if paused { "Resume" } else { "Pause" });
                if paused { cpu.stop() } else { cpu.start() }
            }
            if e.id == *recordable.id() {
                // Vision off before the fly becomes capturable; excluded again before it's back on.
                eye = None; // ends the thread; a look in flight is thrown away with it
                if let Driver::Brain { world, .. } = &mut driver {
                    world.look = Default::default();
                }
                sees = set_recordable(hwnd, recordable.is_checked());
                see_from = Instant::now() + Duration::from_millis(500);
            }
        }

        // The fly is drawn at a physical size, so a new scale means a new window size and
        // new buffers. Only the buffers: while paused nothing is drawn or pushed to the screen,
        // so the window keeps its old picture until Resume, whose first frame redraws it.
        let mut redraw = DPI_CHANGED.swap(false, Ordering::Relaxed);
        if redraw {
            scale = dpi_scale(hwnd);
            canvas = Canvas::new(scale);
            pose = driver.pose(scale);
            feet = Feet::new(&pose, scale); // feet are pinned in screen pixels: re-pin at the new scale
        }

        if clock.paused {
            // No stepping, no drawing, no UpdateLayeredWindow: just keep pumping messages
            // (the loop above) a few times a second so the tray still responds.
            std::thread::sleep(Duration::from_millis(30));
            continue;
        }

        let now = Instant::now();
        if now >= next_poll {
            if let Driver::Brain { world, body, .. } = &mut driver {
                cpu.check();
                if sees && eye.is_none() && now >= see_from {
                    eye = Some(vision::Eye::new());
                }
                let look = std::mem::take(&mut world.look);
                *world = world::poll(hwnd);
                world.look = eye.as_mut().and_then(|e| e.poll(body.head(scale), scale)).unwrap_or(look);
                if let Some(t) = &mut trace {
                    t.poll(world);
                    if t.secs() >= minutes * 60.0 {
                        break 'main;
                    }
                }
                if let Some(dir) = &dump_vision {
                    // Once a second for 10 s, then exit.
                    let secs = now.duration_since(started).as_secs() as u32;
                    if secs >= 10 {
                        break 'main;
                    }
                    if secs >= dumped {
                        dumped = secs + 1;
                        let line = format!(
                            "{secs} origin {:?} cell {} px, {} edge points, {:.2} ms, vis L {:.2} R {:.2}, contact L {:.2} R {:.2}
",
                            world.look.origin, world.look.cell, world.look.pts.len(), world.look.ms, body.vis.0, body.vis.1, body.contact.0, body.contact.1
                        );
                        print!("{line}");
                        let text = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("vision.txt"));
                        if let Err(e) = text.and_then(|mut f| std::io::Write::write_all(&mut f, line.as_bytes())) {
                            eprintln!("flit: {e}");
                        }
                        if let Err(e) = vision::dump(&world.look, dir, secs) {
                            eprintln!("flit: {e}");
                        }
                    }
                }
            }
            next_poll = now + POLL;
        }
        if now >= next_frame {
            // Motion is driven by real elapsed running time, not by counting frames, so a
            // dropped frame doesn't change the fly's speed (see `Clock`).
            let dt = clock.tick(now);
            driver.advance(dt, scale);
            if let (Some(t), Driver::Brain { body: b, .. }) = (&mut trace, &driver) {
                t.frame(b.x, b.y, b.geo.0.max(b.vis.0).max(b.geo.1.max(b.vis.1)), dt);
            }
            pose = driver.pose(scale);
            feet.update(&pose, scale);
            redraw = true;
            next_frame += FRAME;
            if next_frame < now {
                next_frame = now + FRAME; // fell behind: don't burst to catch up
            }
        } else if !redraw {
            std::thread::sleep(next_frame - now);
        }
        if redraw {
            canvas.present(hwnd, &pose, &feet, scale, driver.whiskers());
        }
    }
    if flag("--debug") {
        println!("{}", cpu.report());
    }
    if let (Some(t), Some(path)) = (&trace, &trace_map) {
        println!("{}", t.legend());
        if let Err(e) = t.draw().and_then(|pm| pm.save_png(path).map_err(|e| e.to_string())) {
            eprintln!("flit: {e}");
        }
    }
    // SAFETY: hwnd is ours and still valid. Then `_tray` drops, removing the tray icon.
    let _ = unsafe { DestroyWindow(hwnd) };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pause for 10 s (simulated timeline, no sleeping): after Resume the fly must be exactly
    /// where a fly that never paused is after the same amount of *running* time.
    #[test]
    fn pause_loses_no_time_and_adds_none() {
        let t0 = Instant::now();
        let frame = Duration::from_micros(16_667);
        // What the real loop does each due frame: tick the clock, step the walker.
        let frames = |clock: &mut Clock, fly: &mut path::Walker, now: &mut Instant, n: u32| {
            for _ in 0..n {
                *now += frame;
                fly.step(clock.tick(*now));
            }
        };

        let (mut clock, mut fly, mut now) = (Clock { last: t0, paused: false }, path::Walker::new(), t0);
        frames(&mut clock, &mut fly, &mut now, 300);
        clock.set_paused(true, now);
        now += Duration::from_secs(10); // paused: the real loop skips tick() entirely
        clock.set_paused(false, now);
        now += frame;
        let first = clock.tick(now);
        assert!((first - 0.016667).abs() < 1e-4, "first frame after Resume was {first}s");
        fly.step(first);
        frames(&mut clock, &mut fly, &mut now, 299);

        let (mut steady_clock, mut steady, mut steady_now) =
            (Clock { last: t0, paused: false }, path::Walker::new(), t0);
        frames(&mut steady_clock, &mut steady, &mut steady_now, 600);

        let (a, b) = (fly.step(0.0), steady.step(0.0));
        assert!((a.x - b.x).abs() < 0.01 && (a.y - b.y).abs() < 0.01, "fly jumped ahead");
    }

    fn brain_driver() -> Driver {
        let mut brain = Brain::new(Pack::parse(flit::brain::STUB).unwrap(), 1);
        brain.warmup(3, 16);
        Driver::Brain { brain, body: Body::new(work_area_centre(0.0, 0.0)), owed_ms: 0.0, debug: None, world: world::World::default() }
    }

    /// Five simulated minutes on this machine's real monitors at 60 Hz: the pose stays finite
    /// and on a work area. Then a 30 s pause: the first frame after Resume moves the fly by at
    /// most one frame's worth, and a 10 s stall costs at most `MAX_STEPS` of brain time.
    /// Prints what the fly did, for eyeballing.
    #[test]
    fn brain_drives_cleanly_through_pause_and_stall() {
        let scale = 1.25;
        let (mut d, t0) = (brain_driver(), Instant::now());
        let (mut clock, mut now) = (Clock { last: t0, paused: false }, t0);
        let frame = Duration::from_micros(16_667);
        let (mut walked, mut turned, mut still, mut bounces) = (0.0_f32, 0.0_f32, 0, 0);
        let mut prev = d.pose(scale);
        for _ in 0..60 * 300 {
            now += frame;
            d.advance(clock.tick(now), scale);
            let p = d.pose(scale);
            assert!(p.x.is_finite() && p.y.is_finite() && p.heading.is_finite() && p.gait_phase.is_finite());
            assert!(on_work_area(p.x, p.y), "fly left the work area at ({}, {})", p.x, p.y);
            let step = (p.x - prev.x).hypot(p.y - prev.y);
            walked += step;
            still += (step < 0.05 * scale) as u32;
            let dh = p.heading - prev.heading;
            let dh = dh.sin().atan2(dh.cos());
            turned += dh.abs();
            bounces += (dh.abs() > 0.01 && step < 0.5 * scale && p.speed > 5.0) as u32;
            prev = p;
        }
        println!(
            "5 min: walked {:.0} px ({:.0} px/s), turned {:.0} deg total, still {:.0}% of frames, ~{bounces} edge frames",
            walked, walked / 300.0, turned.to_degrees(), still as f32 / 180.0
        );

        clock.set_paused(true, now);
        now += Duration::from_secs(30);
        clock.set_paused(false, now);
        now += frame;
        d.advance(clock.tick(now), scale);
        let p = d.pose(scale);
        let jump = (p.x - prev.x).hypot(p.y - prev.y);
        assert!(p.x.is_finite() && jump < 0.5 * 16.7 * scale, "resume jumped {jump} px");

        // A stall: the clock caps it at 0.1 s, the driver at 50 brain steps.
        let Driver::Brain { owed_ms, .. } = &d else { unreachable!() };
        let owed = *owed_ms;
        d.advance(10.0, scale);
        let Driver::Brain { owed_ms, .. } = &d else { unreachable!() };
        assert!(owed < 1.0 && *owed_ms < 1.0, "excess brain time not dropped");
        let q = d.pose(scale);
        assert!((q.x - p.x).hypot(q.y - p.y) < 0.5 * 50.0 * scale, "stall moved the fly too far");
    }

    /// Channel order is the silent bug: red must stay red. tiny-skia RGBA [R,G,B,A] in,
    /// Windows BGRA out.
    #[test]
    fn cpu_meter_skips_pauses() {
        let burn = |ms| {
            let t = Instant::now();
            while t.elapsed() < Duration::from_millis(ms) {
                std::hint::black_box(0u64.wrapping_add(1));
            }
        };
        let mut m = CpuMeter::default();
        m.start();
        burn(300);
        m.stop();
        std::thread::sleep(Duration::from_millis(300)); // paused: not counted
        m.start();
        burn(300);
        let r = m.report();
        // Other tests share the process, so CPU is only bounded below.
        assert!((0.55..0.75).contains(&m.acc.1) && m.acc.0 > 0.4, "{r}");
    }

    #[test]
    fn red_stays_red() {
        // The eye's mid red, exactly as the palette gives it, plus a half-alpha pixel.
        let (r, g, b) = art::EYE[1];
        let c = Color::from_rgba(r, g, b, 1.0).unwrap().to_color_u8();
        assert_eq!([c.red(), c.green(), c.blue()], [199, 26, 48]);
        let mut out = [0u8; 8];
        rgba_to_bgra(&[199, 26, 48, 255, 100, 0, 0, 128], &mut out);
        assert_eq!(out, [48, 26, 199, 255, 0, 0, 100, 128]);
    }

    /// On the real desktop: shows the fly for ~2 s at the primary screen's centre and captures
    /// under it, with and without WDA_EXCLUDEFROMCAPTURE, with SRCCOPY (what `vision` uses) and
    /// CAPTUREBLT, each against a capture from before it appeared. The fly must be invisible to
    /// what `vision` does. Run: `cargo test self_exclusion -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn self_exclusion() {
        use windows::Win32::Graphics::Gdi::{CAPTUREBLT, SRCCOPY};
        // SAFETY: plain FFI call; fails harmlessly if already set.
        let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        let hwnd = overlay();
        let scale = dpi_scale(hwnd);
        let mut canvas = Canvas::new(scale);
        let pose = Body::new(screen_center()).pose(scale);
        canvas.present(hwnd, &pose, &Feet::new(&pose, scale), scale, None);
        let (origin, size) = (art::window_origin(&pose, scale, canvas.size), canvas.size as usize);
        let settle = || std::thread::sleep(Duration::from_millis(400));
        let grab = |rop| vision::capture(origin, size, rop).expect("capture");
        let changed = |a: &[u8], b: &[u8]| a.chunks_exact(4).zip(b.chunks_exact(4)).filter(|(p, q)| p[..3] != q[..3]).count();
        let before = (grab(SRCCOPY), grab(CAPTUREBLT | SRCCOPY));
        // SAFETY: hwnd is our live window.
        let _ = unsafe { ShowWindow(hwnd, SW_SHOWNOACTIVATE) };
        let mut rows = vec![];
        // Recordable, then excluded again: the tray toggle's path.
        for (name, on) in [("recordable (WDA_NONE)", true), ("excluded (WDA_EXCLUDEFROMCAPTURE)", false)] {
            let sees = set_recordable(hwnd, on);
            settle();
            let (plain, blt) = (changed(&before.0, &grab(SRCCOPY)), changed(&before.1, &grab(CAPTUREBLT | SRCCOPY)));
            println!("{name}, vision {sees}: fly pixels in capture: SRCCOPY {plain}, CAPTUREBLT {blt} (of {})", size * size);
            rows.push((plain, blt));
        }
        // SAFETY: our window, destroyed once.
        let _ = unsafe { DestroyWindow(hwnd) };
        assert!(rows[0].0 + rows[0].1 > 100, "the fly never showed up in any capture: the test proves nothing");
        assert_eq!(rows[1].0, 0, "vision sees the fly");
    }
}
