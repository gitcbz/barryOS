//! 8259 PIC (Programmable Interrupt Controller) driver.
//!
//! Remaps IRQ 0..15 to INT vectors 32..47 so they don't collide with CPU
//! exceptions.  Masks all IRQs except timer (IRQ0) and keyboard (IRQ1).
//!
//! We use the legacy PIC (not APIC) for Stage 3 — it's simpler and works
//! on both QEMU and VMware.

use crate::serial;

const PIC1: u16 = 0x20;  // master PIC
const PIC2: u16 = 0xA0;  // slave PIC
const PIC1_CMD:  u16 = PIC1;
const PIC1_DATA: u16 = PIC1 + 1;
const PIC2_CMD:  u16 = PIC2;
const PIC2_DATA: u16 = PIC2 + 1;

const ICW1_ICW4:   u8 = 0x01;  // ICW4 needed
const ICW1_INIT:   u8 = 0x10;  // init
const ICW4_8086:   u8 = 0x01;  // 8086 mode

/// IRQ vector offsets.
const IRQ_BASE: u8 = 0x20;  // IRQ0 → INT 32

/// End-of-interrupt for master (IRQ 0-7).
#[inline]
pub fn eoi_master() {
    unsafe {
        core::arch::asm!(
            "out dx, al",
            in("dx") PIC1_CMD,
            in("al") 0x20u8,
            options(nostack, nomem, preserves_flags),
        );
    }
}

/// End-of-interrupt for slave (IRQ 8-15).
#[inline]
pub fn eoi_slave() {
    unsafe {
        core::arch::asm!(
            "out dx, al",
            in("dx") PIC2_CMD,
            in("al") 0x20u8,
            options(nostack, nomem, preserves_flags),
        );
        eoi_master();  // also EOI master for cascade
    }
}

/// Send EOI for the given IRQ vector.
pub fn eoi(vector: u64) {
    if vector >= 40 {
        eoi_slave();
    } else if vector >= 32 {
        eoi_master();
    }
    // <32 = CPU exception, no EOI needed.
}

/// Initialize + remap the PIC.
pub fn init() {
    unsafe {
        // Disable interrupts during PIC reconfig.
        core::arch::asm!("cli", options(nostack, preserves_flags));

        // Mask all interrupts first.
        port_out(PIC1_DATA, 0xFF);
        port_out(PIC2_DATA, 0xFF);
        io_wait();

        // Start init in cascade mode (ICW1).
        port_out(PIC1_CMD, ICW1_INIT | ICW1_ICW4);
        io_wait();
        port_out(PIC2_CMD, ICW1_INIT | ICW1_ICW4);
        io_wait();

        // ICW2: vector offsets.
        port_out(PIC1_DATA, IRQ_BASE);
        io_wait();
        port_out(PIC2_DATA, IRQ_BASE + 8);
        io_wait();

        // ICW3: cascade wiring.
        port_out(PIC1_DATA, 4);
        io_wait();
        port_out(PIC2_DATA, 2);
        io_wait();

        // ICW4: 8086 mode.
        port_out(PIC1_DATA, ICW4_8086);
        io_wait();
        port_out(PIC2_DATA, ICW4_8086);
        io_wait();

        // Verify: read back the mask register.
        let v1 = port_in(PIC1_DATA);
        let v2 = port_in(PIC2_DATA);
        serial::print_str("[irq] PIC masks after init: m1=0x");
        serial::print_hex(v1 as u64);
        serial::print_str(" m2=0x");
        serial::print_hex(v2 as u64);
        serial::print_str("\n");

        // Unmask IRQ0 (timer), IRQ1 (keyboard) and IRQ2 (cascade to the slave).
        let mask: u8 = !(1 | 2 | 4);
        port_out(PIC1_DATA, mask);
        // Slave: IRQ12 is the PS/2 mouse, and bit 2 is the cascade line the
        // master uses to reach the slave at all.  Everything else stays masked.
        let slave_mask: u8 = 0xFB & !(1 << 4);
        port_out(PIC2_DATA, slave_mask);
    }

    serial::print_str("[irq] PIC remapped: IRQ0..15 → INT 32..47 (timer, kbd, mouse)\n");
}

/// Small I/O delay — a jump-to-self is the classic 8259 PIC delay.
#[inline]
unsafe fn io_wait() {
    core::arch::asm!(
        "out 0x80, al",
        in("al") 0u8,
        options(nostack, nomem, preserves_flags),
    );
}

#[inline]
unsafe fn port_out(port: u16, val: u8) {
    core::arch::asm!(
        "out dx, al",
        in("dx") port,
        in("al") val,
        options(nostack, preserves_flags),
    );
}

#[inline]
unsafe fn port_in(port: u16) -> u8 {
    let val: u8;
    core::arch::asm!(
        "in al, dx",
        out("al") val,
        in("dx") port,
        options(nostack, preserves_flags),
    );
    val
}
