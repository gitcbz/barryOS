//! PS/2 keyboard driver.
//!
//! The IRQ1 handler in `interrupts/irq.rs` reads the scancode and calls
//! `handle_scancode`.  This module turns scancodes into characters and queues
//! them; the kernel's input loop drains the queue and feeds the terminal.
//!
//! The previous revision decoded to lowercase only (no Shift, no Caps Lock),
//! wrote the result to the serial port, and threw it away — nothing in the
//! kernel ever consumed a keystroke, so the desktop could not be typed into.

use crate::serial;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Scancode → ASCII, unshifted (Set 1).  0 = no printable character.
/// Index 1 (Esc) is special-cased in `handle_scancode` rather than mapped here,
/// so that it produces a real sentinel instead of an unmapped zero.
const MAP_NORMAL: [u8; 59] = [
    0,    0,    b'1', b'2', b'3', b'4', b'5', b'6', b'7', b'8', b'9',
    b'0', b'-', b'=', 0,   0,    b'q', b'w', b'e', b'r', b't', b'y',
    b'u', b'i', b'o', b'p', b'[', b']', 0,   0,    b'a', b's', b'd',
    b'f', b'g', b'h', b'j', b'k', b'l', b';', b'\'', b'`', 0,   b'\\',
    b'z', b'x', b'c', b'v', b'b', b'n', b'm', b',', b'.', b'/', 0,
    b'*', 0,   b' ', 0,
];

/// Scancode → ASCII with Shift held.  Letters uppercase, digits → symbols.
/// Grouped 11 per row to line up with `MAP_NORMAL` (indices 0-10, 11-21, ...).
const MAP_SHIFT: [u8; 59] = [
    0,   0,   b'!', b'@', b'#', b'$', b'%', b'^', b'&', b'*', b'(',
    b')', b'_', b'+', 0,   0,   b'Q', b'W', b'E', b'R', b'T', b'Y',
    b'U', b'I', b'O', b'P', b'{', b'}', 0,   0,   b'A', b'S', b'D',
    b'F', b'G', b'H', b'J', b'K', b'L', b':', b'"', b'~', 0,   b'|',
    b'Z', b'X', b'C', b'V', b'B', b'N', b'M', b'<', b'>', b'?', 0,
    b'*', 0,   b' ', 0,
];

const SCAN_ENTER:     u8 = 0x1C;
const SCAN_BACKSPACE: u8 = 0x0E;
const SCAN_LSHIFT:    u8 = 0x2A;
const SCAN_RSHIFT:    u8 = 0x36;
const SCAN_CAPSLOCK:  u8 = 0x3A;
const SCAN_EXTENDED:  u8 = 0xE0;

/// Sentinels handed to the consumer for keys that are not characters.
pub const KEY_ENTER:     u8 = b'\n';
pub const KEY_BACKSPACE: u8 = 0x08;
pub const KEY_ESC:       u8 = 0x1B;
pub const KEY_TAB:       u8 = b'\t';
/// Arrow keys and Delete, from the 0xE0-prefixed extended set.  These are
/// control codes that cannot collide with typed ASCII.
pub const KEY_UP:        u8 = 0x11;
pub const KEY_DOWN:      u8 = 0x12;
pub const KEY_LEFT:      u8 = 0x13;
pub const KEY_RIGHT:     u8 = 0x14;
pub const KEY_DELETE:    u8 = 0x7F;

/// Modifier state.
static SHIFT_DOWN: AtomicBool = AtomicBool::new(false);
static CAPS_ON:    AtomicBool = AtomicBool::new(false);
/// True while the byte after an 0xE0 prefix is being consumed.
static EXT_PENDING: AtomicBool = AtomicBool::new(false);

/// Total keys pressed (diagnostics).
pub static KEYS_TOTAL: AtomicUsize = AtomicUsize::new(0);

/// Decoded characters waiting for the input loop.  Single producer (IRQ1),
/// single consumer (the input loop), so head/tail atomics are enough.
const QUEUE_SIZE: usize = 64;
static mut QUEUE: [u8; QUEUE_SIZE] = [0; QUEUE_SIZE];
static Q_HEAD: AtomicUsize = AtomicUsize::new(0);
static Q_TAIL: AtomicUsize = AtomicUsize::new(0);

/// Take the next decoded key, or None if none is waiting.
pub fn poll() -> Option<u8> {
    let head = Q_HEAD.load(Ordering::Acquire);
    if head == Q_TAIL.load(Ordering::Acquire) {
        return None;
    }
    let ch = unsafe {
        let p = core::ptr::addr_of!(QUEUE) as *const u8;
        core::ptr::read_volatile(p.add(head))
    };
    Q_HEAD.store((head + 1) % QUEUE_SIZE, Ordering::Release);
    Some(ch)
}

/// Push a decoded key for the input loop.
fn push(ch: u8) {
    let tail = Q_TAIL.load(Ordering::Acquire);
    let next = (tail + 1) % QUEUE_SIZE;
    if next == Q_HEAD.load(Ordering::Acquire) {
        return;                             // queue full: drop rather than block
    }
    unsafe {
        let p = core::ptr::addr_of_mut!(QUEUE) as *mut u8;
        core::ptr::write_volatile(p.add(tail), ch);
    }
    Q_TAIL.store(next, Ordering::Release);
}

/// Initialize the keyboard driver.
pub fn init() {
    Q_HEAD.store(0, Ordering::SeqCst);
    Q_TAIL.store(0, Ordering::SeqCst);
    KEYS_TOTAL.store(0, Ordering::SeqCst);
    SHIFT_DOWN.store(false, Ordering::SeqCst);
    CAPS_ON.store(false, Ordering::SeqCst);
    serial::print_str("[kb] PS/2 keyboard driver initialized (shift+caps, 64-key queue)\n");
}

/// Process a scancode.  Called from the IRQ1 handler.
pub fn handle_scancode(scancode: u8) {
    KEYS_TOTAL.fetch_add(1, Ordering::Relaxed);

    // Extended keys (arrows, right ctrl/alt, ...) arrive as 0xE0 followed by
    // the actual code.  We have no use for any of them yet; swallow the pair
    // so the second byte is not misread as a normal key.
    if scancode == SCAN_EXTENDED {
        EXT_PENDING.store(true, Ordering::Relaxed);
        return;
    }

    let released = scancode & 0x80 != 0;
    let code = scancode & 0x7F;

    // Extended keys (arrows, right ctrl/alt, Delete, ...) arrive as 0xE0
    // followed by the actual code.  We only care about the navigation block;
    // everything else is swallowed so the second byte is not misread as a
    // normal key.
    if EXT_PENDING.swap(false, Ordering::Relaxed) {
        if !released {
            let ext = match code {
                0x48 => Some(KEY_UP),
                0x50 => Some(KEY_DOWN),
                0x4B => Some(KEY_LEFT),
                0x4D => Some(KEY_RIGHT),
                0x53 => Some(KEY_DELETE),
                _ => None,
            };
            if let Some(k) = ext {
                push(k);
            }
        }
        return;
    }

    // Modifiers first: they are state, not characters.
    match code {
        SCAN_LSHIFT | SCAN_RSHIFT => {
            SHIFT_DOWN.store(!released, Ordering::Relaxed);
            return;
        }
        SCAN_CAPSLOCK => {
            if !released {
                CAPS_ON.store(!CAPS_ON.load(Ordering::Relaxed), Ordering::Relaxed);
            }
            return;
        }
        _ => {}
    }
    if released {
        return;
    }

    match code {
        SCAN_ENTER => push(KEY_ENTER),
        SCAN_BACKSPACE => push(KEY_BACKSPACE),
        0x01 => push(KEY_ESC),      // Esc: cancel whatever is being typed
        0x0F => push(KEY_TAB),      // Tab
        _ => {
            let idx = code as usize;
            if idx < MAP_NORMAL.len() {
                let shift = SHIFT_DOWN.load(Ordering::Relaxed);
                let caps = CAPS_ON.load(Ordering::Relaxed);
                let ch = if shift {
                    MAP_SHIFT[idx]
                } else {
                    // Caps Lock only affects letters.
                    let c = MAP_NORMAL[idx];
                    if caps && c.is_ascii_lowercase() { c - 32 } else { c }
                };
                if ch != 0 {
                    push(ch);
                }
            }
        }
    }
}
