//! Base GUI controls: button, label, dock bar.
//!
//! These are rendered directly to the framebuffer using the font + rect
//! primitives.  No layout engine — each widget is drawn at absolute
//! pixel coordinates.

use crate::dev::framebuffer;
use crate::wm::font;

/// Draw a button with label text.
pub fn draw_button(x: u32, y: u32, w: u32, h: u32, label: &str) {
    // Button background (slightly lighter than window).
    framebuffer::fill_rect(x, y, w, h, 0x2A, 0x33, 0x50);
    // Border.
    framebuffer::fill_rect(x, y, w, 1, 0x10, 0xB9, 0x81);
    framebuffer::fill_rect(x, y, 1, h, 0x10, 0xB9, 0x81);
    framebuffer::fill_rect(x + w - 1, y, 1, h, 0x10, 0xB9, 0x81);
    framebuffer::fill_rect(x, y + h - 1, w, 1, 0x10, 0xB9, 0x81);
    // Label text (centered).
    let text_w = (label.len() * 8) as u32;
    let tx = x + (w.saturating_sub(text_w)) / 2;
    let ty = y + (h.saturating_sub(16)) / 2;
    font::draw_str(label, tx, ty, 0xFF, 0xFF, 0xFF);
}

/// Draw a label (text without background).
pub fn draw_label(text: &str, x: u32, y: u32, r: u8, g: u8, b: u8) {
    font::draw_str(text, x, y, r, g, b);
}

/// Draw the dock bar at the bottom of the screen.
pub fn draw_dock(screen_w: u32, screen_h: u32) {
    let dock_h = 36u32;
    let dock_y = screen_h - dock_h;
    // Dock background.
    framebuffer::fill_rect(0, dock_y, screen_w, dock_h, 0x12, 0x16, 0x24);
    // Dock top border.
    framebuffer::fill_rect(0, dock_y, screen_w, 1, 0x10, 0xB9, 0x81);
    // Dock buttons (placeholder icons).
    let btn_size = 28u32;
    let spacing = 8u32;
    let mut bx = spacing;
    while bx + btn_size < screen_w - spacing {
        // Emerald button.
        framebuffer::fill_rect(bx, dock_y + 4, btn_size, btn_size, 0x10, 0xB9, 0x81);
        bx += btn_size + spacing;
    }
}

/// Draw a status bar at the top of the screen.
pub fn draw_status_bar(screen_w: u32) {
    let bar_h = 20u32;
    framebuffer::fill_rect(0, 0, screen_w, bar_h, 0x0A, 0x0D, 0x18);
    font::draw_str("barryOS", 8, 3, 0x10, 0xB9, 0x81);
    font::draw_str("Stage 7", 80, 3, 0x40, 0xC0, 0x90);
}
