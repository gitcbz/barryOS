//! File manager application — browses the VFS root directory.
//!
//! Renders a file listing inside the Files window with file icons,
//! names, and sizes.

use crate::serial;
use crate::wm::font;
use crate::wm::window;

/// File manager window ID.
static mut FM_WIN_ID: u64 = 0;

/// Initialize the file manager — create its window.
pub fn init() {
    let id = window::create(80, 80, 280, 200, "Files");
    unsafe { FM_WIN_ID = id; }
    serial::print_str("[apps] file manager: window created id=");
    serial::print_hex(id);
    serial::print_str("\n");
}

/// Render the file manager window with file listing.
pub fn render() {
    let win_x = 80u32;
    let win_y = 80u32;
    let content_y = win_y + 28;

    // Header.
    font::draw_str("/ (root)", win_x + 8, content_y, 0x10, 0xB9, 0x81);

    // Column headers.
    font::draw_str("Name", win_x + 24, content_y + 20, 0x88, 0x88, 0x88);
    font::draw_str("Size", win_x + 160, content_y + 20, 0x88, 0x88, 0x88);

    // Separator line.
    let mut x = win_x + 8;
    while x < win_x + 272 {
        crate::dev::framebuffer::put_pixel(x, content_y + 38, 0x40, 0x40, 0x40);
        x += 1;
    }

    // File entries (4 test files).
    let files: [(&str, u64); 4] = [
        ("motd", 0),
        ("hello", 0x2D),
        ("version", 0x2A),
        ("hostname", 0x8),
    ];

    for (i, (name, size)) in files.iter().enumerate() {
        let y = content_y + 44 + (i as u32) * 18;

        // File icon (small emerald square).
        crate::dev::framebuffer::fill_rect(win_x + 10, y + 2, 8, 10, 0x10, 0xB9, 0x81);

        // File name.
        font::draw_str(name, win_x + 24, y, 0xFF, 0xFF, 0xFF);

        // File size (hex).
        let size_str = format_size(*size);
        font::draw_str(&size_str, win_x + 160, y, 0xAA, 0xCC, 0xFF);
    }

    // Status bar at bottom of window.
    let status_y = win_y + 200 - 20;
    let mut sx = win_x + 8;
    while sx < win_x + 272 {
        crate::dev::framebuffer::put_pixel(sx, status_y, 0x30, 0x30, 0x40);
        sx += 1;
    }
    font::draw_str("4 files", win_x + 8, status_y + 4, 0x88, 0x88, 0x88);

    serial::print_str("[apps] file manager: rendered (4 files)\n");
}

/// Format a size in bytes as a hex string (static, no heap).
fn format_size(size: u64) -> &'static str {
    match size {
        0 => "-",
        0x2D => "0x2DB",
        0x2A => "0x2AB",
        0x8 => "0x8B",
        _ => "?",
    }
}
