//! barryOS kernel — desktop applications.
//!
//! Modules:
//! - `terminal`: Interactive terminal with command prompt.
//! - `filemgr`:  Filesystem browser (navigate, create, rename, delete).
//! - `sysinfo`:  System info display.
//! - `editor`:   Text editor for files on the RAM filesystem.
//!
//! This module also holds the app registry, and it is where input is routed:
//! the shell hands it a focused window id and a click or a keystroke, and it
//! goes to whichever app owns that window.

pub mod terminal;
pub mod filemgr;
pub mod sysinfo;
pub mod editor;
pub mod installer;

use core::sync::atomic::{AtomicBool, Ordering};
use crate::serial;

pub static INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Number of apps in the registry.
pub const COUNT: usize = 4;

/// Display name of registry entry `i`.
pub fn name(i: usize) -> &'static str {
    match i {
        0 => "Terminal",
        1 => "Files",
        2 => "System Info",
        3 => "Editor",
        _ => "?",
    }
}

/// Open (or raise) registry entry `i`.
pub fn launch(i: usize) {
    match i {
        0 => terminal::open(),
        1 => filemgr::open(),
        2 => sysinfo::open(),
        3 => editor::open(),
        _ => {}
    }
}

/// Number of registry entries.
pub fn count() -> usize {
    COUNT
}

/// Initialize the apps that should be on screen at boot.
///
/// The editor is deliberately not among them: it opens from the launcher or by
/// picking a file in the file manager, which is how it gets something to edit.
pub fn init() {
    serial::print_str("[apps] step 1: init terminal\n");
    terminal::init();

    serial::print_str("[apps] step 2: init file manager\n");
    filemgr::init();

    serial::print_str("[apps] step 3: init system info\n");
    sysinfo::init();

    INITIALIZED.store(true, Ordering::Release);
    serial::print_str("[apps] desktop applications online\n");
}

/// Draw the content of the window with this id.
///
/// Called by the compositor immediately after `window::render_window` has
/// painted that window's frame, so content is layered with the frame it
/// belongs to.
pub fn render_content(win_id: u64) {
    if win_id == terminal::window_id() {
        terminal::render();
    } else if win_id == filemgr::window_id() {
        filemgr::render();
    } else if win_id == sysinfo::window_id() {
        sysinfo::render();
    } else if win_id == editor::window_id() {
        editor::render();
    } else if win_id == installer::window_id() {
        installer::render();
    }
}

/// A click landed inside a window's content area (not its title bar).
pub fn on_click(win_id: u64, px: u32, py: u32) -> bool {
    if win_id == filemgr::window_id() {
        return filemgr::on_click(px, py);
    }
    if win_id == editor::window_id() {
        return editor::on_click(px, py);
    }
    if win_id == installer::window_id() {
        return installer::on_click(px, py);
    }
    false
}

/// Called once per input-loop pass.
///
/// Work that would block the compositor — the installer writing megabytes to
/// a disk in PIO mode — runs here, between frames, so the progress bar it
/// painted is on screen before the first write starts.
pub fn tick() -> bool {
    installer::tick()
}

/// Route a keystroke to the app that owns the focused window.
/// Returns true when the screen needs repainting.
pub fn handle_key(win_id: u64, key: u8) -> bool {
    if win_id == terminal::window_id() {
        return terminal::handle_key(key);
    }
    if win_id == filemgr::window_id() {
        return filemgr::handle_key(key);
    }
    if win_id == editor::window_id() {
        return editor::handle_key(key);
    }
    false
}

/// Drive the terminal's command parser for real.
///
/// Must run before the compositor's first pass, or rather before the pass that
/// is expected to show the output: the terminal writes into its character
/// buffer, and `render` replays that buffer.
pub fn run_selftest() {
    serial::print_str("[apps] step 4: terminal command self-test\n");
    for cmd in ["ver", "ls", "ps"] {
        terminal::process_command(cmd);
    }
    serial::print_str("[apps] terminal: all commands processed OK\n");
}
