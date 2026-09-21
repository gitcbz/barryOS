//! barryOS kernel — Stage 9 compatibility layers.
//!
//! Modules:
//! - `deb`:      .deb package parser (ar archive + control.tar + data.tar).
//! - `rpm`:      .rpm package parser (RPM header + lead + signature).
//! - `appimage`: .AppImage parser (ELF + squashfs detection).
//! - `pe`:       PE32+ loader stub (Win32 compat layer foundation).

pub mod deb;
pub mod rpm;
pub mod appimage;
pub mod pe;
pub mod pe_loader;
pub mod win32;

use core::sync::atomic::{AtomicBool, Ordering};
use crate::serial;

pub static INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Total packages parsed (for diagnostics).
pub static PACKAGES_PARSED: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// Initialize compatibility layers:
///   1. .deb parser (parse a minimal deb in memory).
///   2. .rpm parser (parse a minimal rpm in memory).
///   3. .AppImage parser (detect format).
///   4. PE loader stub (verify PE header parsing).
pub fn init() {
    serial::print_str("[compat] step 1: init .deb parser\n");
    deb::init();
    deb::test_parse();

    serial::print_str("[compat] step 2: init .rpm parser\n");
    rpm::init();
    rpm::test_parse();

    serial::print_str("[compat] step 3: init .AppImage parser\n");
    appimage::init();
    appimage::test_detect();

    serial::print_str("[compat] step 4: init PE loader\n");
    pe::init();
    pe::test_parse();

    serial::print_str("[compat] step 5: PE section loading\n");
    pe_loader::test_sections();

    serial::print_str("[compat] step 6: Win32 compat layer\n");
    win32::init();
    win32::test();

    INITIALIZED.store(true, Ordering::Release);
    serial::print_str("[compat] compatibility layers online\n");
    serial::print_str("[compat] total packages parsed: ");
    serial::print_hex(PACKAGES_PARSED.load(Ordering::Relaxed));
    serial::print_str("\n");
}
