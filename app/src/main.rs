// Without this, Windows gives a GUI-less process a console window that flashes up on every launch.
#![windows_subsystem = "windows"]

mod art;
mod body;
mod fly;
mod path;
mod snapshot;
mod world;

use body::Body;
use flit::brain::{Brain, Pack};
use fly::{Feet, FlyPose};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tiny_skia::{Color, Pixmap};
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
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
    ShowWindow, TranslateMessage, ULW_ALPHA, UpdateLayeredWindow, WM_DPICHANGED, WM_QUIT,
    WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_EX_TRANSPARENT, WS_POPUP,
};
use windows::core::w;

const FRAME: Duration = Duration::from_micros(16_667); // ~60 Hz

/// Set by the window procedure when Windows says our monitor's scaling changed; the main loop
/// notices and rebuilds the window-sized buffers. (A window procedure is a bare `extern fn`
/// with no access to our locals, so a global flag is the simplest way to talk to the loop.)
static DPI_CHANGED: AtomicBool = AtomicBool::new(false);

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
    /// (window x, window y, heading, gait phase) of the frame on screen. Same again = nothing to do.
    shown: Option<[f32; 4]>,
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
    fn present(&mut self, hwnd: HWND, pose: &FlyPose, feet: &Feet, scale: f32) {
        // The window moves in whole art pixels and the fly is drawn at its centre: a fractional
        // offset inside the pixmap would make non-AA edges flicker (see `art`).
        let origin = art::window_origin(pose, scale, self.size);
        // Feet follow from these: they only move when the body moves or the gait advances.
        let key = [origin.0 as f32, origin.1 as f32, pose.heading, pose.gait_phase];
        if self.shown == Some(key) {
            return;
        }
        self.shown = Some(key);
        self.pixmap.fill(Color::TRANSPARENT);
        art::draw(pose, feet, scale, &mut self.pixmap);

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
                let spikes = body.tick(brain, steps, scale, &world.segs, on_work_area, work_area_centre);
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
                                "  world: {} windows, {} segments, poll {:.2} ms, contact L {:.2} R {:.2}, adapt L {:.2} R {:.2}",
                                world.rects.len(),
                                world.segs.len(),
                                world.poll_ms,
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

fn main() {
    // `flit --snapshot <dir>`: render the reference images and exit, no window. A windows-
    // subsystem exe has no console of its own, so borrow the parent's to be able to print.
    let args: Vec<String> = std::env::args().collect();
    let flag = |f: &str| args.iter().any(|a| a == f);
    if flag("--debug") || flag("--pack") || flag("--walk-speed") || flag("--dump-world") {
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
    let quit = MenuItem::new("Quit", true, None);
    let menu = Menu::new();
    menu.append(&pause).unwrap();
    menu.append(&quit).unwrap();
    let icon = Icon::from_rgba([0x57, 0x46, 0x2c, 255].repeat(32 * 32), 32, 32).unwrap();
    let _tray = TrayIconBuilder::new() // dropped at end of main => icon removed, no ghost icon
        .with_menu(Box::new(menu))
        .with_tooltip("flit")
        .with_icon(icon)
        .build()
        .unwrap();

    // SAFETY: every call below is Win32 FFI. Handles come from the calls that create them and
    // are used only on this thread, while still alive.
    let hwnd = unsafe {
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
    };

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
    canvas.present(hwnd, &pose, &feet, scale);
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
            if let Driver::Brain { world, .. } = &mut driver {
                *world = world::poll(hwnd);
            }
            next_poll = now + POLL;
        }
        if now >= next_frame {
            // Motion is driven by real elapsed running time, not by counting frames, so a
            // dropped frame doesn't change the fly's speed (see `Clock`).
            driver.advance(clock.tick(now), scale);
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
            canvas.present(hwnd, &pose, &feet, scale);
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
    fn red_stays_red() {
        // The eye's mid red, exactly as the palette gives it, plus a half-alpha pixel.
        let (r, g, b) = art::EYE[1];
        let c = Color::from_rgba(r, g, b, 1.0).unwrap().to_color_u8();
        assert_eq!([c.red(), c.green(), c.blue()], [199, 26, 48]);
        let mut out = [0u8; 8];
        rgba_to_bgra(&[199, 26, 48, 255, 100, 0, 0, 128], &mut out);
        assert_eq!(out, [48, 26, 199, 255, 0, 0, 100, 128]);
    }
}
