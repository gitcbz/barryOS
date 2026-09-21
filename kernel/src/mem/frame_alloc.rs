//! Bitmap physical page-frame allocator.
//!
//! One bit per 4 KiB frame.  Bit = 1 → frame is used (or not usable at all).
//! Bit = 0 → frame is free for allocation.
//!
//! The bitmap covers the first `MAX_FRAMES` frames (= `MAX_FRAMES * 4 KiB`
//! of physical address space).  With 8 KiB bitmap this covers 256 MiB,
//! enough for QEMU (128–256 MiB) and small VMware VMs.

use super::efi::{PAGE_SIZE, PAGE_SHIFT};
use super::memmap::MemMap;
use crate::serial;

/// 8 KiB bitmap = 65,536 frames = 256 MiB of addressable physical RAM.
pub const BITMAP_BYTES: usize = 8 * 1024;
pub const MAX_FRAMES: usize = BITMAP_BYTES * 8;

/// Static bitmap storage in .bss.  Accessed only via raw pointers to
/// avoid Rust 2024 `static mut` reference issues (see DECISIONS D12).
static mut BITMAP: [u8; BITMAP_BYTES] = [0; BITMAP_BYTES];

/// Return a raw pointer to the bitmap (avoids creating a reference).
#[inline]
fn bitmap_ptr() -> *mut u8 {
    unsafe { core::ptr::addr_of_mut!(BITMAP) as *mut u8 }
}

pub struct BitmapFrameAllocator {
    frames_total:  usize,
    frames_used:   usize,
    frames_usable: usize,
    next_hint:     usize,
}

impl BitmapFrameAllocator {
    pub const fn new() -> Self {
        Self { frames_total: 0, frames_used: 0, frames_usable: 0, next_hint: 0 }
    }

    /// Initialize: mark everything as used, then clear bits for usable regions.
    pub fn init(&mut self, mm: &MemMap) {
        let bp = bitmap_ptr();
        // Fill bitmap with 0xFF (all used) via a manual loop (avoids memset).
        unsafe {
            let mut i = 0;
            while i < BITMAP_BYTES {
                bp.add(i).write_volatile(0xFF);
                i += 1;
            }
        }
        self.frames_total = MAX_FRAMES;

        // Clear bits for usable (ConventionalMemory) regions.
        for i in 0..mm.count {
            let r = mm.regions[i];
            if r.kind.is_usable() {
                let first = (r.start >> PAGE_SHIFT) as usize;
                let n = r.pages as usize;
                for j in 0..n {
                    let f = first + j;
                    if f < MAX_FRAMES {
                        clear_bit(f);
                        self.frames_usable += 1;
                    }
                }
            }
        }

        // Mark the kernel image (1 MiB..1 MiB + 256 pages) as used.
        for f in (0x100_000 / PAGE_SIZE)..(0x100_000 / PAGE_SIZE + 256) {
            if f < MAX_FRAMES {
                set_bit(f);
            }
        }

        self.frames_used = self.frames_total - self.frames_usable;
    }

    /// Allocate one 4 KiB frame.  Returns the physical address or 0 on OOM.
    pub fn alloc(&mut self) -> u64 {
        let start = self.next_hint;
        for i in 0..MAX_FRAMES {
            let idx = (start + i) % MAX_FRAMES;
            if !test_bit(idx) {
                set_bit(idx);
                self.frames_used += 1;
                self.next_hint = (idx + 1) % MAX_FRAMES;
                return (idx as u64) << PAGE_SHIFT;
            }
        }
        0
    }

    /// Allocate `n` contiguous frames.  Returns physical address or 0.
    pub fn alloc_contig(&mut self, n: usize) -> u64 {
        if n == 0 { return 0; }
        let mut run = 0;
        let mut run_start = 0;
        for i in 0..MAX_FRAMES {
            if !test_bit(i) {
                if run == 0 { run_start = i; }
                run += 1;
                if run == n {
                    for j in 0..n { set_bit(run_start + j); }
                    self.frames_used += n;
                    self.next_hint = (run_start + n) % MAX_FRAMES;
                    return (run_start as u64) << PAGE_SHIFT;
                }
            } else {
                run = 0;
            }
        }
        0
    }

    /// Free a previously-allocated frame.
    pub unsafe fn free(&mut self, addr: u64) {
        let idx = (addr as usize) >> PAGE_SHIFT;
        if idx < MAX_FRAMES && test_bit(idx) {
            clear_bit(idx);
            self.frames_used -= 1;
            if idx < self.next_hint {
                self.next_hint = idx;
            }
        }
    }

    pub fn print_stats(&self) {
        serial::print_str("[mem] frame allocator: usable ");
        serial::print_hex(self.frames_usable as u64);
        serial::print_str(" / ");
        serial::print_hex(self.frames_total as u64);
        serial::print_str(" frames, used ");
        serial::print_hex(self.frames_used as u64);
        serial::print_str(" (");
        serial::print_hex((self.frames_used * 4) as u64);
        serial::print_str(" KiB)\n");
    }

    pub fn usable_pages(&self) -> usize { self.frames_usable }
    pub fn used_pages(&self) -> usize { self.frames_used }
}

#[inline]
fn set_bit(idx: usize) {
    unsafe {
        let p = bitmap_ptr().add(idx / 8);
        p.write_volatile(p.read_volatile() | (1 << (idx % 8)));
    }
}
#[inline]
fn clear_bit(idx: usize) {
    unsafe {
        let p = bitmap_ptr().add(idx / 8);
        p.write_volatile(p.read_volatile() & !(1 << (idx % 8)));
    }
}
#[inline]
fn test_bit(idx: usize) -> bool {
    unsafe { (bitmap_ptr().add(idx / 8).read_volatile() >> (idx % 8)) & 1 == 1 }
}
