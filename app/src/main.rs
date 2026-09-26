// Without this, Windows gives a GUI-less process a console window that flashes up on every launch.
#![windows_subsystem = "windows"]

use std::time::{Duration, Instant};
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{Icon, TrayIconBuilder};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION,
    CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, SelectObject,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetSystemMetrics,
    PM_REMOVE, PeekMessageW, RegisterClassW, SM_CXSCREEN, SM_CYSCREEN, SW_SHOWNOACTIVATE,
    ShowWindow, TranslateMessage, ULW_ALPHA, UpdateLayeredWindow, WM_QUIT, WNDCLASSW,
    WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT,
    WS_POPUP,
};
use windows::core::w;

/// Window is a fixed 160x160 px. Small on purpose: every frame pushes SIZE*SIZE pixels to the
/// compositor, so a postage stamp costs almost nothing where a fullscreen overlay would not.
const SIZE_PX: i32 = 160;
const FRAME: Duration = Duration::from_micros(16_667); // ~60 Hz

/// Builds the square's pixels as premultiplied BGRA, one u32 (0xAARRGGBB) per pixel.
///
/// Premultiplied = each colour channel is already multiplied by alpha/255. `UpdateLayeredWindow`
/// with AC_SRC_ALPHA expects this; feed it straight alpha and the edge shows a dark halo.
fn square_pixels() -> Vec<u32> {
    let (half, feather) = (50.0_f32, 4.0_f32); // 100 px square, 4 px soft edge
    let c = SIZE_PX as f32 / 2.0;
    let mut px = Vec::with_capacity((SIZE_PX * SIZE_PX) as usize);
    for y in 0..SIZE_PX {
        for x in 0..SIZE_PX {
            // Chebyshev distance from centre = distance to the square's edge, in "square" metric.
            let m = (x as f32 + 0.5 - c).abs().max((y as f32 + 0.5 - c).abs());
            // 1.0 inside, 0.0 outside, linear ramp across `feather` px centred on the edge.
            let a = ((half + feather / 2.0 - m) / feather).clamp(0.0, 1.0);
            let alpha = (a * 255.0).round() as u32;
            // Pure red (255,0,0) premultiplied: R = 255 * a = alpha, G = B = 0.
            px.push(alpha << 24 | alpha << 16);
        }
    }
    px
}

/// Every window needs a window procedure. Ours has nothing to handle: we draw with
/// `UpdateLayeredWindow`, so WM_PAINT never arrives, and quitting comes from the tray.
unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // SAFETY: forwards Windows' own arguments untouched to Windows' default handler.
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

fn main() {
    // SAFETY: plain FFI call with a constant. Must be the FIRST thing we do, before any window
    // exists; otherwise Windows "virtualises" our coordinates on scaled displays (125%, 150%...)
    // and the window lands in the wrong place. PER_MONITOR_AWARE_V2 = we get real pixels and are
    // told when the window crosses to a monitor with a different scale.
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) }
        .expect("SetProcessDpiAwarenessContext");

    // Tray icon first: it's our only way out. tray-icon makes its own hidden window on this
    // thread, so it works as long as our loop below keeps dispatching messages.
    let quit = MenuItem::new("Quit", true, None);
    let menu = Menu::new();
    menu.append(&quit).unwrap();
    let icon = Icon::from_rgba([255, 0, 0, 255].repeat(32 * 32), 32, 32).unwrap();
    let _tray = TrayIconBuilder::new() // dropped at end of main => icon removed, no ghost icon
        .with_menu(Box::new(menu))
        .with_tooltip("flit")
        .with_icon(icon)
        .build()
        .unwrap();

    // SAFETY: every call below is Win32 FFI. Handles come from the calls that create them and
    // are used only on this thread, while still alive (we never free them before exit).
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
            SIZE_PX,
            SIZE_PX,
            None,
            None,
            Some(hinstance),
            None,
        )
        .unwrap()
    };

    // A 32-bit top-down DIB section: a bitmap whose pixel memory we can write directly.
    // Negative biHeight = top-down (row 0 is the top), which is the order we generate.
    let mut bits = std::ptr::null_mut();
    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: SIZE_PX,
            biHeight: -SIZE_PX,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    // SAFETY: `bmi` and `bits` outlive the call. On success Windows sets `bits` to
    // SIZE_PX*SIZE_PX*4 bytes of pixel memory that lives as long as the bitmap (i.e. forever
    // here; the OS frees GDI objects when we exit).
    let mem_dc = unsafe {
        let dc = CreateCompatibleDC(None); // a memory DC we can select the bitmap into
        let bmp = CreateDIBSection(Some(dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0).unwrap();
        SelectObject(dc, bmp.into());
        dc
    };
    // SAFETY: `bits` points to SIZE_PX*SIZE_PX u32s (see above); nothing else touches it.
    // The square never changes, so we fill it once and only move the window afterwards.
    unsafe {
        let dst = std::slice::from_raw_parts_mut(bits as *mut u32, (SIZE_PX * SIZE_PX) as usize);
        dst.copy_from_slice(&square_pixels());
    }

    // Constant for every frame: use the per-pixel alpha in the bitmap (AC_SRC_ALPHA), with no
    // extra global fade (255 = fully opaque overall).
    let blend = BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: 255,
        AlphaFormat: AC_SRC_ALPHA as u8,
    };
    let size = SIZE { cx: SIZE_PX, cy: SIZE_PX };
    // SAFETY: trivial FFI getters. Physical pixels, thanks to the DPI awareness set above.
    let (sw, sh) = unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };

    // Draws the bitmap AND positions the window in one call (this is the only way to show a
    // layered window's content; WM_PAINT is ignored for it).
    let draw_at = |x: i32, y: i32| {
        // SAFETY: all pointers refer to locals that outlive the call; hwnd and mem_dc are live.
        unsafe {
            UpdateLayeredWindow(
                hwnd,
                None,                  // destination DC: default (the screen)
                Some(&POINT { x, y }), // where the window's top-left goes
                Some(&size),
                Some(mem_dc),
                Some(&POINT { x: 0, y: 0 }), // start at the bitmap's top-left
                COLORREF(0),                 // colour key: unused with ULW_ALPHA
                Some(&blend),
                ULW_ALPHA,
            )
            .unwrap();
        }
    };
    draw_at(0, 0);
    // SW_SHOWNOACTIVATE: show without activating. Plain SW_SHOW would steal focus.
    // SAFETY: hwnd is a live window we created.
    let _ = unsafe { ShowWindow(hwnd, SW_SHOWNOACTIVATE) };

    // Fixed-step loop: drain messages, render when a frame is due, otherwise sleep ~1 ms.
    // No WM_TIMER; the later 1 kHz brain tick slots in next to the render step.
    let start = Instant::now();
    let mut next_frame = start;
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
        if MenuEvent::receiver().try_iter().any(|e| e.id == *quit.id()) {
            break;
        }

        let now = Instant::now();
        if now >= next_frame {
            // Drift in a 200 px-radius circle around screen centre, one lap per 20 s.
            let t = start.elapsed().as_secs_f32() * std::f32::consts::TAU / 20.0;
            draw_at(
                sw / 2 + (200.0 * t.cos()) as i32 - SIZE_PX / 2,
                sh / 2 + (200.0 * t.sin()) as i32 - SIZE_PX / 2,
            );
            next_frame += FRAME;
            if next_frame < now {
                next_frame = now + FRAME; // fell behind (e.g. laptop slept): don't burst to catch up
            }
        } else {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    // SAFETY: hwnd is ours and still valid. Then `_tray` drops, removing the tray icon.
    let _ = unsafe { DestroyWindow(hwnd) };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The alpha bug shows up visually, not as an error, so pin it: premultiplied means no
    /// channel may exceed alpha; centre is solid red; corner is fully transparent.
    #[test]
    fn pixels_are_premultiplied() {
        let px = square_pixels();
        for p in &px {
            let (a, r) = (p >> 24, (p >> 16) & 0xFF);
            assert!(r <= a && p & 0xFFFF == 0);
        }
        assert_eq!(px[(80 * SIZE_PX + 80) as usize], 0xFF_FF_00_00);
        assert_eq!(px[0], 0);
        assert!(px.iter().any(|p| (1..255).contains(&(p >> 24)))); // there IS a soft edge
    }
}
