//! barryOS kernel — Stage 2 entry point.
//!
//! Loaded at physical 0x00100000 by either:
//!   * the BIOS stage-2 bootloader (real -> protected -> long mode), or
//!   * the UEFI loader (`boot/uefi/efi_main.c`).
//!
//! Both arrive in 64-bit long mode with interrupts off, a valid stack at
//! 0x00200000, and RDI = pointer to a `BootInfo` struct (NULL on BIOS).
//!
//! Stage 2 goal: boot to `barryOS booted`, then initialize the memory
//! subsystem (frame allocator, paging, heap) and prove it works.

#![no_std]
#![no_main]

// Enable the `alloc` crate so Vec/Box work in no_std.
extern crate alloc;

mod vga;
mod serial;
mod panic;
mod bootinfo;
mod mem;
mod interrupts;
mod proc;
mod fs;
mod dev;
mod wm;
mod apps;

use core::sync::atomic::Ordering;

/// Stack top provided by both boot paths (must match stage2.asm / efi_main.c).
const STACK_TOP: usize = 0x0020_0000;

/// Naked entry. Placed in `.text.entry` so the linker script puts it first.
/// RDI holds the BootInfo pointer (NULL on BIOS).
#[unsafe(naked)]
#[link_section = ".text.entry"]
#[no_mangle]
pub unsafe extern "C" fn _start() -> ! {
    core::arch::naked_asm!(
        "cli",
        "mov rsp, {stk}",
        "xor rbp, rbp",
        "and rsp, 0xFFFFFFFFFFFFFFF0",
        "call {main}",
        "cli",
        "2:",
        "hlt",
        "jmp 2b",
        stk = const STACK_TOP,
        main = sym rust_main,
    );
}

/// Rust-level entry.  `boot_info` is RDI from the bootloader
/// (0 on BIOS, pointer to BootInfo on UEFI).
#[no_mangle]
pub unsafe extern "C" fn rust_main(boot_info: usize) -> ! {
    bootinfo::BOOT_INFO.store(boot_info as u64, Ordering::Relaxed);

    // Zero the .bss section.
    zero_bss();

    // Initialize output devices.
    serial::init();
    vga::init();

    let boot_kind = if boot_info == 0 { "BIOS" } else { "UEFI" };

    serial::print_str("\n");
    serial::print_str("========================================\n");
    serial::print_str("  barryOS - self-developed x86_64 kernel\n");
    serial::print_str("========================================\n");
    serial::print_str("[boot] path: ");
    serial::print_str(boot_kind);
    serial::print_str("\n");
    serial::print_str("[boot] kernel entry @ 0x");
    serial::print_hex(KERNEL_LOAD as u64);
    serial::print_str("\n");
    serial::print_str("barryOS booted\n");
    serial::print_str("[stage2] initializing memory subsystem...\n");

    // Initialize the memory subsystem (frame allocator + paging + heap).
    mem::init(boot_info);

    serial::print_str("[stage2] memory subsystem online.\n");

    // Stage 3: interrupts (IDT + GDT/TSS + PIC + PIT).
    serial::print_str("[stage3] initializing interrupt subsystem...\n");
    interrupts::irq::init_pit();
    interrupts::init();

    serial::print_str("[stage3] interrupt subsystem online.\n");

    // Stage 4: processes + scheduler + syscalls.
    serial::print_str("[stage4] initializing process subsystem...\n");
    proc::thread::set_frame_allocator(mem::frame_allocator());
    proc::init();

    serial::print_str("[stage4] process subsystem online.\n");

    // Stage 5: VFS + filesystem.
    serial::print_str("[stage5] initializing filesystem subsystem...\n");
    fs::init();

    serial::print_str("[stage5] filesystem subsystem online.\n");

    // Stage 6: device drivers (framebuffer + keyboard).
    serial::print_str("[stage6] initializing device drivers...\n");
    dev::init(boot_info);

    serial::print_str("[stage6] device drivers online.\n");

    // Stage 7: window manager + GUI.
    serial::print_str("[stage7] initializing window manager...\n");
    wm::init();

    serial::print_str("[stage7] window manager online.\n");

    // Stage 8: desktop applications (terminal, file manager, system info).
    serial::print_str("[stage8] initializing desktop apps...\n");
    apps::init();

    serial::print_str("[stage8] desktop apps online.\n");

    // Print final diagnostics.
    proc::process::print_table();
    proc::scheduler::print_stats();
    fs::vfs::print_table();

    serial::print_str("[ok] Stage 8 complete; halting.\n");

    // VGA summary
    vga::clear();
    vga::print_str("barryOS booted [Stage 8]\n");
    vga::print_str("self-developed x86_64 kernel\n");
    vga::print_str("[boot] path: ");
    vga::print_str(boot_kind);
    vga::print_str("\n");
    vga::print_str("[mem] frame alloc + paging + heap OK\n");
    vga::print_str("[irq] IDT + PIC + PIT OK\n");
    vga::print_str("[proc] PCB + scheduler + syscall OK\n");
    vga::print_str("[fs] VFS + RAMfs OK\n");
    vga::print_str("[dev] framebuffer + keyboard OK\n");
    vga::print_str("[wm] windows + font + dock OK\n");
    vga::print_str("[apps] terminal + files + sysinfo OK\n");

    halt_forever();
}

/// Physical load address of the kernel (must match `linker.ld`).
const KERNEL_LOAD: usize = 0x0010_0000;

/// Zero the kernel's .bss (linker script exposes `__bss_start` / `__bss_end`).
unsafe fn zero_bss() {
    extern "C" {
        static mut __bss_start: u8;
        static mut __bss_end: u8;
    }
    let start = core::ptr::addr_of_mut!(__bss_start) as *mut u8;
    let end = core::ptr::addr_of_mut!(__bss_end) as *mut u8;
    let mut p = start;
    while p < end {
        p.write_volatile(0);
        p = p.add(1);
    }
}

unsafe fn halt_forever() -> ! {
    loop {
        core::arch::asm!("cli; hlt", options(nostack, nomem, preserves_flags));
    }
}
