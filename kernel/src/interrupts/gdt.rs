//! GDT + TSS for the kernel.
//!
//! Stage 3 uses a minimal GDT: null + kernel code + kernel data + TSS.
//! The TSS provides an interrupt stack (IST1) for #DF (double fault) so
//! a stack overflow doesn't cause a triple fault.

use core::sync::atomic::AtomicU64;
use crate::serial;

/// GDT entry count: null + code + data + TSS_low + TSS_high = 5 entries.
const GDT_ENTRIES: usize = 5;

/// TSS interrupt stack top (16 KiB, grows down).
const IST_STACK_TOP: usize = 0x0030_0000;

#[repr(C, align(8))]
pub struct TaskStateSegment {
    reserved0: u32,
    /// The full 64-bit stack pointers for the 7 IST entries.
    pub ist: [u64; 7],
    reserved1: u32,
    /// The I/O map base address (we don't use it).
    iomap_base: u32,
}

impl TaskStateSegment {
    pub const fn new() -> Self {
        Self {
            reserved0: 0,
            ist: [0; 7],
            reserved1: 0,
            iomap_base: 0,
        }
    }
}

/// 8-byte GDT entry.
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct GdtEntry {
    limit_low:   u16,
    base_low:    u16,
    base_middle: u8,
    access:      u8,
    granularity: u8,
    base_high:   u8,
}

impl GdtEntry {
    pub const fn zero() -> Self {
        Self { limit_low: 0, base_low: 0, base_middle: 0, access: 0, granularity: 0, base_high: 0 }
    }
}

/// 16-byte GDT entry for TSS (system segment, 64-bit).
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct GdtEntryTss {
    limit_low:   u16,
    base_low:    u16,
    base_middle: u8,
    access:      u8,
    granularity: u8,
    base_high:   u8,
    base_upper:  u32,
    reserved:    u32,
}

/// GDT selector offsets.
pub const KERNEL_CODE: u16 = 0x08;
pub const KERNEL_DATA: u16 = 0x10;
pub const TSS_SEL:     u16 = 0x18;

/// Static GDT + TSS (aligned, accessed via raw pointers).
#[repr(C, align(16))]
struct GdtTable {
    entries: [GdtEntry; 3],   // null, code, data
    tss:     GdtEntryTss,     // TSS system descriptor
}

static GDT: GdtTable = GdtTable {
    entries: [
        GdtEntry::zero(),   // null
        // Code: base=0, limit=0xFFFFF, 64-bit, present, DPL=0, S=1, type=exec/read
        GdtEntry {
            limit_low: 0xFFFF,
            base_low: 0,
            base_middle: 0,
            access: 0x9A,       // present | DPL=0 | S=1 (code) | type=exec/read
            granularity: 0xAF,  // G=1, D/B=1, L=1 (64-bit), high limit nibble
            base_high: 0,
        },
        // Data: base=0, limit=0xFFFFF, present, DPL=0, S=1, type=read/write
        GdtEntry {
            limit_low: 0xFFFF,
            base_low: 0,
            base_middle: 0,
            access: 0x92,       // present | DPL=0 | S=1 (data) | type=read/write
            granularity: 0xCF,  // G=1, D/B=1, high limit nibble
            base_high: 0,
        },
    ],
    tss: GdtEntryTss {
        limit_low: 0,
        base_low: 0,
        base_middle: 0,
        access: 0,
        granularity: 0,
        base_high: 0,
        base_upper: 0,
        reserved: 0,
    },
};

static TSS: TaskStateSegment = TaskStateSegment::new();

static GDT_ADDR: AtomicU64 = AtomicU64::new(0);

/// GDTR format for `lgdt`.
#[repr(C, packed)]
struct GdtDescriptor {
    limit: u16,
    base: u64,
}

pub fn init() {
    unsafe {
        // Set up TSS with IST1 = IST_STACK_TOP (for #DF).
        let tss_ptr = core::ptr::addr_of!(TSS) as *mut TaskStateSegment;
        (*tss_ptr).ist[0] = IST_STACK_TOP as u64;

        // Patch the TSS GDT entry with the TSS address + limit.
        let tss_addr = tss_ptr as u64;
        let tss_limit = (core::mem::size_of::<TaskStateSegment>() - 1) as u16;

        let tss_entry = core::ptr::addr_of!(GDT.tss) as *mut GdtEntryTss;
        (*tss_entry).limit_low   = tss_limit;
        (*tss_entry).base_low    = (tss_addr & 0xFFFF) as u16;
        (*tss_entry).base_middle = ((tss_addr >> 16) & 0xFF) as u8;
        (*tss_entry).access      = 0x89; // present | type=0x9 (64-bit TSS available)
        (*tss_entry).granularity = ((tss_limit >> 8) & 0x0F) as u8;
        (*tss_entry).base_high   = ((tss_addr >> 24) & 0xFF) as u8;
        (*tss_entry).base_upper  = ((tss_addr >> 32) & 0xFFFFFFFF) as u32;
        (*tss_entry).reserved   = 0;

        let gdt_addr = core::ptr::addr_of!(GDT) as u64;
        GDT_ADDR.store(gdt_addr, core::sync::atomic::Ordering::Release);

        let desc = GdtDescriptor {
            limit: (core::mem::size_of_val(&GDT) - 1) as u16,
            base: gdt_addr,
        };

        serial::print_str("[irq] about to lgdt\n");
        // Load GDT.
        core::arch::asm!(
            "lgdt [{}]",
            in(reg) &desc as *const GdtDescriptor,
            options(nostack, preserves_flags),
        );
        serial::print_str("[irq] lgdt done\n");

        // Load segment registers (data segments).
        core::arch::asm!(
            "mov ax, {sel}",
            "mov ds, ax",
            "mov es, ax",
            "mov fs, ax",
            "mov gs, ax",
            "mov ss, ax",
            sel = const KERNEL_DATA,
            out("ax") _,
            options(nostack, preserves_flags),
        );
        serial::print_str("[irq] seg regs loaded\n");

        // Load TSS.
        core::arch::asm!(
            "ltr ax",
            in("ax") TSS_SEL,
            options(nostack, preserves_flags),
        );
        serial::print_str("[irq] ltr done\n");
    }

    serial::print_str("[irq] GDT loaded @ 0x");
    serial::print_hex(GDT_ADDR.load(core::sync::atomic::Ordering::Acquire));
    serial::print_str(", TSS IST1=0x");
    serial::print_hex(IST_STACK_TOP as u64);
    serial::print_str("\n");
}
