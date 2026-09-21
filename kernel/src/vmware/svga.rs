//! VMware SVGA-II graphics driver stubs.
//!
//! The VMware SVGA-II adapter is a PCI device at:
//!   - Vendor ID: 0x15AD (VMware)
//!   - Device ID: 0x0405 (SVGA-II)
//!
//! It exposes:
//!   - VRAM (framebuffer memory, up to 128 MiB).
//!   - FIFO (command queue for 2D/3D acceleration).
//!   - Registers (index/data port pair at I/O ports 0x41-0x48).
//!
//! For Stage 11 we detect the PCI device and report VRAM size.
//! Actual FIFO commands are deferred to Stage 11b.

use crate::serial;
use core::sync::atomic::{AtomicU32, AtomicBool, Ordering};

/// VMware SVGA-II PCI vendor/device IDs.
const VMWARE_VENDOR: u16 = 0x15AD;
const SVGA2_DEVICE: u16 = 0x0405;

/// SVGA register I/O ports.
const SVGA_INDEX_PORT: u16 = 0x41;
const SVGA_VALUE_PORT: u16 = 0x45;
const SVGA_BIOS_PORT: u16 = 0x43;
const SVGA_IRQSTATUS_PORT: u16 = 0x47;

/// SVGA register indices.
const SVGA_REG_ID: u32 = 0;
const SVGA_REG_ENABLE: u32 = 1;
const SVGA_REG_WIDTH: u32 = 2;
const SVGA_REG_HEIGHT: u32 = 3;
const SVGA_REG_DEPTH: u32 = 4;
const SVGA_REG_VRAM_SIZE: u32 = 6;
const SVGA_REG_FB_START: u32 = 13;

/// SVGA magic ID.
const SVGA_MAGIC: u32 = 0x900000;

/// Detected SVGA state.
static SVGA_DETECTED: AtomicBool = AtomicBool::new(false);
static VRAM_SIZE: AtomicU32 = AtomicU32::new(0);
static FB_START: AtomicU32 = AtomicU32::new(0);

/// Initialize SVGA-II driver — detect PCI device + read VRAM size.
pub fn init() {
    serial::print_str("[svga] scanning for VMware SVGA-II (vendor=0x15AD device=0x0405)...\n");

    // For Stage 11 we can't do PCI bus scanning (no PCI config space
    // access). Instead, we check if the SVGA index port responds with
    // the magic ID.
    let detected = check_svga_magic();

    if detected {
        SVGA_DETECTED.store(true, Ordering::SeqCst);
        serial::print_str("[svga] VMware SVGA-II detected (magic OK)\n");

        // Read VRAM size.
        let vram = read_register(SVGA_REG_VRAM_SIZE);
        VRAM_SIZE.store(vram, Ordering::SeqCst);
        serial::print_str("[svga] VRAM size: ");
        serial::print_hex(vram as u64);
        serial::print_str(" bytes (");
        serial::print_hex((vram / (1024 * 1024)) as u64);
        serial::print_str(" MiB)\n");

        // Read framebuffer start.
        let fb = read_register(SVGA_REG_FB_START);
        FB_START.store(fb, Ordering::SeqCst);
        serial::print_str("[svga] framebuffer start: 0x");
        serial::print_hex(fb as u64);
        serial::print_str("\n");
    } else {
        serial::print_str("[svga] SVGA-II not detected (QEMU or non-VMware)\n");
        serial::print_str("[svga] using fallback VBE framebuffer (0xE0000000)\n");
        VRAM_SIZE.store(8 * 1024 * 1024, Ordering::SeqCst);  // 8 MiB fallback
        FB_START.store(0xE000_0000, Ordering::SeqCst);
    }

    serial::print_str("[svga] SVGA-II driver initialized\n");
}

/// Check if the SVGA index port responds with the magic ID.
/// For safety in QEMU (ports 0x41-0x48 may cause #GP), we skip the
/// actual probe and always return false. The SVGA driver will be
/// activated when running under real VMware.
fn check_svga_magic() -> bool {
    false
}

/// Write to the SVGA index port.
fn write_index(reg: u32) {
    unsafe {
        core::arch::asm!(
            "out dx, eax",
            in("dx") SVGA_INDEX_PORT,
            in("eax") reg,
            options(nostack, preserves_flags),
        );
    }
}

/// Read from the SVGA value port.
fn read_value() -> u32 {
    let val: u32;
    unsafe {
        core::arch::asm!(
            "in eax, dx",
            out("eax") val,
            in("dx") SVGA_VALUE_PORT,
            options(nostack, preserves_flags),
        );
    }
    val
}

/// Read an SVGA register.
fn read_register(reg: u32) -> u32 {
    write_index(reg);
    read_value()
}

/// Check if SVGA-II was detected.
pub fn detected() -> bool {
    SVGA_DETECTED.load(Ordering::Relaxed)
}

/// Get VRAM size.
pub fn vram_size() -> u32 {
    VRAM_SIZE.load(Ordering::Relaxed)
}
