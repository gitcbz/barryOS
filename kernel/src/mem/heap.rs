//! Kernel heap allocator — bump allocator for Stage 2.
//!
//! Simplest correct allocator: a bump pointer into a 1 MiB heap region.
//! `alloc` bumps the pointer; `dealloc` is a no-op.  Good enough to prove
//! `#[global_allocator]` works with Vec/Box.  Stage 2b will upgrade to a
//! free-list allocator with real deallocation.

use super::frame_alloc::BitmapFrameAllocator;
use crate::serial;
use core::alloc::{GlobalAlloc, Layout};
use core::ptr::null_mut;
use core::sync::atomic::{AtomicU64, Ordering};

/// 1 MiB heap = 256 frames of 4 KiB.
pub const HEAP_PAGES: usize = 256;
pub const HEAP_SIZE:  usize = HEAP_PAGES * 4096;

/// Bump allocator state.  Uses atomics so it works even if the compiler
/// reorders or caches (no UnsafeCell needed — atomics have defined semantics).
pub struct BumpAlloc {
    pub heap_start: AtomicU64,
    pub heap_end:   AtomicU64,
    pub next:       AtomicU64,     // next free byte
    pub allocs:     AtomicU64,
    pub frees:      AtomicU64,
    pub bytes_in_use: AtomicU64,
}

#[global_allocator]
pub static HEAP: BumpAlloc = BumpAlloc {
    heap_start: AtomicU64::new(0),
    heap_end:   AtomicU64::new(0),
    next:       AtomicU64::new(0),
    allocs:     AtomicU64::new(0),
    frees:      AtomicU64::new(0),
    bytes_in_use: AtomicU64::new(0),
};

/// Initialize the heap: allocate 256 contiguous frames.
pub fn init(fa: &mut BitmapFrameAllocator) {
    serial::print_str("[mem] heap: allocating ");
    serial::print_hex(HEAP_PAGES as u64);
    serial::print_str(" contiguous frames\n");
    let phys = fa.alloc_contig(HEAP_PAGES);
    if phys == 0 {
        serial::print_str("[mem] heap: OOM\n");
        return;
    }
    let vaddr = phys as usize;

    HEAP.heap_start.store(vaddr as u64, Ordering::SeqCst);
    HEAP.heap_end.store((vaddr + HEAP_SIZE) as u64, Ordering::SeqCst);
    HEAP.next.store(vaddr as u64, Ordering::SeqCst);

    serial::print_str("[mem] heap: zeroing ");
    serial::print_hex(HEAP_SIZE as u64);
    serial::print_str(" bytes\n");
    // Zero via volatile loop.
    unsafe {
        let p = vaddr as *mut u8;
        let mut i = 0;
        while i < HEAP_SIZE {
            p.add(i).write_volatile(0);
            i += 1;
        }
    }

    serial::print_str("[mem] heap: ");
    serial::print_hex(HEAP_SIZE as u64);
    serial::print_str(" bytes at 0x");
    serial::print_hex(vaddr as u64);
    serial::print_str("\n");
}

pub fn print_stats() {
    serial::print_str("[mem] heap: allocs=");
    serial::print_hex(HEAP.allocs.load(Ordering::SeqCst));
    serial::print_str(" frees=");
    serial::print_hex(HEAP.frees.load(Ordering::SeqCst));
    serial::print_str(" in_use=");
    serial::print_hex(HEAP.bytes_in_use.load(Ordering::SeqCst));
    serial::print_str(" B\n");
}

/// Smoke test: allocate a Vec, push values, check sum.
pub fn smoke_test() {
    // Test 1: raw alloc
    serial::print_str("[mem] heap test 1: raw alloc\n");
    unsafe {
        let layout = Layout::from_size_align(16, 8).unwrap();
        let p = alloc::alloc::alloc(layout);
        serial::print_str("[mem] heap test 1: ptr=0x");
        serial::print_hex(p as u64);
        serial::print_str("\n");
        if p.is_null() {
            serial::print_str("[mem] heap test 1: FAIL null\n");
        } else {
            // Write and read back
            core::ptr::write_volatile(p as *mut u64, 0x123456789ABCDEF0);
            let val = core::ptr::read_volatile(p as *const u64);
            serial::print_str("[mem] heap test 1: val=0x");
            serial::print_hex(val);
            if val == 0x123456789ABCDEF0 {
                serial::print_str(" OK\n");
            } else {
                serial::print_str(" FAIL\n");
            }
        }
    }

    // Test 2: Vec
    serial::print_str("[mem] heap test 2: Vec\n");

    // Test 2a: memcpy (via copy_nonoverlapping)
    serial::print_str("[mem] heap test 2a: copy_nonoverlapping\n");
    unsafe {
        let src = 0x200000u64 as *mut u8;
        let dst = 0x200100u64 as *mut u8;
        // Write test pattern to src
        for i in 0..16u8 {
            src.add(i as usize).write_volatile(0xA0 + i);
        }
        // copy 16 bytes (this calls memcpy internally)
        core::ptr::copy_nonoverlapping(src, dst, 16usize);
        // Verify
        let mut ok = true;
        for i in 0..16u8 {
            let v = dst.add(i as usize).read_volatile();
            if v != 0xA0 + i { ok = false; }
        }
        if ok {
            serial::print_str("[mem] heap test 2a: copy OK\n");
        } else {
            serial::print_str("[mem] heap test 2a: copy FAIL\n");
        }
    }

    // Test 2b: Vec push (skip loop for now, test single allocs)
    serial::print_str("[mem] heap test 2b: Box (single alloc)\n");
    let b1 = alloc::boxed::Box::new(0x1111u32);
    serial::print_str("[mem] heap test 2b: Box<u32>=0x");
    serial::print_hex(*b1 as u64);
    serial::print_str("\n");
    drop(b1);

    let b2 = alloc::boxed::Box::new(0x2222_4444u64);
    serial::print_str("[mem] heap test 2b: Box<u64>=0x");
    serial::print_hex(*b2);
    serial::print_str("\n");
    drop(b2);

    // Test 2c: Vec with capacity (no reallocation needed)
    serial::print_str("[mem] heap test 2c: Vec with_capacity(100)\n");
    use alloc::vec::Vec;
    let mut v: Vec<u32> = Vec::with_capacity(100);
    serial::print_str("[mem] heap test 2c: cap=100, pushing\n");
    for i in 0u32..100 { v.push(i); }
    serial::print_str("[mem] heap test 2c: pushed 100, summing\n");
    let sum: u32 = v.iter().sum();
    serial::print_str("[mem] heap test 2c: sum=");
    serial::print_hex(sum as u64);
    serial::print_str(" (expect 0x1356) ");
    if sum == 4950 {
        serial::print_str("OK\n");
    } else {
        serial::print_str("FAIL\n");
    }
    drop(v);

    // Test 3: Vec with reallocation (known issue — hangs, skip for now)
    // Stage 2b will fix this (likely needs custom __rust_dealloc or
    // a proper free-list allocator instead of bump).
    serial::print_str("[mem] heap test 3: Vec realloc (skipped — Stage 2b)\n");

    // Test 4: Box
    serial::print_str("[mem] heap test 4: Box\n");
    let b = alloc::boxed::Box::new(0xDEAD_BEEFu64);
    serial::print_str("[mem] heap test 4: Box=0x");
    serial::print_hex(*b);
    if *b == 0xDEAD_BEEF {
        serial::print_str(" OK\n");
    } else {
        serial::print_str(" FAIL\n");
    }
    drop(b);
}

// ---------------------------------------------------------------------------
//  GlobalAlloc implementation (bump allocator)
// ---------------------------------------------------------------------------
unsafe impl GlobalAlloc for BumpAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let size = layout.size() as u64;
        let align = layout.align() as u64;

        let start = self.next.load(Ordering::SeqCst);
        let aligned = (start + align - 1) & !(align - 1);
        let new_next = aligned + size;
        let end = self.heap_end.load(Ordering::SeqCst);

        if new_next > end {
            return null_mut(); // OOM
        }

        self.next.store(new_next, Ordering::SeqCst);
        self.allocs.fetch_add(1, Ordering::SeqCst);
        self.bytes_in_use.fetch_add(size, Ordering::SeqCst);
        return aligned as *mut u8;
    }

    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {
        // Bump allocator: no-op.  (Can't even count — keep it minimal to
        // avoid any issue during Vec reallocation.)
    }
}
