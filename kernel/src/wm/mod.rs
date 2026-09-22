//! barryOS kernel — Stage 7 window manager + GUI.
//!
//! Modules:
//! - `font`:     8x16 bitmap font for text rendering on framebuffer.
//! - `window`:   Window struct + create/move/resize/close/render.
//! - `widgets`: Base GUI controls (button, label, dock bar).
//! - `desktop`: Top-level desktop compositor (draws all windows + dock).

pub mod font;
pub mod window;
pub mod widgets;
pub mod desktop;
pub mod shell;
pub mod boot;
pub mod login;

use core::sync::atomic::{AtomicBool, Ordering};
use crate::serial;

pub static INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Initialize the window manager:
///   1. Font renderer (8x16 bitmap).
///   2. Window table (16 slots).
///   3. Create test windows (title bar, content, dock).
///   4. Render desktop.
pub fn init() {
    serial::print_str("[wm] step 1: init bitmap font\n");
    font::init();

    serial::print_str("[wm] step 2: init window table\n");
    window::init();

    serial::print_str("[wm] step 3: create desktop windows\n");
    desktop::create_desktop();

    // No render here: the apps create their windows in `apps::init()`, which
    // runs next, and the compositor has to see them.  `main` calls
    // `desktop::render()` once, after every window exists.
    INITIALIZED.store(true, Ordering::Release);
    serial::print_str("[wm] window manager online\n");
}
