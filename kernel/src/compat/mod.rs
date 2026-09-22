//! barryOS kernel — compatibility layers.
//!
//! Modules:
//! - `deb`:       .deb package parser (ar archive + control/data members).
//! - `rpm`:       .rpm package parser (lead + signature + header).
//! - `appimage`:  .AppImage detection (ELF + squashfs).
//! - `pe_loader`: PE image loader — parse, map, relocate, bind imports, run.
//! - `win32`:     the KERNEL32 functions a loaded image can actually call.
//!
//! What is real here and what is not:
//!
//!   PE32+ (x86-64)  loads and executes.
//!   PE32  (i386)    parses; execution needs a 32-bit compatibility segment.
//!   .deb / .rpm     the headers are parsed.  No compression, no installation.
//!   .AppImage       detected.  Never mounted.

pub mod deb;
pub mod rpm;
pub mod appimage;
pub mod pe_loader;
pub mod win32;

use core::sync::atomic::{AtomicBool, Ordering};
use crate::serial;

pub static INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Total packages parsed (for diagnostics).
pub static PACKAGES_PARSED: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// Is this the start of a PE image?
pub fn is_pe(data: &[u8]) -> bool {
    data.len() >= 0x40 && data[0] == b'M' && data[1] == b'Z'
}

/// Load and run a PE image.  Returns the program's exit code, or why it could
/// not be run.
pub fn run_pe(data: &[u8]) -> Result<pe_loader::RunResult, &'static str> {
    pe_loader::run(data)
}

pub fn init() {
    serial::print_str("[compat] step 1: .deb parser\n");
    deb::init();
    deb::test_parse();

    serial::print_str("[compat] step 2: .rpm parser\n");
    rpm::init();
    rpm::test_parse();

    serial::print_str("[compat] step 3: .AppImage detector\n");
    appimage::init();
    appimage::test_detect();

    serial::print_str("[compat] step 4: Win32 layer\n");
    win32::init();

    INITIALIZED.store(true, Ordering::Release);
    serial::print_str("[compat] compatibility layers online\n");
}
