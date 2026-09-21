//! IDT (Interrupt Descriptor Table) for x86_64.
//!
//! 256 entries, each a 16-byte interrupt gate descriptor.  We load it with
//! `lidt` and point each vector at a raw assembly stub in `handlers.rs`
//! that saves registers and calls a Rust handler.
//!
//! NOTE: Rust nightly 1.100 has a codegen bug where `naked_fn as u64`
//! returns 0 for functions marked `#[naked]`.  We work around by using
//! a macro that emits `lea` via inline asm at each call site, capturing
//! the function symbol directly.

use core::ptr::addr_of_mut;
use crate::serial;

/// 16-byte interrupt-gate descriptor (x86_64).
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct IdtEntry {
    pub offset_low:    u16,   // bits 0..15 of handler address
    pub selector:      u16,   // code segment selector (0x08 = kernel code)
    pub ist:           u8,    // IST index (0 = no IST, 1 = IST1 for #DF)
    pub type_attr:     u8,    // present | DPL=0 | type=0xE (interrupt gate)
    pub offset_middle: u16,   // bits 16..31
    pub offset_high:   u32,   // bits 32..63
    pub reserved:      u32,
}

impl IdtEntry {
    pub const fn zero() -> Self {
        Self {
            offset_low: 0,
            selector: 0,
            ist: 0,
            type_attr: 0,
            offset_middle: 0,
            offset_high: 0,
            reserved: 0,
        }
    }
}

/// The IDT: 256 16-byte entries.
#[repr(C, align(16))]
pub struct Idt {
    pub entries: [IdtEntry; 256],
}

impl Idt {
    pub const fn new() -> Self {
        Self { entries: [IdtEntry::zero(); 256] }
    }
}

/// Static IDT (accessed via raw pointer — Rust 2024 safe pattern).
static mut IDT: Idt = Idt::new();

/// IDTR for `lidt`.
#[repr(C, packed)]
struct IdtDescriptor {
    limit: u16,
    base: u64,
}

/// Get the address of a naked function symbol via inline asm `lea`.
/// This works around the `naked_fn as u64` → 0 codegen bug.
macro_rules! fn_addr {
    ($f:path) => {{
        let addr: u64;
        unsafe {
            core::arch::asm!(
                "lea {0}, [rip + {1}]",
                out(reg) addr,
                sym $f,
                options(nostack, preserves_flags),
            );
        }
        addr
    }};
}

/// Set IDT entry `i` to point at handler `$f` with the given IST.
macro_rules! set_entry {
    ($idt:expr, $i:expr, $f:path, $ist:expr) => {
        {
            let addr = fn_addr!($f);
            let e = &mut $idt.entries[$i];
            e.offset_low    = (addr & 0xFFFF) as u16;
            e.selector      = super::gdt::KERNEL_CODE;
            e.ist           = $ist;
            e.type_attr     = 0x8E;
            e.offset_middle = ((addr >> 16) & 0xFFFF) as u16;
            e.offset_high   = ((addr >> 32) & 0xFFFFFFFF) as u32;
            e.reserved      = 0;
        }
    };
}

/// Initialize the IDT with all handlers, then load it.
pub fn init() {
    let idt = unsafe { &mut *addr_of_mut!(IDT) };

    // CPU exceptions (vectors 0..31).
    set_entry!(idt, 0,  super::handlers::de_handler,  0);
    set_entry!(idt, 1,  super::handlers::db_handler,  0);
    set_entry!(idt, 2,  super::handlers::nmi_handler,  0);
    set_entry!(idt, 3,  super::handlers::bp_handler,   0);
    set_entry!(idt, 4,  super::handlers::of_handler,   0);
    set_entry!(idt, 5,  super::handlers::br_handler,   0);
    set_entry!(idt, 6,  super::handlers::ud_handler,  0);
    set_entry!(idt, 7,  super::handlers::nm_handler,   0);
    set_entry!(idt, 8,  super::handlers::df_handler,  1); // #DF IST1
    set_entry!(idt, 9,  super::handlers::xo_handler,   0);
    set_entry!(idt, 10, super::handlers::ts_handler,  0);
    set_entry!(idt, 11, super::handlers::np_handler,  0);
    set_entry!(idt, 12, super::handlers::ss_handler,  0);
    set_entry!(idt, 13, super::handlers::gp_handler,  0);
    set_entry!(idt, 14, super::handlers::pf_handler,  0);
    set_entry!(idt, 15, super::handlers::xf_handler,  0);
    set_entry!(idt, 16, super::handlers::mf_handler,  0);
    set_entry!(idt, 17, super::handlers::ac_handler,  0);
    set_entry!(idt, 18, super::handlers::mc_handler,  0);
    set_entry!(idt, 19, super::handlers::xm_handler,  0);
    set_entry!(idt, 20, super::handlers::ve_handler,  0);
    for i in 21..32 {
        set_entry!(idt, i, super::handlers::reserved_handler, 0);
    }

    // IRQ vectors (32..47).
    set_entry!(idt, 32, super::handlers::irq0_timer,    0);
    set_entry!(idt, 33, super::handlers::irq1_keyboard, 0);
    set_entry!(idt, 34, super::handlers::irq2_cascade,  0);
    set_entry!(idt, 35, super::handlers::irq3_com2,     0);
    set_entry!(idt, 36, super::handlers::irq4_com1,     0);
    set_entry!(idt, 37, super::handlers::irq5_lpt2,     0);
    set_entry!(idt, 38, super::handlers::irq6_floppy,   0);
    set_entry!(idt, 39, super::handlers::irq7_lpt1,     0);
    set_entry!(idt, 40, super::handlers::irq8_rtc,      0);
    set_entry!(idt, 41, super::handlers::irq9_acpi,     0);
    set_entry!(idt, 42, super::handlers::irq10_pci,     0);
    set_entry!(idt, 43, super::handlers::irq11_pci,     0);
    set_entry!(idt, 44, super::handlers::irq12_mouse,    0);
    set_entry!(idt, 45, super::handlers::irq13_fpu,     0);
    set_entry!(idt, 46, super::handlers::irq14_ata,     0);
    set_entry!(idt, 47, super::handlers::irq15_ata,     0);

    for i in 48..256 {
        set_entry!(idt, i, super::handlers::spurious_handler, 0);
    }

    // Load IDT.
    let desc = IdtDescriptor {
        limit: (core::mem::size_of::<Idt>() - 1) as u16,
        base: addr_of_mut!(IDT) as u64,
    };
    unsafe {
        core::arch::asm!(
            "lidt [{}]",
            in(reg) &desc,
            options(nostack, preserves_flags),
        );
    }

    // Debug: dump IRQ0 (vector 32) IDT entry to verify the handler address.
    let e = &idt.entries[32];
    let off = (e.offset_low as u64)
        | ((e.offset_middle as u64) << 16)
        | ((e.offset_high as u64) << 32);
    serial::print_str("[irq] IDT[32]: off=0x");
    serial::print_hex(off);
    serial::print_str(" sel=0x");
    serial::print_hex(e.selector as u64);
    serial::print_str(" ist=");
    serial::print_hex(e.ist as u64);
    serial::print_str(" attr=0x");
    serial::print_hex(e.type_attr as u64);
    serial::print_str("\n");

    serial::print_str("[irq] IDT loaded (256 entries @ 0x");
    serial::print_hex(addr_of_mut!(IDT) as u64);
    serial::print_str(")\n");
}
