//! Desktop compositor: top-level desktop rendering.
//!
//! Draws the desktop background, status bar, and dock.
//! Windows are created and rendered by the individual apps
//! (terminal, file manager, system info).

use crate::dev::framebuffer;
use crate::serial;
use crate::wm::{window, widgets};

/// Create the default desktop.  In Stage 8, apps create their own
/// windows, so this just initializes the WM if needed.
pub fn create_desktop() {
    serial::print_str("[wm] desktop created (apps create their own windows)\n");
}

/// Render the complete desktop: background + status bar + windows + dock.
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

    // Desktop background (dark blue).
    framebuffer::fill_rect(0, 0, w, h, 0x0A, 0x0D, 0x18);

    // Status bar.
    widgets::draw_status_bar(w);

    // Windows (rendered by apps — but render any WM-managed windows too).
    window::render_all();

    // Dock.
    widgets::draw_dock(w, h);

    serial::print_str("[wm] desktop rendered (background + status bar + ");
    serial::print_hex(window::count() as u64);
    serial::print_str(" windows + dock)\n");
}
