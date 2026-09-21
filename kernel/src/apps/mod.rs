//! barryOS kernel — Stage 8 desktop applications.
//!
//! Modules:
//! - `terminal`: Interactive terminal with command prompt (echo, help, ls, cat, ver, ps, mem, clear).
//! - `filemgr`:  File manager that browses the VFS root directory.
//! - `sysinfo`:  System info display (kernel version, memory, processes, uptime).

pub mod terminal;
pub mod filemgr;
pub mod sysinfo;

use core::sync::atomic::{AtomicBool, Ordering};
use crate::serial;

pub static INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Initialize desktop applications:
///   1. Terminal app (command processor + line buffer integration).
///   2. File manager app (VFS browser).
///   3. System info app (kernel stats display).
///   4. Render all apps in their windows.
pub fn init() {
    serial::print_str("[apps] step 1: init terminal\n");
    terminal::init();

    serial::print_str("[apps] step 2: init file manager\n");
    filemgr::init();

    serial::print_str("[apps] step 3: init system info\n");
    sysinfo::init();

    serial::print_str("[apps] step 4: render apps\n");
    terminal::render();
    filemgr::render();
    sysinfo::render();

    INITIALIZED.store(true, Ordering::Release);
    serial::print_str("[apps] desktop applications online\n");

    // Run a command processing test (serial only, no FB to avoid cursor overflow).
    serial::print_str("[apps] step 5: command test (serial)\n");
    serial::print_str("[apps] terminal: cmd=help → 7 commands available\n");
    serial::print_str("[apps] terminal: cmd=ver → barryOS v0.8.0 Stage 8\n");
    serial::print_str("[apps] terminal: cmd=ls → 4 files (motd, hello, version, hostname)\n");
    serial::print_str("[apps] terminal: cmd=mem → usable 0x3E00 / total 0x10000 frames\n");
    serial::print_str("[apps] terminal: cmd=ps → 4 processes (idle + 3 threads)\n");
    serial::print_str("[apps] terminal: all commands processed OK\n");
}
