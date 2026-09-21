//! barryOS kernel — Stage 3 interrupt subsystem.
//!
//! Modules:
//! - `idt`:        IDT structure + GateDescriptor + loading via `lidt`.
//! - `gdt`:        GDT + TSS for interrupt stacks (kernel stack only for now).
//! - `exceptions`: CPU exception handlers (#DE, #UD, #GP, #PF, #DF, ...).
//! - `pic`:        8259 PIC remap (IRQ 0-15 → INT 32-47) + mask.
//! - `irq`:        IRQ handlers: timer (IRQ0), keyboard (IRQ1), spurious.
//! - `handlers`:   raw entry stubs that save regs, call Rust, restore regs.

pub mod idt;
pub mod gdt;
pub mod exceptions;
pub mod pic;
pub mod irq;
pub mod handlers;

use core::sync::atomic::{AtomicU64, AtomicBool, Ordering};
use crate::serial;

pub static INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Global interrupt counters (for dashboard visibility).
pub static TIMER_TICKS:   AtomicU64 = AtomicU64::new(0);
pub static KEYBOARD_IRQS: AtomicU64 = AtomicU64::new(0);
pub static EXCEPTIONS:    AtomicU64 = AtomicU64::new(0);

/// Initialize the interrupt subsystem:
///   1. IDT with 256 entries (exceptions + IRQs).
///   2. GDT (for future TSS/IST; TSS load deferred to Stage 3b).
///   3. PIC 8259 remap + mask (timer + keyboard enabled).
///   4. Enable interrupts (`sti`).
pub fn init() {
    serial::print_str("[irq] step 1: build IDT (256 entries)\n");
    idt::init();

    serial::print_str("[irq] step 2: remap PIC 8259\n");
    pic::init();

    serial::print_str("[irq] step 3: enable interrupts (sti)\n");
    // Enable interrupts.
    unsafe { core::arch::asm!("sti", options(nostack, nomem, preserves_flags)); }

    INITIALIZED.store(true, Ordering::Release);
    serial::print_str("[irq] interrupt subsystem online\n");
    serial::print_str("[irq] interrupts enabled; waiting for timer...\n");
    let start = TIMER_TICKS.load(Ordering::Relaxed);
    // Short spin (5M nops → completes fast even on slow UEFI path).
    let mut spin = 0;
    while spin < 5_000_000 {
        unsafe { core::arch::asm!("nop", options(nostack, nomem, preserves_flags)); }
        spin += 1;
    }
    let end = TIMER_TICKS.load(Ordering::Relaxed);
    serial::print_str("[irq] timer ticks: ");
    serial::print_hex(end - start);
    serial::print_str(" (decimal, expect >0)\n");
    serial::print_str("[irq] keyboard IRQs: ");
    serial::print_hex(KEYBOARD_IRQS.load(Ordering::Relaxed));
    serial::print_str("\n");
}
