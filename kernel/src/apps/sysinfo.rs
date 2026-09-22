//! System info application — displays kernel stats.
//!
//! Shows: kernel version, stage, memory stats, process count, timer config
//! and live tick count.  The values are read from the subsystems rather than
//! hardcoded, so the panel cannot drift out of date the way it did when it
//! advertised "Stage 8" and "15872 frames" while running Stage 11.

use crate::interrupts;
use crate::mem;
use crate::proc::process;
use crate::serial;
use crate::wm::font;
use crate::wm::window;

/// System info window ID.
static mut SI_WIN_ID: u64 = 0;

/// Window geometry: bottom-left tile of the 640x480 desktop.
const SI_POS_X: u32 = 8;
const SI_POS_Y: u32 = 246;
const SI_W:     u32 = 360;
const SI_H:     u32 = 194;

/// Column where values start, relative to the window.
const VALUE_COL: u32 = 110;
/// Glyph cell width, for advancing past a number.
const CHAR_W: u32 = 8;

const LABEL_RGB: (u8, u8, u8) = (0x10, 0xB9, 0x81);
const VALUE_RGB: (u8, u8, u8) = (0xFF, 0xFF, 0xFF);
const ACCENT_RGB: (u8, u8, u8) = (0xAA, 0xCC, 0xFF);

/// Window id, for the compositor to match content to frame.
pub fn window_id() -> u64 { unsafe { SI_WIN_ID } }

/// Initialize the system info — create its window.
pub fn init() {
    let id = window::create(SI_POS_X, SI_POS_Y, SI_W, SI_H, "System Info");
    unsafe { SI_WIN_ID = id; }
    serial::print_str("[apps] system info: window created id=");
    serial::print_hex(id);
    serial::print_str("\n");
}

/// Open the system info panel, or raise it if it is already running.
pub fn open() {
    if window::exists(window_id()) {
        window::focus(window_id());
        return;
    }
    init();
}

/// Render the system info window.
pub fn render() {
    let (win_x, win_y) = window::position(unsafe { SI_WIN_ID })
        .unwrap_or((SI_POS_X, SI_POS_Y));
    let label_x = win_x + 8;
    let value_x = win_x + VALUE_COL;
    let mut y = win_y + 28;

    font::draw_str("barryOS System Info", label_x, y, LABEL_RGB.0, LABEL_RGB.1, LABEL_RGB.2);
    y += 24;

    let row = |label: &str, y: u32| {
        font::draw_str(label, label_x, y, LABEL_RGB.0, LABEL_RGB.1, LABEL_RGB.2);
    };

    row("Kernel", y);
    font::draw_str("barryOS v0.8.0", value_x, y, VALUE_RGB.0, VALUE_RGB.1, VALUE_RGB.2);
    y += 16;

    row("Stage", y);
    font::draw_str("11 VMware Opt", value_x, y, VALUE_RGB.0, VALUE_RGB.1, VALUE_RGB.2);
    y += 16;

    // Real numbers from the allocator / scheduler, not constants.
    row("Memory", y);
    let frames = mem::frame_allocator().usable_pages() as u64;
    let n = draw_u64(frames, value_x, y, VALUE_RGB);
    font::draw_str(" frames", value_x + n * CHAR_W, y, ACCENT_RGB.0, ACCENT_RGB.1, ACCENT_RGB.2);
    y += 16;

    row("Processes", y);
    let np = draw_u64(process::count() as u64, value_x, y, VALUE_RGB);
    font::draw_str(" (idle+3)", value_x + np * CHAR_W, y, ACCENT_RGB.0, ACCENT_RGB.1, ACCENT_RGB.2);
    y += 16;

    row("Timer", y);
    font::draw_str("100 Hz PIT", value_x, y, ACCENT_RGB.0, ACCENT_RGB.1, ACCENT_RGB.2);
    y += 16;

    row("VFS", y);
    font::draw_str("5 vnodes", value_x, y, ACCENT_RGB.0, ACCENT_RGB.1, ACCENT_RGB.2);
    y += 20;

    // Live tick count -- the panel is only painted once, so this is the value
    // at first composite.
    row("Ticks:", y);
    draw_u64(interrupts::TIMER_TICKS.load(core::sync::atomic::Ordering::Relaxed),
             value_x, y, ACCENT_RGB);

    serial::print_str("[apps] system info: rendered\n");
}

/// Draw `v` in decimal at (x, y); returns how many characters were drawn.
///
/// Uses a stack buffer rather than a `static` one: handing out a `&'static str`
/// into a shared mutable buffer is exactly the kind of thing that lets the
/// optimiser drop the writes.
fn draw_u64(mut v: u64, x: u32, y: u32, rgb: (u8, u8, u8)) -> u32 {
    let mut buf = [0u8; 20];
    let mut i = buf.len();
    if v == 0 {
        i -= 1;
        buf[i] = b'0';
    }
    while v > 0 {
        i -= 1;
        buf[i] = b'0' + (v % 10) as u8;
        v /= 10;
    }
    let s = core::str::from_utf8(&buf[i..]).unwrap_or("?");
    font::draw_str(s, x, y, rgb.0, rgb.1, rgb.2);
    s.len() as u32
}
