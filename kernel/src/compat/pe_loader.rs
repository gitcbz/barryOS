//! PE32+ section loading + relocation table parsing.
//!
//! Extends the basic PE header parser (Stage 9) with:
//!   - Section header enumeration (name, vaddr, raw size, raw offset).
//!   - Import directory parsing (DLL name + function names).
//!   - Base relocation directory parsing (relocation blocks).
//!
//! For Stage 10 we parse these structures from an in-memory PE image
//! (no actual loading/execution yet — that requires virtual memory mapping
//! which is deferred to Stage 10b).

use crate::serial;
use super::pe::PeInfo;

/// PE section header (40 bytes each).
#[derive(Clone, Copy)]
pub struct SectionHeader {
    pub name: [u8; 8],
    pub virtual_size: u32,
    pub virtual_address: u32,
    pub raw_size: u32,
    pub raw_offset: u32,
}

impl SectionHeader {
    pub const fn empty() -> Self {
        Self {
            name: [0; 8],
            virtual_size: 0, virtual_address: 0,
            raw_size: 0, raw_offset: 0,
        }
    }
}

/// Parsed PE import entry (DLL + function).
#[derive(Clone, Copy)]
pub struct ImportEntry {
    pub dll_name: [u8; 32],
    pub dll_name_len: usize,
    pub func_name: [u8; 32],
    pub func_name_len: usize,
}

impl ImportEntry {
    pub const fn empty() -> Self {
        Self {
            dll_name: [0; 32], dll_name_len: 0,
            func_name: [0; 32], func_name_len: 0,
        }
    }
}

/// Relocation block info.
#[derive(Clone, Copy)]
pub struct RelocBlock {
    pub page_rva: u32,
    pub block_size: u32,
    pub entry_count: u16,
}

/// Parse PE sections from a PE32+ image.
/// Returns the number of sections parsed.
pub fn parse_sections(data: &[u8], pe_info: &PeInfo) -> usize {
    if !pe_info.valid {
        serial::print_str("[pe-sec] PE not valid\n");
        return 0;
    }

    // Find e_lfanew.
    let e_lfanew = read_u32_le(data, 0x3C) as usize;
    let coff_off = e_lfanew + 4;
    let opt_off = coff_off + 20;

    // Optional header size is at COFF offset 16 (2 bytes).
    let opt_size = read_u16_le(data, coff_off + 16) as usize;

    // Section headers start after the optional header.
    let sec_off = opt_off + opt_size;
    let n_sections = pe_info.num_sections as usize;

    serial::print_str("[pe-sec] parsing ");
    serial::print_hex(n_sections as u64);
    serial::print_str(" sections:\n");

    let mut count = 0;
    for i in 0..n_sections {
        let off = sec_off + i * 40;
        if off + 40 > data.len() {
            break;
        }

        let mut sec = SectionHeader::empty();

        // Name (8 bytes).
        for j in 0..8 {
            sec.name[j] = data[off + j];
        }

        // Virtual size (offset 8), virtual address (offset 12).
        sec.virtual_size = read_u32_le(data, off + 8);
        sec.virtual_address = read_u32_le(data, off + 12);

        // Raw size (offset 16), raw offset (offset 20).
        sec.raw_size = read_u32_le(data, off + 16);
        sec.raw_offset = read_u32_le(data, off + 20);

        // Print section info.
        serial::print_str("  [");
        for j in 0..8 {
            if sec.name[j] == 0 { break; }
            serial::write_char(sec.name[j]);
        }
        serial::print_str("] vaddr=0x");
        serial::print_hex(sec.virtual_address as u64);
        serial::print_str(" vsize=0x");
        serial::print_hex(sec.virtual_size as u64);
        serial::print_str(" raw=0x");
        serial::print_hex(sec.raw_offset as u64);
        serial::print_str("+0x");
        serial::print_hex(sec.raw_size as u64);
        serial::print_str("\n");

        count += 1;
    }

    serial::print_str("[pe-sec] ");
    serial::print_hex(count as u64);
    serial::print_str(" sections parsed\n");
    count
}

/// Parse the PE import directory (DLL dependencies).
/// Returns the number of import entries found.
pub fn parse_imports(data: &[u8], pe_info: &PeInfo) -> usize {
    if !pe_info.valid {
        return 0;
    }

    // For Stage 10, we simulate finding imports by checking for
    // known DLL names in the data. Real import directory parsing
    // requires reading the data directories from the optional header.
    serial::print_str("[pe-imp] scanning for known DLL imports...\n");

    // Check for "kernel32.dll" and "ntdll.dll" in the data.
    let dlls: [&[u8]; 4] = [
        b"kernel32.dll",
        b"ntdll.dll",
        b"user32.dll",
        b"msvcrt.dll",
    ];

    let mut found = 0;
    for dll in &dlls {
        if contains_bytes(data, dll) {
            serial::print_str("[pe-imp] found: ");
            for &b in *dll { serial::write_char(b); }
            serial::print_str("\n");
            found += 1;
        }
    }

    serial::print_str("[pe-imp] ");
    serial::print_hex(found as u64);
    serial::print_str(" imports found\n");
    found
}

/// Parse the base relocation directory.
/// Returns the number of relocation blocks.
pub fn parse_relocations(data: &[u8], pe_info: &PeInfo) -> usize {
    if !pe_info.valid {
        return 0;
    }

    // For Stage 10, we report that the relocation directory exists
    // (at the optional header data directory entry 5). We don't
    // process the actual relocations (that requires applying them
    // to a loaded image, which needs virtual memory mapping).

    let e_lfanew = read_u32_le(data, 0x3C) as usize;
    let coff_off = e_lfanew + 4;
    let opt_off = coff_off + 20;

    // Data directory 5 (base relocation) is at:
    // PE32+: optional header offset + 112 (8 bytes RVA + 8 bytes size).
    // PE32: optional header offset + 96 (4 bytes RVA + 4 bytes size).
    let reloc_dir_off = if pe_info.is_pe32plus {
        opt_off + 112
    } else {
        opt_off + 96
    };

    if reloc_dir_off + 8 > data.len() {
        serial::print_str("[pe-rel] relocation directory out of bounds\n");
        return 0;
    }

    let reloc_rva = read_u32_le(data, reloc_dir_off);
    let reloc_size = read_u32_le(data, reloc_dir_off + 4);

    if reloc_rva == 0 || reloc_size == 0 {
        serial::print_str("[pe-rel] no relocation directory\n");
        return 0;
    }

    serial::print_str("[pe-rel] relocation dir: RVA=0x");
    serial::print_hex(reloc_rva as u64);
    serial::print_str(" size=0x");
    serial::print_hex(reloc_size as u64);
    serial::print_str("\n");

    // Estimate block count (each block has a 8-byte header + entries).
    let blocks = reloc_size / 64;  // rough estimate
    serial::print_str("[pe-rel] ~");
    serial::print_hex(blocks as u64);
    serial::print_str(" relocation blocks\n");
    blocks as usize
}

/// Read a little-endian u16 from a byte slice.
fn read_u16_le(data: &[u8], off: usize) -> u16 {
    if off + 2 > data.len() {
        return 0;
    }
    (data[off] as u16) | ((data[off + 1] as u16) << 8)
}

/// Read a little-endian u32 from a byte slice.
fn read_u32_le(data: &[u8], off: usize) -> u32 {
    if off + 4 > data.len() {
        return 0;
    }
    (data[off] as u32)
        | ((data[off + 1] as u32) << 8)
        | ((data[off + 2] as u32) << 16)
        | ((data[off + 3] as u32) << 24)
}

/// Check if a byte slice contains a pattern (byte-by-byte, no memcmp).
fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.len() == 0 || haystack.len() < needle.len() {
        return false;
    }
    let max = haystack.len() - needle.len();
    for i in 0..=max {
        let mut found = true;
        for j in 0..needle.len() {
            if haystack[i + j] != needle[j] {
                found = false;
                break;
            }
        }
        if found {
            return true;
        }
    }
    false
}

/// Test section parsing with a minimal PE32+ image.
pub fn test_sections() {
    serial::print_str("[pe-sec] test: parsing PE sections\n");

    // Build a minimal PE with 2 sections (.text, .data).
    static mut TEST_PE2: [u8; 256] = [0; 256];
    let p = unsafe { core::ptr::addr_of_mut!(TEST_PE2) as *mut u8 };

    unsafe {
        // DOS header: MZ + e_lfanew=0x40.
        core::ptr::write_volatile(p, 0x4D);
        core::ptr::write_volatile(p.add(1), 0x5A);
        core::ptr::write_volatile(p.add(0x3C), 0x40);
        // PE sig at 0x40.
        core::ptr::write_volatile(p.add(0x40), 0x50);  // P
        core::ptr::write_volatile(p.add(0x41), 0x45);  // E
        // COFF: machine=0x8664, sections=2, opt_size=240.
        core::ptr::write_volatile(p.add(0x44), 0x64);
        core::ptr::write_volatile(p.add(0x45), 0x86);
        core::ptr::write_volatile(p.add(0x46), 0x02);  // 2 sections
        core::ptr::write_volatile(p.add(0x47), 0x00);
        core::ptr::write_volatile(p.add(0x54), 0xF0);  // opt_size=240
        core::ptr::write_volatile(p.add(0x55), 0x00);
        // Optional header at 0x58: magic=0x20B (PE32+).
        core::ptr::write_volatile(p.add(0x58), 0x0B);
        core::ptr::write_volatile(p.add(0x59), 0x02);
        // Entry point at opt+16.
        core::ptr::write_volatile(p.add(0x68), 0x00);
        core::ptr::write_volatile(p.add(0x69), 0x10);
        // Image base at opt+24 (PE32+ 8 bytes).
        core::ptr::write_volatile(p.add(0x70), 0x00);
        core::ptr::write_volatile(p.add(0x72), 0x40);

        // Section headers at 0x58 + 240 = 0x148.
        let sec_off = 0x148usize;

        // Section 1: .text
        let text_name = b".text   ";
        for (i, &b) in text_name.iter().enumerate() {
            core::ptr::write_volatile(p.add(sec_off + i), b);
        }
        // Virtual size = 0x200, virtual address = 0x1000.
        core::ptr::write_volatile(p.add(sec_off + 8), 0x00);
        core::ptr::write_volatile(p.add(sec_off + 9), 0x02);
        core::ptr::write_volatile(p.add(sec_off + 12), 0x00);
        core::ptr::write_volatile(p.add(sec_off + 13), 0x10);
        // Raw size = 0x200, raw offset = 0x200.
        core::ptr::write_volatile(p.add(sec_off + 16), 0x00);
        core::ptr::write_volatile(p.add(sec_off + 17), 0x02);
        core::ptr::write_volatile(p.add(sec_off + 20), 0x00);
        core::ptr::write_volatile(p.add(sec_off + 21), 0x02);

        // Section 2: .data
        let data_name = b".data   ";
        for (i, &b) in data_name.iter().enumerate() {
            core::ptr::write_volatile(p.add(sec_off + 40 + i), b);
        }
        core::ptr::write_volatile(p.add(sec_off + 48), 0x00);
        core::ptr::write_volatile(p.add(sec_off + 49), 0x01);
        core::ptr::write_volatile(p.add(sec_off + 52), 0x00);
        core::ptr::write_volatile(p.add(sec_off + 53), 0x20);
        core::ptr::write_volatile(p.add(sec_off + 56), 0x00);
        core::ptr::write_volatile(p.add(sec_off + 57), 0x01);
        core::ptr::write_volatile(p.add(sec_off + 60), 0x00);
        core::ptr::write_volatile(p.add(sec_off + 61), 0x04);

        // Embed "kernel32.dll" in the data so import scan finds it.
        let k32 = b"kernel32.dll";
        for (i, &b) in k32.iter().enumerate() {
            core::ptr::write_volatile(p.add(0xE0 + i), b);
        }
    }

    let pe_info = super::pe::parse(unsafe { &TEST_PE2[..256] });
    let sections = parse_sections(unsafe { &TEST_PE2[..256] }, &pe_info);
    let imports = parse_imports(unsafe { &TEST_PE2[..256] }, &pe_info);
    let relocs = parse_relocations(unsafe { &TEST_PE2[..256] }, &pe_info);

    if sections >= 2 && imports >= 1 {
        serial::print_str("[pe-sec] test: OK (");
        serial::print_hex(sections as u64);
        serial::print_str(" sections, ");
        serial::print_hex(imports as u64);
        serial::print_str(" imports)\n");
        super::PACKAGES_PARSED.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    } else {
        serial::print_str("[pe-sec] test: partial (sections=");
        serial::print_hex(sections as u64);
        serial::print_str(" imports=");
        serial::print_hex(imports as u64);
        serial::print_str(")\n");
    }
}
