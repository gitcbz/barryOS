//! Desktop compositor: top-level desktop rendering.
//!
//! Draws the desktop background, status bar, windows, and dock.
//! This is the final render pass that puts everything on screen.

use crate::dev::framebuffer;
use crate::serial;
use crate::wm::{window, widgets, font};

/// Create the default desktop: status bar + 2 windows + dock.
pub fn create_desktop() {
    // Window 1: terminal.
    window::create(40, 40, 320, 200, "Terminal");
    // Window 2: file manager.
    window::create(80, 80, 280, 180, "Files");
    serial::print_str("[wm] desktop created: 2 windows\n");
}

/// Render the complete desktop.
pub fn render() {
    let (fb_addr, w, h, _bpp) = framebuffer::info();
    if fb_addr == 0 || w == 0 || h == 0 {
        serial::print_str("[wm] framebuffer not available\n");
        return;
    }

    serial::print_str("[wm] rendering desktop ");
    serial::print_hex(w as u64);
    serial::print_str("x");
    serial::print_hex(h as u64);
    serial::print_str("\n");

    // Desktop background (dark blue gradient effect — just solid).
    framebuffer::fill_rect(0, 0, w, h, 0x0A, 0x0D, 0x18);

    // Status bar.
    widgets::draw_status_bar(w);

    // Windows.
    window::render_all();

    // Content inside windows (labels).
    // Terminal window content.
    font::draw_str("$ barryOS", 48, 68, 0x10, 0xB9, 0x81);
    font::draw_str("barryOS v0.7", 48, 88, 0xFF, 0xFF, 0xFF);
    font::draw_str("Stage 7 WM", 48, 108, 0xAA, 0xCC, 0xFF);

    // File manager window content.
    font::draw_str("/  (root)", 88, 108, 0xFF, 0xFF, 0xFF);
    font::draw_str("motd", 88, 128, 0xCC, 0xCC, 0xCC);
    font::draw_str("hello", 88, 148, 0xCC, 0xCC, 0xCC);
    font::draw_str("version", 88, 168, 0xCC, 0xCC, 0xCC);

    // Dock.
    widgets::draw_dock(w, h);

    serial::print_str("[wm] desktop rendered (background + status bar + ");
    serial::print_hex(window::count() as u64);
    serial::print_str(" windows + dock)\n");
}
