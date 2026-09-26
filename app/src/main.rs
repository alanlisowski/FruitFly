// Without this, Windows gives a GUI-less process a console window that flashes up on every launch.
#![windows_subsystem = "windows"]

mod art;
mod fly;
mod path;
mod snapshot;

use fly::{Feet, FlyPose};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tiny_skia::{Color, Pixmap};
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{Icon, TrayIconBuilder};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION,
    CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, HBITMAP, HDC,
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
}

impl Canvas {
    fn new(size: i32) -> Self {
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
        }
    }

    /// Draws `pose` and pushes it to the screen, moving and (if needed) resizing the window.
    fn present(&mut self, hwnd: HWND, pose: &FlyPose, feet: &Feet, scale: f32) {
        // Whole-pixel window position (floored); the fly's fractional part is drawn *inside*
        // the pixmap. Rounding the window instead would make slow motion snap and shimmer.
        let origin = (
            pose.x.floor() as i32 - self.size / 2,
            pose.y.floor() as i32 - self.size / 2,
        );
        self.pixmap.fill(Color::TRANSPARENT);
        art::draw(pose, feet, scale, &mut self.pixmap, origin);

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
    if args.get(1).map(String::as_str) == Some("--snapshot") {
        // SAFETY: plain FFI call; failing just means there is no parent console to print to.
        let _ = unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
        match snapshot::run(std::path::Path::new(args.get(2).map(String::as_str).unwrap_or("."))) {
            Ok(()) => println!("snapshots written"),
            Err(e) => println!("snapshot failed: {e}"),
        }
        return;
    }

    // SAFETY: plain FFI call with a constant. Must be the FIRST thing we do, before any window
    // exists; otherwise Windows "virtualises" our coordinates on scaled displays (125%, 150%...)
    // and the window lands in the wrong place. PER_MONITOR_AWARE_V2 = we get real pixels and are
    // told (WM_DPICHANGED) when the window crosses to a monitor with a different scale.
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) }
        .expect("SetProcessDpiAwarenessContext");

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
    let mut center = screen_center();
    let mut canvas = Canvas::new(fly::window_size(scale));
    let mut walker = path::Walker::new();
    let mut step = walker.step(0.0);
    let mut pose = to_pose(&step, scale, center);
    let mut feet = Feet::new(&pose, scale);
    canvas.present(hwnd, &pose, &feet, scale);
    // SW_SHOWNOACTIVATE: show without activating. Plain SW_SHOW would steal focus.
    // SAFETY: hwnd is a live window we created.
    let _ = unsafe { ShowWindow(hwnd, SW_SHOWNOACTIVATE) };

    // Fixed-step loop: drain messages, render when a frame is due, otherwise sleep ~1 ms.
    // No WM_TIMER; the later 1 kHz brain tick slots in next to the render step.
    let mut clock = Clock { last: Instant::now(), paused: false };
    let mut next_frame = clock.last;
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
            center = screen_center();
            canvas = Canvas::new(fly::window_size(scale));
            pose = to_pose(&step, scale, center);
            feet = Feet::new(&pose, scale); // feet are pinned in screen pixels: re-pin at the new scale
        }

        if clock.paused {
            // No stepping, no drawing, no UpdateLayeredWindow: just keep pumping messages
            // (the loop above) a few times a second so the tray still responds.
            std::thread::sleep(Duration::from_millis(30));
            continue;
        }

        let now = Instant::now();
        if now >= next_frame {
            // Motion is driven by real elapsed running time, not by counting frames, so a
            // dropped frame doesn't change the fly's speed (see `Clock`).
            step = walker.step(clock.tick(now));
            pose = to_pose(&step, scale, center);
            feet.update(&pose, scale);
            redraw = true;
            next_frame += FRAME;
            if next_frame < now {
                next_frame = now + FRAME; // fell behind: don't burst to catch up
            }
        } else if !redraw {
            std::thread::sleep(Duration::from_millis(1));
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

    /// Channel order is the silent bug: red must stay red. tiny-skia RGBA [R,G,B,A] in,
    /// Windows BGRA out.
    #[test]
    fn red_stays_red() {
        let mut out = [0u8; 8];
        rgba_to_bgra(&[200, 10, 20, 255, 100, 0, 0, 128], &mut out);
        assert_eq!(out, [20, 10, 200, 255, 0, 0, 100, 128]);
    }
}
