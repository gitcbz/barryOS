//! Memory map parsing.
//!
//! On UEFI: read the EFI_MEMORY_DESCRIPTOR array passed via BootInfo.
//! On BIOS: synthesize a minimal usable region (no E820 in stage2 yet).

use super::efi::{EfiMemoryDescriptor, MemoryType};
use crate::serial;

/// Maximum number of memory regions we track.
pub const MAX_REGIONS: usize = 32;

/// A normalized memory region.
#[derive(Clone, Copy)]
pub struct Region {
    pub start:    u64,
    pub pages:    u64,
    pub kind:     MemoryType,
}

/// Parsed memory map handed to the frame allocator.
pub struct MemMap {
    pub regions:  [Region; MAX_REGIONS],
    pub count:    usize,
    pub source:   &'static str,
    pub total_pages:   u64,
    pub usable_pages:  u64,
}

impl MemMap {
    pub const fn new() -> Self {
        // Use a sentinel region (start=0, pages=0) for "empty".
        let blank = Region { start: 0, pages: 0, kind: MemoryType::Reserved };
        Self {
            regions: [blank; MAX_REGIONS],
            count: 0,
            source: "",
            total_pages: 0,
            usable_pages: 0,
        }
    }

    pub fn push(&mut self, r: Region) {
        if self.count < MAX_REGIONS {
            self.total_pages += r.pages;
            if r.kind.is_usable() {
                self.usable_pages += r.pages;
            }
            self.regions[self.count] = r;
            self.count += 1;
        }
    }

    pub fn print(&self) {
        serial::print_str("\n[mem] memory map (source: ");
        serial::print_str(self.source);
        serial::print_str("), ");
        serial::print_hex(self.count as u64);
        serial::print_str(" regions\n");
        for i in 0..self.count {
            let r = self.regions[i];
            serial::print_str("  [");
            serial::print_hex(i as u64);
            serial::print_str("] ");
            serial::print_str(r.kind.name());
            serial::print_str(" 0x");
            serial::print_hex(r.start);
            serial::print_str(" +");
            serial::print_hex(r.pages);
            serial::print_str("p\n");
        }
    }
}

/// BootInfo mirror (must match `boot/uefi/efi_main.c` and `bootinfo.rs`).
#[repr(C)]
struct BootInfoC {
    magic:               u64,
    framebuffer_addr:    u64,
    framebuffer_size:    u64,
    width:               u32,
    height:              u32,
    pixels_per_scanline: u32,
    pixel_format:        u32,
    memmap:              u64,
    memmap_size:         u64,
    memmap_desc_size:    u64,
    memmap_desc_version: u64,
}

const BARRYOS_BOOTINFO_MAGIC: u64 = 0x534F_5252_4142; // 'BARROS'

/// Parse the memory map from the BootInfo pointer (UEFI) or synthesize (BIOS).
/// Writes into the provided MemMap (avoids large struct return-by-value).
pub fn parse_into(boot_info: usize, out: &mut MemMap) {
    if boot_info == 0 {
        bios_fallback_into(out);
        return;
    }
    let bi: &BootInfoC = unsafe { &*(boot_info as *const BootInfoC) };
    if bi.magic != BARRYOS_BOOTINFO_MAGIC {
        serial::print_str("[mem] WARN: BootInfo magic mismatch, using BIOS fallback\n");
        bios_fallback_into(out);
        return;
    }
    parse_uefi_into(bi, out)
}

fn parse_uefi_into(bi: &BootInfoC, out: &mut MemMap) {
    out.source = "UEFI";
    if bi.memmap == 0 || bi.memmap_size == 0 || bi.memmap_desc_size == 0 {
        serial::print_str("[mem] WARN: empty UEFI memmap, using BIOS fallback\n");
        bios_fallback_into(out);
        return;
    }
    let base = bi.memmap as *const u8;
    let n = (bi.memmap_size / bi.memmap_desc_size) as usize;
    for i in 0..n {
        let p = unsafe { base.add(i * bi.memmap_desc_size as usize) as *const EfiMemoryDescriptor };
        let d = unsafe { p.read_volatile() };
        out.push(Region {
            start: d.physical_start,
            pages: d.number_of_pages,
            kind:  d.region_type(),
        });
    }
}

/// BIOS fallback: assume 64 MiB usable, mark kernel area as used.
pub fn bios_fallback() -> MemMap {
    let mut m = MemMap::new();
    bios_fallback_into(&mut m);
    m
}

fn bios_fallback_into(out: &mut MemMap) {
    out.source = "BIOS";
    // Region 0: kernel image (1 MiB .. 2 MiB) — LoaderData (not allocatable).
    out.push(Region {
        start: 0x10_0000,
        pages: 256,
        kind:  MemoryType::LoaderData,
    });
    // Region 1: free RAM (2 MiB .. 64 MiB) = 62 MiB = 15872 pages.
    out.push(Region {
        start: 0x20_0000,
        pages: 15872,
        kind:  MemoryType::ConventionalMemory,
    });
}
