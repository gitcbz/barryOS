//! barryOS kernel — Stage 2 memory subsystem.
//!
//! Modules:
//! - `efi`:          EFI_MEMORY_DESCRIPTOR + type constants (matches UEFI spec).
//! - `memmap`:       parse BootInfo memmap (UEFI) or synthesize fallback (BIOS).
//! - `frame_alloc`:  bitmap physical page-frame allocator.
//! - `paging`:       x86_64 4-level page tables we own + CR3 switch.
//! - `heap`:         linked-list free-list heap + #[global_allocator].

pub mod efi;
pub mod memmap;
pub mod frame_alloc;
pub mod paging;
pub mod heap;

use core::sync::atomic::AtomicBool;
use crate::serial;
use memmap::MemMap;

pub static INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Static MemMap (avoids large struct return-by-value which was hanging
/// in the BIOS boot path — see DECISIONS D12).  Stage 2 is single-threaded.
static mut BI: MemMap = MemMap::new();

/// Static frame allocator — globally accessible for proc/syscall subsystems.
static mut FA: frame_alloc::BitmapFrameAllocator = frame_alloc::BitmapFrameAllocator::new();

/// Get a mutable reference to the global frame allocator.
pub fn frame_allocator() -> &'static mut frame_alloc::BitmapFrameAllocator {
    unsafe { &mut *core::ptr::addr_of_mut!(FA) }
}

/// Initialize the full memory subsystem.
pub fn init(boot_info: usize) {
    serial::print_str("[mem] step 1: parse memmap\n");
    unsafe { memmap::parse_into(boot_info, &mut BI); }
    unsafe { BI.print(); }

    serial::print_str("[mem] step 2: frame allocator\n");
    unsafe { FA.init(&BI); }
    unsafe { FA.print_stats(); }

    serial::print_str("[mem] step 3: paging remap\n");
    unsafe { paging::remap(&mut FA); }
    paging::print_stats();

    serial::print_str("[mem] step 4: heap init\n");
    heap::init(frame_allocator());
    heap::print_stats();

    serial::print_str("[mem] step 5: heap smoke test\n");
    heap::smoke_test();

    INITIALIZED.store(true, core::sync::atomic::Ordering::Release);
    serial::print_str("[mem] all steps done\n");
}
