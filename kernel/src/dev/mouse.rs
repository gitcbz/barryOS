//! PS/2 mouse driver (IRQ12) and the pointer it drives on the framebuffer.
//!
//! The device is the standard auxiliary PS/2 port: enable it through the
//! 8042 controller, turn on IRQ12, then ask the mouse for 3-byte packets.
//! Each packet is `flags, dx, dy` where the sign bits of the deltas live in
//! the flag byte.
//!
//! The cursor is drawn straight into the framebuffer, so it saves the pixels
//! it covers and restores them before moving — nothing else re-renders the
//! area underneath.

use crate::dev::framebuffer;
use crate::serial;
use core::sync::atomic::{AtomicBool, AtomicI32, AtomicU8, AtomicU32, Ordering};

const PS2_DATA:   u16 = 0x60;
const PS2_STATUS: u16 = 0x64;
const PS2_CMD:    u16 = 0x64;

/// Cursor bitmap: 12 columns x 19 rows, MSB = leftmost of 12 bits.
const CURSOR_W: u32 = 12;
const CURSOR_H: u32 = 19;
#[rustfmt::skip]
const CURSOR_MASK: [u16; CURSOR_H as usize] = [
    0b100000000000,
    0b110000000000,
    0b111000000000,
    0b111100000000,
    0b111110000000,
    0b111111000000,
    0b111111100000,
    0b111111110000,
    0b111111111000,
    0b111111111100,
    0b111111111110,
    0b111111100000,
    0b110111100000,
    0b100011110000,
    0b000011110000,
    0b000001111000,
    0b000001111000,
    0b000000111100,
    0b000000011000,
];

/// Save/restore area: the mask plus its one-pixel drop shadow.
const SAVE_W: u32 = CURSOR_W + 1;
const SAVE_H: u32 = CURSOR_H + 1;

static MOUSE_X: AtomicI32 = AtomicI32::new(200);
static MOUSE_Y: AtomicI32 = AtomicI32::new(150);
/// bit0 left, bit1 right, bit2 middle.
static BUTTONS: AtomicU8 = AtomicU8::new(0);

/// Set on movement or button change; the event loop consumes it.
static DIRTY: AtomicBool = AtomicBool::new(false);
/// True once the init handshake succeeded.
static PRESENT: AtomicBool = AtomicBool::new(false);

/// Packets and IRQs observed (diagnostics).
pub static PACKETS: AtomicU32 = AtomicU32::new(0);
pub static IRQS: AtomicU32 = AtomicU32::new(0);

/// Packet assembly: the three bytes shift in 8 bits at a time.
static PACKET: AtomicU32 = AtomicU32::new(0);
static CYCLE: AtomicU8 = AtomicU8::new(0);

/// Where the cursor was last painted, or -1 when it is not on screen.
static mut PAINTED_X: i32 = -1;
static mut PAINTED_Y: i32 = -1;
static mut SAVE: [u32; (SAVE_W * SAVE_H) as usize] = [0; (SAVE_W * SAVE_H) as usize];

/// Current pointer position.
pub fn position() -> (i32, i32) {
    (MOUSE_X.load(Ordering::Relaxed), MOUSE_Y.load(Ordering::Relaxed))
}

/// Button state, bit0 = left.
pub fn buttons() -> u8 {
    BUTTONS.load(Ordering::Relaxed)
}

/// Did the device initialise?
pub fn present() -> bool {
    PRESENT.load(Ordering::Relaxed)
}

/// Take the "something changed" flag, clearing it.
pub fn take_dirty() -> bool {
    DIRTY.swap(false, Ordering::Relaxed)
}

/// Initialise the auxiliary PS/2 port and start the mouse reporting.
pub fn init() {
    unsafe {
        // 1. Enable the auxiliary device (the mouse port).
        ctrl_write(0xA8);

        // 2. Controller configuration byte: enable IRQ12, un-block the aux
        //    clock (bit 5 is "disable"), and enable the aux port itself.
        ctrl_write(0x20);
        let mut cb = data_read();
        cb |= 0x02;      // IRQ12 on
        cb &= !0x20;     // aux clock enabled
        ctrl_write(0x60);
        data_write(cb);

        // 3. Device: reset defaults (0xF6) then enable data reporting (0xF4).
        //    Each is answered with 0xFA; anything else means no mouse.
        let ok = mouse_cmd(0xF6) && mouse_cmd(0xF4);
        PRESENT.store(ok, Ordering::SeqCst);
    }

    let (w, h) = framebuffer::size();
    // Start in the middle of the screen; the hardcoded default would be
    // off-screen on a small mode.
    MOUSE_X.store((w / 2) as i32, Ordering::SeqCst);
    MOUSE_Y.store((h / 2) as i32, Ordering::SeqCst);

    serial::print_str("[mouse] PS/2 mouse initialized (IRQ12), present=");
    serial::print_hex(present() as u64);
    serial::print_str("\n");
}

/// Called from the IRQ12 handler.  Assembles 3-byte packets and applies them.
pub fn handle_irq() {
    IRQS.fetch_add(1, Ordering::Relaxed);

    let (status, byte) = unsafe { (port_in(PS2_STATUS), port_in(PS2_DATA)) };
    // Bit 0 = output buffer full, bit 5 = the byte came from the aux port.
    if status & 0x01 == 0 || status & 0x20 == 0 {
        return;
    }

    let cycle = CYCLE.load(Ordering::Relaxed);
    // Byte 0 always has bit 3 set; use it to resync if we ever fall out of step.
    if cycle == 0 && byte & 0x08 == 0 {
        return;
    }

    let packet = PACKET.load(Ordering::Relaxed) | ((byte as u32) << (cycle * 8));
    let next = cycle + 1;
    if next == 3 {
        CYCLE.store(0, Ordering::Relaxed);
        PACKET.store(0, Ordering::Relaxed);
        apply_packet(packet);
    } else {
        PACKET.store(packet, Ordering::Relaxed);
        CYCLE.store(next, Ordering::Relaxed);
    }
}

/// Unpack a 3-byte packet and move the pointer.
fn apply_packet(p: u32) {
    let flags = (p & 0xFF) as u8;
    if flags & 0xC0 != 0 {
        return;      // X/Y overflow: drop the packet rather than jump
    }
    let mut dx = ((p >> 8) & 0xFF) as i32;
    let mut dy = ((p >> 16) & 0xFF) as i32;
    if flags & 0x10 != 0 { dx -= 256; }    // sign bit lives in the flag byte
    if flags & 0x20 != 0 { dy -= 256; }

    let (w, h) = framebuffer::size();
    // PS/2 reports Y upward, the screen counts downward.
    let nx = (MOUSE_X.load(Ordering::Relaxed) + dx).clamp(0, w.saturating_sub(1) as i32);
    let ny = (MOUSE_Y.load(Ordering::Relaxed) - dy).clamp(0, h.saturating_sub(1) as i32);
    MOUSE_X.store(nx, Ordering::Relaxed);
    MOUSE_Y.store(ny, Ordering::Relaxed);
    BUTTONS.store(flags & 0x07, Ordering::Relaxed);

    PACKETS.fetch_add(1, Ordering::Relaxed);
    DIRTY.store(true, Ordering::Relaxed);
}

/// Repaint the cursor at its current position, restoring what it covered.
pub fn redraw() {
    restore();
    let (x, y) = position();
    save(x, y);
    blit(x, y);
}

/// Hide the cursor (restore the saved background).
pub fn hide() {
    restore();
}

/// Put back the pixels the cursor is currently covering.
fn restore() {
    unsafe {
        if PAINTED_X < 0 {
            return;
        }
        let p = core::ptr::addr_of!(SAVE) as *const u32;
        for row in 0..SAVE_H {
            for col in 0..SAVE_W {
                let v = core::ptr::read_volatile(p.add((row * SAVE_W + col) as usize));
                framebuffer::put_pixel_raw(
                    PAINTED_X as u32 + col,
                    PAINTED_Y as u32 + row,
                    v,
                );
            }
        }
        PAINTED_X = -1;
        PAINTED_Y = -1;
    }
}

/// Remember the pixels about to be covered.
fn save(x: i32, y: i32) {
    unsafe {
        // Keep the whole cursor on screen so the save/restore area is always
        // inside the framebuffer.
        let (w, h) = framebuffer::size();
        let cx = x.min(w.saturating_sub(SAVE_W) as i32).max(0);
        let cy = y.min(h.saturating_sub(SAVE_H) as i32).max(0);
        MOUSE_X.store(cx, Ordering::Relaxed);
        MOUSE_Y.store(cy, Ordering::Relaxed);

        let p = core::ptr::addr_of_mut!(SAVE) as *mut u32;
        for row in 0..SAVE_H {
            for col in 0..SAVE_W {
                let v = framebuffer::get_pixel_raw(cx as u32 + col, cy as u32 + row);
                core::ptr::write_volatile(p.add((row * SAVE_W + col) as usize), v);
            }
        }
        PAINTED_X = cx;
        PAINTED_Y = cy;
    }
}

/// Draw the arrow: a black shadow offset by one pixel, then white on top, so
/// it stays visible over both the dark desktop and the light window chrome.
fn blit(x: i32, y: i32) {
    for (row, &bits) in CURSOR_MASK.iter().enumerate() {
        for col in 0..CURSOR_W {
            if bits & (0x800 >> col) != 0 {
                framebuffer::put_pixel(x as u32 + col + 1, y as u32 + row as u32 + 1, 0, 0, 0);
            }
        }
    }
    for (row, &bits) in CURSOR_MASK.iter().enumerate() {
        for col in 0..CURSOR_W {
            if bits & (0x800 >> col) != 0 {
                framebuffer::put_pixel(x as u32 + col, y as u32 + row as u32, 0xFF, 0xFF, 0xFF);
            }
        }
    }
}

// ---------------------------------------------------------------------------
//  8042 controller access
// ---------------------------------------------------------------------------

/// Send a byte to the controller (not the device).
unsafe fn ctrl_write(v: u8) {
    wait_input_clear();
    port_out(PS2_CMD, v);
}

/// Send a byte on the data port (to the device, after a 0xD4 command).
unsafe fn data_write(v: u8) {
    wait_input_clear();
    port_out(PS2_DATA, v);
}

unsafe fn data_read() -> u8 {
    wait_output_full();
    port_in(PS2_DATA)
}

/// Send a command byte to the mouse and check for its 0xFA ack.
unsafe fn mouse_cmd(b: u8) -> bool {
    ctrl_write(0xD4);          // "next data byte goes to the aux device"
    data_write(b);
    data_read() == 0xFA
}

/// Bounded waits: a dead or absent controller must not hang the boot.
unsafe fn wait_input_clear() {
    for _ in 0..100_000 {
        if port_in(PS2_STATUS) & 0x02 == 0 {
            return;
        }
    }
}

unsafe fn wait_output_full() {
    for _ in 0..100_000 {
        if port_in(PS2_STATUS) & 0x01 != 0 {
            return;
        }
    }
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
