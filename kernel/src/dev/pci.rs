//! PCI bus enumeration (legacy configuration mechanism #1, ports 0xCF8/0xCFC).
//!
//! This is the prerequisite for every device driver that does not sit at a
//! fixed legacy address: the NIC has to be found before it can be driven, and
//! the VMware SVGA-II adapter can finally be identified properly instead of
//! being guessed at through I/O ports.
//!
//! Scope: enough to locate a device, read its BARs and map them.  No interrupt
//! routing (MSI/MSI-X), no bridges beyond recursing into bus numbers as they
//! are discovered.

use crate::serial;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

const CONFIG_ADDR: u16 = 0xCF8;
const CONFIG_DATA: u16 = 0xCFC;

/// How many devices we are willing to remember.
pub const MAX_DEVICES: usize = 32;

#[derive(Clone, Copy)]
pub struct PciDevice {
    pub bus: u8,
    pub dev: u8,
    pub func: u8,
    pub vendor: u16,
    pub device: u16,
    pub class: u8,
    pub subclass: u8,
    pub prog_if: u8,
    /// BAR0: raw register value.
    pub bar0: u32,
    /// Decoded BAR0 physical address (0 if not a memory BAR).
    pub bar0_addr: u64,
    /// Size in bytes decoded from BAR0, 0 if unknown.
    pub bar0_size: u64,
}

impl PciDevice {
    const fn empty() -> Self {
        Self {
            bus: 0, dev: 0, func: 0, vendor: 0, device: 0,
            class: 0, subclass: 0, prog_if: 0,
            bar0: 0, bar0_addr: 0, bar0_size: 0,
        }
    }
}

static mut DEVICES: [PciDevice; MAX_DEVICES] = [PciDevice::empty(); MAX_DEVICES];
static COUNT: AtomicUsize = AtomicUsize::new(0);
pub static INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Number of devices found.
pub fn count() -> usize {
    COUNT.load(Ordering::Acquire)
}

/// The device at index `i`, if any.
pub fn device(i: usize) -> Option<PciDevice> {
    if i >= count() {
        return None;
    }
    unsafe {
        let p = core::ptr::addr_of!(DEVICES) as *const PciDevice;
        Some(core::ptr::read_volatile(p.add(i)))
    }
}

/// Find the first device with this vendor/device id.
pub fn find(vendor: u16, device: u16) -> Option<PciDevice> {
    for i in 0..count() {
        if let Some(d) = self::device(i) {
            if d.vendor == vendor && d.device == device {
                return Some(d);
            }
        }
    }
    None
}

/// Find the first device in this class/subclass (e.g. 0x02/0x00 = Ethernet).
pub fn find_class(class: u8, subclass: u8) -> Option<PciDevice> {
    for i in 0..count() {
        if let Some(d) = self::device(i) {
            if d.class == class && d.subclass == subclass {
                return Some(d);
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
//  Configuration space access
// ---------------------------------------------------------------------------

pub fn config_read_u32(bus: u8, dev: u8, func: u8, offset: u8) -> u32 {
    let addr: u32 = 0x8000_0000
        | ((bus as u32) << 16)
        | (((dev as u32) & 0x1F) << 11)
        | (((func as u32) & 0x07) << 8)
        | ((offset as u32) & 0xFC);
    unsafe {
        outl(CONFIG_ADDR, addr);
        inl(CONFIG_DATA)
    }
}

pub fn config_read_u16(bus: u8, dev: u8, func: u8, offset: u8) -> u16 {
    let v = config_read_u32(bus, dev, func, offset & 0xFC);
    ((v >> ((offset & 2) * 8)) & 0xFFFF) as u16
}

pub fn config_read_u8(bus: u8, dev: u8, func: u8, offset: u8) -> u8 {
    let v = config_read_u32(bus, dev, func, offset & 0xFC);
    ((v >> ((offset & 3) * 8)) & 0xFF) as u8
}

pub fn config_write_u32(bus: u8, dev: u8, func: u8, offset: u8, value: u32) {
    let addr: u32 = 0x8000_0000
        | ((bus as u32) << 16)
        | (((dev as u32) & 0x1F) << 11)
        | (((func as u32) & 0x07) << 8)
        | ((offset as u32) & 0xFC);
    unsafe {
        outl(CONFIG_ADDR, addr);
        outl(CONFIG_DATA, value);
    }
}

/// Decode a memory BAR's size by writing all-ones and reading back the mask.
fn bar_size(bus: u8, dev: u8, func: u8, bar_index: u8) -> u64 {
    let off = 0x10 + bar_index * 4;
    let orig = config_read_u32(bus, dev, func, off);
    config_write_u32(bus, dev, func, off, 0xFFFF_FFFF);
    let mask = config_read_u32(bus, dev, func, off);
    config_write_u32(bus, dev, func, off, orig);
    if mask == 0 || mask == 0xFFFF_FFFF {
        return 0;
    }
    // Bits 3..0 of a memory BAR are flags, not address.
    (!(mask & 0xFFFF_FFF0)).wrapping_add(1) as u64
}

/// Scan the PCI buses and record every function that responds.
pub fn init() {
    serial::print_str("[pci] scanning configuration space...\n");

    let mut n = 0usize;
    for bus in 0u16..=255 {
        for dev in 0u8..32 {
            let vendor = config_read_u16(bus as u8, dev, 0, 0x00);
            if vendor == 0xFFFF {
                continue;                       // nothing at this slot
            }
            // Header type bit 7 says this is a multi-function device.
            let header = config_read_u8(bus as u8, dev, 0, 0x0E);
            let funcs = if header & 0x80 != 0 { 8 } else { 1 };

            for func in 0u8..funcs {
                let vendor = config_read_u16(bus as u8, dev, func, 0x00);
                if vendor == 0xFFFF {
                    continue;
                }
                if n >= MAX_DEVICES {
                    serial::print_str("[pci] device table full, stopping\n");
                    COUNT.store(n, Ordering::Release);
                    INITIALIZED.store(true, Ordering::Release);
                    return;
                }

                let device = config_read_u16(bus as u8, dev, func, 0x02);
                let class = config_read_u8(bus as u8, dev, func, 0x0B);
                let subclass = config_read_u8(bus as u8, dev, func, 0x0A);
                let prog_if = config_read_u8(bus as u8, dev, func, 0x09);
                let bar0 = config_read_u32(bus as u8, dev, func, 0x10);
                let is_io = bar0 & 1 != 0;
                let (bar0_addr, bar0_size) = if is_io {
                    ((bar0 & 0xFFFF_FFFC) as u64, 0)
                } else {
                    let size = bar_size(bus as u8, dev, func, 0);
                    (((bar0 & 0xFFFF_FFF0) as u64), size)
                };

                let d = PciDevice {
                    bus: bus as u8, dev, func, vendor, device,
                    class, subclass, prog_if, bar0, bar0_addr, bar0_size,
                };
                unsafe {
                    let p = core::ptr::addr_of_mut!(DEVICES) as *mut PciDevice;
                    core::ptr::write_volatile(p.add(n), d);
                }
                n += 1;
            }
        }
    }

    COUNT.store(n, Ordering::Release);

    serial::print_str("[pci] found ");
    serial::print_hex(n as u64);
    serial::print_str(" device(s):\n");
    for i in 0..n {
        if let Some(d) = device(i) {
            serial::print_str("  ");
            serial::print_hex(d.bus as u64);
            serial::print_str(":");
            serial::print_hex(d.dev as u64);
            serial::print_str(".");
            serial::print_hex(d.func as u64);
            serial::print_str("  ");
            serial::print_hex(d.vendor as u64);
            serial::print_str(":");
            serial::print_hex(d.device as u64);
            serial::print_str("  class ");
            serial::print_hex(d.class as u64);
            serial::print_str(":");
            serial::print_hex(d.subclass as u64);
            if d.bar0_size != 0 {
                serial::print_str("  bar0 0x");
                serial::print_hex(d.bar0_addr);
                serial::print_str(" (");
                serial::print_hex(d.bar0_size / 1024);
                serial::print_str(" KiB)");
            }
            serial::print_str("\n");
        }
    }

    INITIALIZED.store(true, Ordering::Release);
}

// ---------------------------------------------------------------------------
//  Port I/O
// ---------------------------------------------------------------------------

#[inline]
unsafe fn outl(port: u16, val: u32) {
    core::arch::asm!(
        "out dx, eax",
        in("dx") port,
        in("eax") val,
        options(nostack, preserves_flags),
    );
}

#[inline]
unsafe fn inl(port: u16) -> u32 {
    let v: u32;
    core::arch::asm!(
        "in eax, dx",
        out("eax") v,
        in("dx") port,
        options(nostack, preserves_flags),
    );
    v
}
