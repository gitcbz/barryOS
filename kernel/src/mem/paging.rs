//! x86_64 four-level page tables — owned by the kernel.
//!
//! After `remap()` the kernel runs on page tables it allocated itself
//! (not the bootloader's).  We identity-map the first 4 GiB with 1 GiB
//! pages so all code/data/MMIO below 4 GiB stays accessible, and we
//! switch CR3 to our new PML4.

use super::frame_alloc::BitmapFrameAllocator;
use crate::serial;
use core::sync::atomic::{AtomicU64, Ordering};

const ENTRY_COUNT: usize = 512;

/// Page table entry flags (x86_64).
const FLAG_PRESENT:     u64 = 1 << 0;
const FLAG_WRITABLE:    u64 = 1 << 1;
const FLAG_HUGE_PAGE:   u64 = 1 << 7;  // PS bit — 1 GiB / 2 MiB page

/// A 4096-byte aligned page table (512 entries).
#[repr(C, align(4096))]
struct PageTable {
    entries: [u64; ENTRY_COUNT],
}

/// Our PML4 and PDPT0 — `static mut`: `remap()` fills them in at runtime.
/// They used to be immutable statics written through `addr_of!() as *mut`,
/// which is UB; the `write_volatile` below happened to keep the stores alive,
/// but the same pattern silently lost every store in interrupts/gdt.rs.
static mut PML4:  PageTable = PageTable { entries: [0; ENTRY_COUNT] };
static mut PDPT0: PageTable = PageTable { entries: [0; ENTRY_COUNT] };

static PML4_PHYS: AtomicU64 = AtomicU64::new(0);
static MAP_BYTES: AtomicU64 = AtomicU64::new(0);

/// Build our own page tables and switch CR3.
///
/// Maps the first 4 GiB of physical memory 1:1 using 1 GiB pages.
/// After the switch the kernel continues executing at its current
/// physical address (0x100000) which is still mapped.
pub fn remap(_fa: &mut BitmapFrameAllocator) {
    // Get raw pointers to our static page tables.
    let pml4  = core::ptr::addr_of_mut!(PML4)  as *mut u64;
    let pdpt0 = core::ptr::addr_of_mut!(PDPT0) as *mut u64;

    unsafe {
        // Zero PML4 and PDPT0 via volatile writes (avoid memset).
        for i in 0..ENTRY_COUNT {
            pml4.add(i).write_volatile(0);
            pdpt0.add(i).write_volatile(0);
        }

        // PML4[0] -> PDPT0 (present + writable)
        let pdpt0_phys = pdpt0 as u64;
        pml4.add(0).write_volatile(pdpt0_phys | FLAG_PRESENT | FLAG_WRITABLE);

        // PML4[511] -> PDPT0 too (high canonical mapping, for Stage 2b).
        pml4.add(511).write_volatile(pdpt0_phys | FLAG_PRESENT | FLAG_WRITABLE);

        // PDPT0[0..4] -> four 1 GiB pages covering 0..4 GiB
        for i in 0..4u64 {
            pdpt0.add(i as usize).write_volatile(
                (i << 30) | FLAG_PRESENT | FLAG_WRITABLE | FLAG_HUGE_PAGE,
            );
        }

        let pml4_phys = pml4 as u64;
        PML4_PHYS.store(pml4_phys, Ordering::Release);

        // Read current CR3 so we can validate the switch.
        let old_cr3 = read_cr3();

        // Switch!  After this, the CPU uses our page tables.
        write_cr3(pml4_phys);

        // Flush caches + TLB.
        core::arch::asm!("wbinvd", options(nostack, nomem, preserves_flags));

        let new_cr3 = read_cr3();
        serial::print_str("[mem] paging: CR3 0x");
        serial::print_hex(old_cr3);
        serial::print_str(" -> 0x");
        serial::print_hex(new_cr3);
        serial::print_str("\n");

        MAP_BYTES.store(4 * 1024 * 1024 * 1024u64, Ordering::Release);
    }
}

pub fn print_stats() {
    let pml4 = PML4_PHYS.load(Ordering::Acquire);
    let mb = MAP_BYTES.load(Ordering::Acquire);
    serial::print_str("[mem] paging: PML4 @ 0x");
    serial::print_hex(pml4);
    serial::print_str(", identity ");
    serial::print_hex(mb / (1024 * 1024));
    serial::print_str(" MiB (1 GiB pages)\n");
}

#[inline]
fn read_cr3() -> u64 {
    let v: u64;
    unsafe { core::arch::asm!("mov {}, cr3", out(reg) v, options(nostack, nomem, preserves_flags)); }
    v
}

#[inline]
unsafe fn write_cr3(v: u64) {
    core::arch::asm!("mov cr3, {}", in(reg) v, options(nostack, nomem, preserves_flags));
}
