//! barryOS kernel — device drivers.
//!
//! Modules:
//! - `framebuffer`: linear framebuffer graphics (pixel plotting, rects, text).
//! - `keyboard`:    PS/2 keyboard (scancode → ASCII, 64-key queue).
//! - `mouse`:       PS/2 mouse (IRQ12, 3-byte packets) + framebuffer cursor.
//! - `pci`:         PCI bus enumeration (everything non-legacy needs this).
//! - `serial` lives in `serial.rs` — working since before this stage.
//!
//! The framebuffer is *not* initialised here: `main` brings it up right after
//! the memory subsystem so the boot splash can start reporting progress as
//! early as possible.

pub mod framebuffer;
pub mod keyboard;
pub mod mouse;
pub mod pci;
pub mod ata;

use core::sync::atomic::{AtomicBool, Ordering};
use crate::serial;

pub static INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Initialize the device drivers that are not the framebuffer.
pub fn init() {
    serial::print_str("[dev] step 1: init PS/2 keyboard\n");
    keyboard::init();

    serial::print_str("[dev] step 2: init PS/2 mouse\n");
    mouse::init();

    serial::print_str("[dev] step 3: enumerate PCI bus\n");
    pci::init();

    serial::print_str("[dev] step 4: probe ATA disks\n");
    ata::init();

    INITIALIZED.store(true, Ordering::Release);
    serial::print_str("[dev] device drivers online\n");
}
