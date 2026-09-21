//! barryOS kernel — Stage 6 device drivers.
//!
//! Modules:
//! - `framebuffer`: GOP linear framebuffer graphics (pixel plotting, rects, text).
//! - `keyboard`:    PS/2 keyboard (scancode → ASCII, line buffer).
//! - `serial2`:    (already in `serial.rs`) — serial debug is already working.

pub mod framebuffer;
pub mod keyboard;

use core::sync::atomic::{AtomicBool, Ordering};
use crate::serial;

pub static INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Initialize device drivers:
///   1. Framebuffer (GOP from BootInfo or fallback VESA/VBE).
///   2. PS/2 keyboard (already in IRQ1 handler — add line buffer).
pub fn init(boot_info: usize) {
    serial::print_str("[dev] step 1: init framebuffer\n");
    framebuffer::init(boot_info);

    serial::print_str("[dev] step 2: init PS/2 keyboard\n");
    keyboard::init();

    // Draw a test pattern to prove the framebuffer works.
    serial::print_str("[dev] step 3: draw test pattern\n");
    framebuffer::draw_test_pattern();

    INITIALIZED.store(true, Ordering::Release);
    serial::print_str("[dev] device drivers online\n");
}
