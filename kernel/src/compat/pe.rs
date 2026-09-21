//! PE32+ loader stub (Win32 compatibility layer foundation).
//!
//! Parses a PE32+ (64-bit Windows executable) header to extract:
//!   - DOS header (MZ magic at offset 0).
//!   - PE signature ("PE\0\0" at e_lfanew offset).
//!   - COFF header (machine, num sections, entry point).
//!   - Optional header (image base, entry point, section alignment).
//!
//! For Stage 9 we parse headers only — no actual loading/execution.
//! Stage 10 will add NTDLL/KERNEL32 emulation on top of syscalls.

use crate::serial;

/// DOS header magic.
const DOS_MAGIC: [u8; 2] = [0x4D, 0x5A];  // "MZ"
/// PE signature.
const PE_SIG: [u8; 4] = [0x50, 0x45, 0x00, 0x00];  // "PE\0\0"

/// Parsed PE info.
#[derive(Clone, Copy)]
pub struct PeInfo {
    pub is_pe32plus: bool,
    pub machine: u16,
    pub num_sections: u16,
    pub entry_point: u32,
    pub image_base: u64,
    pub valid: bool,
}

impl PeInfo {
    pub const fn empty() -> Self {
        Self {
            is_pe32plus: false, machine: 0, num_sections: 0,
            entry_point: 0, image_base: 0, valid: false,
        }
    }
}

pub fn init() {
    serial::print_str("[pe] PE32+ loader initialized (header parsing)\n");
}

/// Parse a PE32+ file from a byte slice.
pub fn parse(data: &[u8]) -> PeInfo {
    let mut info = PeInfo::empty();

    // DOS header check — byte-by-byte.
    if data.len() < 64 || data[0] != DOS_MAGIC[0] || data[1] != DOS_MAGIC[1] {
        serial::print_str("[pe] no DOS/MZ header\n");
        return info;
    }

    // e_lfanew (offset to PE header) at offset 0x3C (4 bytes, LE).
    let e_lfanew = u32::from_le_bytes([data[0x3C], data[0x3D], data[0x3E], data[0x3F]]) as usize;
    if e_lfanew + 24 > data.len() {
        serial::print_str("[pe] e_lfanew out of bounds\n");
        return info;
    }

    // PE signature — byte-by-byte.
    if data[e_lfanew] != PE_SIG[0] || data[e_lfanew+1] != PE_SIG[1]
        || data[e_lfanew+2] != PE_SIG[2] || data[e_lfanew+3] != PE_SIG[3] {
        serial::print_str("[pe] no PE signature\n");
        return info;
    }

    // COFF header (20 bytes after PE sig).
    let coff_off = e_lfanew + 4;
    info.machine = u16::from_le_bytes([data[coff_off], data[coff_off + 1]]);
    info.num_sections = u16::from_le_bytes([data[coff_off + 2], data[coff_off + 3]]);

    // Optional header starts at coff_off + 20.
    let opt_off = coff_off + 20;
    if opt_off + 24 > data.len() {
        serial::print_str("[pe] optional header out of bounds\n");
        return info;
    }

    // Magic: 0x10B = PE32, 0x20B = PE32+.
    let opt_magic = u16::from_le_bytes([data[opt_off], data[opt_off + 1]]);
    info.is_pe32plus = opt_magic == 0x20B;

    // Entry point (offset 16 from optional header start).
    info.entry_point = u32::from_le_bytes([
        data[opt_off + 16], data[opt_off + 17],
        data[opt_off + 18], data[opt_off + 19],
    ]);

    // Image base: PE32+ = 8 bytes at offset 24; PE32 = 4 bytes at offset 28.
    if info.is_pe32plus {
        info.image_base = u64::from_le_bytes([
            data[opt_off + 24], data[opt_off + 25], data[opt_off + 26], data[opt_off + 27],
            data[opt_off + 28], data[opt_off + 29], data[opt_off + 30], data[opt_off + 31],
        ]);
    } else {
        info.image_base = u32::from_le_bytes([
            data[opt_off + 28], data[opt_off + 29],
            data[opt_off + 30], data[opt_off + 31],
        ]) as u64;
    }

    info.valid = true;

    serial::print_str("[pe] valid: machine=0x");
    serial::print_hex(info.machine as u64);
    serial::print_str(" sections=");
    serial::print_hex(info.num_sections as u64);
    serial::print_str(" entry=0x");
    serial::print_hex(info.entry_point as u64);
    serial::print_str(" base=0x");
    serial::print_hex(info.image_base);
    serial::print_str(info.is_pe32plus.then_some(" (PE32+)").unwrap_or(" (PE32)"));
    serial::print_str("\n");

    info
}

/// Test parsing with a minimal in-memory PE32+ executable.
pub fn test_parse() {
    serial::print_str("[pe] test: parsing minimal PE32+\n");

    static mut TEST_PE: [u8; 128] = [0; 128];
    let p = unsafe { core::ptr::addr_of_mut!(TEST_PE) as *mut u8 };

    unsafe {
        // DOS header: "MZ" at 0.
        core::ptr::write_volatile(p.add(0), 0x4D);
        core::ptr::write_volatile(p.add(1), 0x5A);
        // e_lfanew = 0x40 (64) at offset 0x3C.
        core::ptr::write_volatile(p.add(0x3C), 0x40);
        core::ptr::write_volatile(p.add(0x3D), 0x00);
        core::ptr::write_volatile(p.add(0x3E), 0x00);
        core::ptr::write_volatile(p.add(0x3F), 0x00);
        // PE signature at 0x40.
        core::ptr::write_volatile(p.add(0x40), 0x50);  // P
        core::ptr::write_volatile(p.add(0x41), 0x45);  // E
        core::ptr::write_volatile(p.add(0x42), 0x00);
        core::ptr::write_volatile(p.add(0x43), 0x00);
        // COFF: machine = 0x8664 (x86-64).
        core::ptr::write_volatile(p.add(0x44), 0x64);
        core::ptr::write_volatile(p.add(0x45), 0x86);
        // Num sections = 2.
        core::ptr::write_volatile(p.add(0x46), 0x02);
        core::ptr::write_volatile(p.add(0x47), 0x00);
        // Optional header at 0x58 (0x40 + 4 + 20).
        let opt = 0x58usize;
        // Magic = 0x20B (PE32+).
        core::ptr::write_volatile(p.add(opt), 0x0B);
        core::ptr::write_volatile(p.add(opt + 1), 0x02);
        // Entry point = 0x1000 at opt+16.
        core::ptr::write_volatile(p.add(opt + 16), 0x00);
        core::ptr::write_volatile(p.add(opt + 17), 0x10);
        core::ptr::write_volatile(p.add(opt + 18), 0x00);
        core::ptr::write_volatile(p.add(opt + 19), 0x00);
        // Image base = 0x400000 at opt+24 (8 bytes LE).
        core::ptr::write_volatile(p.add(opt + 24), 0x00);
        core::ptr::write_volatile(p.add(opt + 25), 0x00);
        core::ptr::write_volatile(p.add(opt + 26), 0x40);
        core::ptr::write_volatile(p.add(opt + 27), 0x00);
    }

    let info = parse(unsafe { &TEST_PE[..128] });
    if info.valid {
        serial::print_str("[pe] test: OK (PE32+ header parsed)\n");
        super::PACKAGES_PARSED.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    } else {
        serial::print_str("[pe] test: FAIL\n");
    }
}
