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
///   1. GDT + TSS (the selectors the IDT gates below refer to).
///   2. IDT with 256 entries (exceptions + IRQs).
///   3. PIC 8259 remap + mask (timer + keyboard enabled).
///   4. Enable interrupts (`sti`).
pub fn init() {
    // The GDT has to be ours *before* the first interrupt can be delivered.
    // Every IDT gate below names selector 0x08 as its target code segment, and
    // 0x08 only means "64-bit kernel code" in the GDT built here.  The BIOS
    // path got away without this because stage2.asm installs an identical
    // layout; under UEFI the firmware's GDT is still live, 0x08 is whatever
    // the firmware put there, and the first timer IRQ took a #GP loading CS --
    // whose handler is delivered through the same broken selector, so #DF and
    // then a triple fault (VMware reports "virtual CPU entered shutdown").
    serial::print_str("[irq] step 1: build GDT + TSS\n");
    gdt::init();

    serial::print_str("[irq] step 2: build IDT (256 entries)\n");
    idt::init();

    serial::print_str("[irq] step 3: remap PIC 8259\n");
    pic::init();

    serial::print_str("[irq] step 4: enable interrupts (sti)\n");
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
