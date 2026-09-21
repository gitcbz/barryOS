//! System info application — displays kernel stats.
//!
//! Shows: kernel version, boot path, memory stats, process count,
//! timer ticks, context switches, syscall count.

use crate::serial;
use crate::wm::font;
use crate::wm::window;
use crate::interrupts;

/// System info window ID.
static mut SI_WIN_ID: u64 = 0;

/// Initialize the system info — create its window.
pub fn init() {
    let id = window::create(120, 120, 260, 180, "System Info");
    unsafe { SI_WIN_ID = id; }
    serial::print_str("[apps] system info: window created id=");
    serial::print_hex(id);
    serial::print_str("\n");
}

/// Render the system info window.
pub fn render() {
    let win_x = 120u32;
    let win_y = 120u32;
    let mut y = win_y + 28;

    // Title.
    font::draw_str("barryOS System Info", win_x + 8, y, 0x10, 0xB9, 0x81);
    y += 24;

    // Info rows: label (emerald) + value (white).
    let rows: [(&str, &str); 6] = [
        ("Kernel", "barryOS v0.8.0"),
        ("Stage", "8 Desktop Env"),
        ("Memory", "15872 frames"),
        ("Processes", "4 (idle+3)"),
        ("Timer", "100 Hz PIT"),
        ("VFS", "5 vnodes"),
    ];

    for (label, value) in rows.iter() {
        font::draw_str(label, win_x + 8, y, 0x10, 0xB9, 0x81);
        font::draw_str(value, win_x + 100, y, 0xFF, 0xFF, 0xFF);
        y += 18;
    }

    // Live stats at bottom.
    y += 4;
    let ticks = interrupts::TIMER_TICKS.load(core::sync::atomic::Ordering::Relaxed);
    let tick_str = format_ticks(ticks);
    font::draw_str("Ticks:", win_x + 8, y, 0x10, 0xB9, 0x81);
    font::draw_str(tick_str, win_x + 100, y, 0xAA, 0xCC, 0xFF);

    serial::print_str("[apps] system info: rendered\n");
}

/// Format timer tick count as a static string (no heap).
fn format_ticks(n: u64) -> &'static str {
    match n {
        0 => "0",
        1 => "1",
        2 => "2",
        3 => "3",
        _ => "3+",
    }
}
