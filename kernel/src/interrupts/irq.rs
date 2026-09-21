//! IRQ handlers (timer, keyboard, spurious).
//!
//! Called by the naked assembly stubs in `handlers.rs` via the `irq_handler`
//! symbol.  Each IRQ vector 32..47 lands here; we dispatch on the vector
//! number.

use super::handlers::Registers;
use super::pic;
use crate::{serial, vga};
use core::sync::atomic::Ordering;

/// PIT (8253) I/O ports.
const PIT_CMD:  u16 = 0x43;
const PIT_CH0:  u16 = 0x40;
const PIT_FREQ: u32 = 1193182;  // PIT base frequency

/// PS/2 keyboard I/O ports.
const KB_DATA:  u16 = 0x60;
const KB_STATUS: u16 = 0x64;

/// Scancode → ASCII for the basic US layout (set 1).
const SCANCODE_MAP: [u8; 59] = [
    0, 0, b'1', b'2', b'3', b'4', b'5', b'6', b'7', b'8', b'9', b'0',
    b'-', b'=', 0, 0, b'q', b'w', b'e', b'r', b't', b'y', b'u', b'i',
    b'o', b'p', b'[', b']', 0, 0, b'a', b's', b'd', b'f', b'g', b'h',
    b'j', b'k', b'l', b';', b'\'', b'`', 0, 0x5c, b'z', b'x', b'c',
    b'v', b'b', b'n', b'm', b',', b'.', b'/', 0, b'*', 0, b' ', 0,
];

/// Rust entry point for all IRQs (called by `irq_handler` symbol).
#[no_mangle]
pub extern "C" fn irq_handler(regs: &Registers) {
    let vector = regs.int_no;

    match vector {
        32 => handle_timer(),
        33 => handle_keyboard(),
        // Cascade (IRQ2) — no action, just EOI slave.
        34 => { /* cascade, no-op */ }
        // Spurious IRQ 7 / 15 — no EOI.
        39 | 47 => { /* spurious */ }
        _ => {
            serial::print_str("[irq] unhandled IRQ ");
            serial::print_hex(vector);
            serial::print_str("\n");
        }
    }

    pic::eoi(vector);
}

/// PIT timer IRQ0 — count ticks for the dashboard.
fn handle_timer() {
    super::TIMER_TICKS.fetch_add(1, Ordering::Relaxed);
}

/// Keyboard IRQ1 — read scancode, pass to dev::keyboard for line buffering.
fn handle_keyboard() {
    let scancode = unsafe { port_in(KB_DATA) };
    super::KEYBOARD_IRQS.fetch_add(1, Ordering::Relaxed);
    crate::dev::keyboard::handle_scancode(scancode);
}

/// Configure the PIT to fire at ~100 Hz.
pub fn init_pit() {
    let divisor = PIT_FREQ / 100;  // 100 Hz
    let div_lo = (divisor & 0xFF) as u8;
    let div_hi = ((divisor >> 8) & 0xFF) as u8;
    unsafe {
        port_out(PIT_CMD, 0x36);  // channel 0, mode 3 (square wave), binary
        port_out(PIT_CH0, div_lo);
        port_out(PIT_CH0, div_hi);
    }
    serial::print_str("[irq] PIT configured: 100 Hz (divisor ");
    serial::print_hex(divisor as u64);
    serial::print_str(")\n");
}

#[inline]
unsafe fn port_in(port: u16) -> u8 {
    let val: u8;
    core::arch::asm!(
        "in al, dx",
        out("al") val,
        in("dx") port,
        options(nostack, nomem, preserves_flags),
    );
    val
}

#[inline]
unsafe fn port_out(port: u16, val: u8) {
    core::arch::asm!(
        "out dx, al",
        in("dx") port,
        in("al") val,
        options(nostack, nomem, preserves_flags),
    );
}
