//! Boot info plumbing from the bootloader.
//!
//! Stage 1 only stores the raw pointer the UEFI loader passed (NULL on BIOS).
//! Stage 2 will dereference it as `&BootInfo` once paging is set up.

use core::sync::atomic::AtomicU64;

pub static BOOT_INFO: AtomicU64 = AtomicU64::new(0);

#[repr(C)]
pub struct BootInfo {
    pub magic: u64,
    pub framebuffer_addr: u64,
    pub framebuffer_size: u64,
    pub width: u32,
    pub height: u32,
    pub pixels_per_scanline: u32,
    pub pixel_format: u32,
    pub memmap: u64,
    pub memmap_size: u64,
    pub memmap_desc_size: u64,
    pub memmap_desc_version: u64,
}
