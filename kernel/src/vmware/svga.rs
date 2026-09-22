//! VMware SVGA-II graphics driver — detection only.
//!
//! The VMware SVGA-II adapter is a PCI device (vendor 0x15AD, device 0x0405)
//! exposing VRAM, a command FIFO and a register pair.
//!
//! Nothing here programs the device.  The desktop is drawn through the
//! firmware's GOP framebuffer, which the loader hands over in BootInfo, and
//! that already gives us the right resolution and pitch.  Programming SVGA-II
//! would only be needed to pick a mode the firmware does not offer, or for
//! 2D acceleration — Stage 11b work.

use crate::serial;
use core::sync::atomic::{AtomicU32, AtomicBool, Ordering};

/// SVGA-II register I/O ports.
///
/// These were 0x41/0x43/0x45/0x47 — inside the 8253 PIT / 8237 DMA range.
/// Probing those is what produced the #GP the old code "worked around" by
/// disabling the probe: the ports were simply wrong.  The SVGA-II register
/// pair is 0x1CE (index) and 0x1CF (value).
const SVGA_INDEX_PORT: u16 = 0x1CE;
const SVGA_VALUE_PORT: u16 = 0x1CF;

/// SVGA register indices.
const SVGA_REG_ID: u32 = 0;

/// SVGA-II device id, as latched by SVGA_REG_ID.
#[allow(dead_code)]
const SVGA_ID_2: u32 = 0x9000_0002;

/// Detected SVGA state.  Both stay zero: the register interface is not read
/// until the driver actually drives the device (Stage 11b).
static SVGA_DETECTED: AtomicBool = AtomicBool::new(false);
static VRAM_SIZE: AtomicU32 = AtomicU32::new(0);

/// Report the SVGA-II situation.
///
/// Deliberately performs no I/O.  On any hypervisor other than VMware the
/// SVGA register ports are unassigned and an `in` raises #GP, which this
/// kernel has no way to resume from; and even on VMware there is nothing to
/// gain here, because the framebuffer already came from GOP.
pub fn init() {
    serial::print_str("[svga] VMware SVGA-II (vendor 0x15AD device 0x0405)\n");

    if !crate::vmware::backdoor::detect_vmware() {
        serial::print_str("[svga] not running under VMware -- SVGA-II not applicable\n");
        serial::print_str("[svga] framebuffer: provided by GOP/BootInfo\n");
        serial::print_str("[svga] SVGA-II driver initialized (inactive)\n");
        return;
    }

    serial::print_str("[svga] running under VMware\n");
    serial::print_str("[svga] framebuffer: provided by GOP/BootInfo\n");
    serial::print_str("[svga] register interface + FIFO: Stage 11b\n");
    serial::print_str("[svga] SVGA-II driver initialized (detect only)\n");
}

/// Write to the SVGA index port.  Unused until Stage 11b.
#[allow(dead_code)]
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

/// Read from the SVGA value port.  Unused until Stage 11b.
#[allow(dead_code)]
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

/// Read an SVGA register.  Unused until Stage 11b.
#[allow(dead_code)]
fn read_register(reg: u32) -> u32 {
    write_index(reg);
    read_value()
}

/// Check if SVGA-II was detected.
pub fn detected() -> bool {
    SVGA_DETECTED.load(Ordering::Relaxed)
}

/// Get VRAM size (0 until Stage 11b reads the register).
pub fn vram_size() -> u32 {
    VRAM_SIZE.load(Ordering::Relaxed)
}
