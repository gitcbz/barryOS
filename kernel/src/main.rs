//! barryOS kernel — Stage 1 entry point.
//!
//! Loaded at physical 0x00100000 by either:
//!   * the BIOS stage-2 bootloader (real -> protected -> long mode), or
//!   * the UEFI loader (`boot/uefi/efi_main.c`).
//!
//! Both arrive in 64-bit long mode with interrupts off, a valid stack at
//! 0x00200000, and RDI = pointer to a `BootInfo` struct (NULL on BIOS).
//!
//! Stage 1 goal: print `barryOS booted` to the serial port (COM1) and to
//! the 80x25 VGA text framebuffer, then halt cleanly.

#![no_std]
#![no_main]

mod vga;
mod serial;
mod panic;
mod bootinfo;

use core::sync::atomic::Ordering;

/// Stack top provided by both boot paths (must match stage2.asm / efi_main.c).
const STACK_TOP: usize = 0x0020_0000;

/// Naked entry. Placed in `.text.entry` so the linker script puts it first.
/// RDI holds the BootInfo pointer (NULL on BIOS).  We hand it off to
/// `rust_main` as the System-V first argument register.
#[unsafe(naked)]
#[link_section = ".text.entry"]
#[no_mangle]
pub unsafe extern "C" fn _start() -> ! {
    core::arch::naked_asm!(
        "cli",
        "mov rsp, {stk}",
        "xor rbp, rbp",
        "and rsp, 0xFFFFFFFFFFFFFFF0",   // 16-byte align
        "call {main}",
        "cli",
        "1: hlt",
        "jmp 1b",
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
    serial::print_str("[ok] Stage 1 complete; halting.\n");

    vga::clear();
    vga::print_str("barryOS booted\n");
    vga::print_str("self-developed x86_64 kernel\n");
    vga::print_str("[Stage 1] boot path: ");
    vga::print_str(boot_kind);
    vga::print_str("\n");

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
