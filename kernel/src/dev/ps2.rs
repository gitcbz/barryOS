//! The 8042 keyboard controller.
//!
//! Both PS/2 devices hang off one chip, and everything interesting about it is
//! shared: the two ports, the handshake that says a byte may be sent, and one
//! configuration byte whose bits decide whether either device is heard from at
//! all.  It lives in its own module because it belongs to neither driver, and
//! because the byte that matters has to be read and written by exactly one
//! piece of code — read twice by two drivers is how a keyboard ends up
//! disabled by the mouse's initialisation.

use core::arch::asm;

pub const DATA: u16 = 0x60;
pub const STATUS: u16 = 0x64;
pub const CMD: u16 = 0x64;

/// Status register: a byte is waiting to be read.
pub const ST_OUTPUT_FULL: u8 = 0x01;
/// Status register: the controller is not ready to take a byte.
pub const ST_INPUT_FULL: u8 = 0x02;
/// Status register: the waiting byte came from the auxiliary (mouse) port.
pub const ST_FROM_AUX: u8 = 0x20;

#[inline]
pub unsafe fn status() -> u8 {
    port_in(STATUS)
}

#[inline]
pub unsafe fn read_data() -> u8 {
    port_in(DATA)
}

/// Send a byte to the controller itself — a command, not device data.
pub unsafe fn ctrl_write(v: u8) {
    wait_input_clear();
    port_out(CMD, v);
}

/// Send a byte on the data port, which after a `0xD4` command goes to the
/// device rather than to the controller.
pub unsafe fn data_write(v: u8) {
    wait_input_clear();
    port_out(DATA, v);
}

/// The next byte the controller produces.
pub unsafe fn data_read() -> u8 {
    wait_output_full();
    port_in(DATA)
}

/// Discard anything already in the output buffer.
///
/// A byte in there is the answer to an *earlier* question — a scancode, an
/// acknowledgement, the result of the controller's own self test — and a
/// driver that asks for something and reads it without draining this first
/// gets that earlier answer instead.  There is nowhere else for it to come
/// from, and nothing at this point is listening for it, so throwing it away
/// loses nothing.
pub unsafe fn drain_output() {
    // Bounded, and short: the controller has at most a couple of bytes.  The
    // pause between reads is not politeness — a byte put on the bus by the
    // device takes a moment to appear in the status register, and a loop that
    // reads faster than that finds one byte and stops.
    for _ in 0..16 {
        if status() & ST_OUTPUT_FULL == 0 {
            return;
        }
        let _ = read_data();
        io_delay();
    }
}

/// Bounded waits: a dead or absent controller must not hang the boot.
unsafe fn wait_input_clear() {
    for _ in 0..100_000 {
        if status() & ST_INPUT_FULL == 0 {
            return;
        }
    }
}

unsafe fn wait_output_full() {
    for _ in 0..100_000 {
        if status() & ST_OUTPUT_FULL != 0 {
            return;
        }
    }
}

/// A port write is not instantaneous as far as the next port access is
/// concerned.
#[inline]
pub unsafe fn io_delay() {
    port_out(0x80, 0);
}

#[inline]
pub unsafe fn port_in(port: u16) -> u8 {
    let val: u8;
    asm!(
        "in al, dx",
        out("al") val,
        in("dx") port,
        options(nostack, nomem, preserves_flags),
    );
    val
}

#[inline]
pub unsafe fn port_out(port: u16, val: u8) {
    asm!(
        "out dx, al",
        in("dx") port,
        in("al") val,
        options(nostack, nomem, preserves_flags),
    );
}
