//! PS/2 keyboard driver.
//!
//! The IRQ1 handler in `interrupts::irq.rs` already reads scancodes.
//! This module adds a line buffer + echo support: typed characters
//! accumulate in a 256-byte buffer, and when Enter is pressed, the
//! line is available for processing.

use crate::serial;
use core::sync::atomic::{AtomicUsize, Ordering};

/// Keyboard line buffer.
static mut LINE_BUF: [u8; 256] = [0; 256];
static LINE_LEN: AtomicUsize = AtomicUsize::new(0);

/// Total keys pressed (for diagnostics).
pub static KEYS_TOTAL: AtomicUsize = AtomicUsize::new(0);

/// Scancode → ASCII (Set 1, US layout).  0 = non-printable / undefined.
const SCANCODE_MAP: [u8; 59] = [
    0, 0, b'1', b'2', b'3', b'4', b'5', b'6', b'7', b'8', b'9', b'0',
    b'-', b'=', 0, 0, b'q', b'w', b'e', b'r', b't', b'y', b'u', b'i',
    b'o', b'p', b'[', b']', 0, 0, b'a', b's', b'd', b'f', b'g', b'h',
    b'j', b'k', b'l', b';', b'\'', b'`', 0, 0x5c, b'z', b'x', b'c',
    b'v', b'b', b'n', b'm', b',', b'.', b'/', 0, b'*', 0, b' ', 0,
];

/// Enter scancode (Set 1).
const SCAN_ENTER: u8 = 0x1C;
/// Backspace scancode.
const SCAN_BACKSPACE: u8 = 0x0E;

/// Initialize the keyboard driver.
pub fn init() {
    LINE_LEN.store(0, Ordering::SeqCst);
    KEYS_TOTAL.store(0, Ordering::SeqCst);
    serial::print_str("[kb] PS/2 keyboard driver initialized (256-byte line buffer)\n");
}

/// Process a scancode.  Called from the IRQ1 handler in `interrupts/irq.rs`.
pub fn handle_scancode(scancode: u8) {
    KEYS_TOTAL.fetch_add(1, Ordering::Relaxed);

    // Key release = bit 7 set; ignore.
    if scancode & 0x80 != 0 {
        return;
    }

    if scancode == SCAN_ENTER {
        // End of line — echo newline + null-terminate.
        serial::print_str("\n");
        let len = LINE_LEN.load(Ordering::Relaxed);
        if len > 0 {
            serial::print_str("[kb] line: \"");
            let buf_ptr = unsafe { core::ptr::addr_of_mut!(LINE_BUF) as *const u8 };
            for i in 0..len {
                let ch = unsafe { core::ptr::read_volatile(buf_ptr.add(i)) };
                serial::write_char(ch);
            }
            serial::print_str("\"\n");
            LINE_LEN.store(0, Ordering::Relaxed);
        }
        return;
    }

    if scancode == SCAN_BACKSPACE {
        let len = LINE_LEN.load(Ordering::Relaxed);
        if len > 0 {
            LINE_LEN.store(len - 1, Ordering::Relaxed);
            serial::print_str("\x08 \x08");  // backspace + space + backspace
        }
        return;
    }

    // Printable key?
    if (scancode as usize) < SCANCODE_MAP.len() {
        let ch = SCANCODE_MAP[scancode as usize];
        if ch != 0 {
            let len = LINE_LEN.load(Ordering::Relaxed);
            if len < 255 {
                unsafe {
                    let p = core::ptr::addr_of_mut!(LINE_BUF) as *mut u8;
                    core::ptr::write_volatile(p.add(len), ch);
                }
                LINE_LEN.store(len + 1, Ordering::Relaxed);
                serial::write_char(ch);  // echo
            }
        }
    }
}
