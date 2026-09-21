//! CPU exception handlers (Rust side).
//!
//! Called by the naked assembly stubs in `handlers.rs` after registers are
//! saved.  Prints the exception name + register dump to serial + VGA, then
//! halts (Stage 3 doesn't have a task scheduler yet).

use super::handlers::Registers;
use crate::{serial, vga};

/// Exception names indexed by vector number.
const EXCEPTION_NAMES: [&str; 32] = [
    "#DE Divide Error",         "#DB Debug",                "NMI Non-Maskable",
    "#BP Breakpoint",           "#OF Overflow",             "#BR Bound Range",
    "#UD Invalid Opcode",       "#NM Device Not Available", "#DF Double Fault",
    "#MX Segment Overrun",      "#TS Invalid TSS",          "#NP Not Present",
    "#SS Stack Fault",          "#GP General Protection",   "#PF Page Fault",
    "Reserved",                 "#MF x87 FPE",              "#AC Alignment Check",
    "#MC Machine Check",        "#XM SIMD FPE",             "#VE Virtualization",
    "Reserved", "Reserved", "Reserved", "Reserved", "Reserved",
    "Reserved", "Reserved", "Reserved", "Reserved", "Reserved", "Reserved",
];

/// Rust entry point for all CPU exceptions (called by `exception_handler`
/// symbol referenced in handlers.rs).
#[no_mangle]
pub extern "C" fn exception_handler(regs: &Registers) {
    super::EXCEPTIONS.fetch_add(1, core::sync::atomic::Ordering::Relaxed);

    let name = EXCEPTION_NAMES.get(regs.int_no as usize).copied().unwrap_or("Unknown");

    serial::print_str("\n!!! CPU EXCEPTION !!!\n");
    serial::print_str("  vector: ");
    serial::print_hex(regs.int_no);
    serial::print_str(" (");
    serial::print_str(name);
    serial::print_str(")\n");
    serial::print_str("  error code: 0x");
    serial::print_hex(regs.err_code);
    serial::print_str("\n");

    // For #PF, dump CR2 (faulting address).
    if regs.int_no == 14 {
        let cr2 = read_cr2();
        serial::print_str("  CR2 (fault addr): 0x");
        serial::print_hex(cr2);
        serial::print_str("\n");
    }

    serial::print_str("  registers:\n");
    print_reg("RAX", regs.rax);
    print_reg("RBX", regs.rbx);
    print_reg("RCX", regs.rcx);
    print_reg("RDX", regs.rdx);
    print_reg("RSI", regs.rsi);
    print_reg("RDI", regs.rdi);
    print_reg("RBP", regs.rbp);
    print_reg("R8 ", regs.r8);
    print_reg("R9 ", regs.r9);
    print_reg("R10", regs.r10);
    print_reg("R11", regs.r11);
    print_reg("R12", regs.r12);
    print_reg("R13", regs.r13);
    print_reg("R14", regs.r14);
    print_reg("R15", regs.r15);

    // RIP + RFLAGS from the saved stack frame.
    let rip = unsafe { core::ptr::read_volatile((regs as *const Registers as *const u8).add(15*8 + 16) as *const u64) };
    serial::print_str("  RIP: 0x");
    serial::print_hex(rip);
    serial::print_str("\n");

    // Mirror to VGA.
    vga::print_str("\n!!! EXCEPTION ");
    vga::print_str(name);
    vga::print_str(" !!!\n");

    // Halt forever (no task scheduler yet).
    loop {
        unsafe { core::arch::asm!("cli; hlt", options(nostack, nomem, preserves_flags)); }
    }
}

fn print_reg(name: &str, val: u64) {
    serial::print_str("    ");
    serial::print_str(name);
    serial::print_str(" = 0x");
    serial::print_hex(val);
    serial::print_str("\n");
}

#[inline]
fn read_cr2() -> u64 {
    let v: u64;
    unsafe { core::arch::asm!("mov {}, cr2", out(reg) v, options(nostack, nomem, preserves_flags)); }
    v
}
