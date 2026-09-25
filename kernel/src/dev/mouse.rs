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
use crate::dev::ps2;
use crate::serial;
use core::sync::atomic::{AtomicBool, AtomicI32, AtomicU8, AtomicU32, Ordering};

use crate::dev::ps2::{DATA as PS2_DATA, STATUS as PS2_STATUS};

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
        // 1. Enable the auxiliary device (the mouse port).  The controller's
        //    configuration byte — IRQ12 among its bits — belongs to the
        //    keyboard driver, which writes it once and knows what it wrote.
        ps2::ctrl_write(0xA8);

        // 2. Anything the controller said before we asked it anything is
        //    about to be read as though it were the mouse's answer.
        ps2::drain_output();

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

    // The status is read *before* the byte, and the byte only once it is known
    // to be the mouse's.  Both devices share one output buffer, and reading it
    // takes the byte out of it for good: a handler that reads first and checks
    // afterwards destroys whatever was there.  A keyboard byte in this buffer
    // is a keystroke that nobody will ever see again, because by the time the
    // keyboard's own interrupt arrives the byte it was going to read is gone.
    let status = unsafe { ps2::status() };
    if status & ps2::ST_OUTPUT_FULL == 0 {
        return;                              // a spurious IRQ12, nothing waiting
    }
    if status & ps2::ST_FROM_AUX == 0 {
        // The keyboard's byte, delivered to the mouse's interrupt — which is
        // what `keyboard.allowBothIRQs` in a VMware configuration asks for.
        // It is still the keyboard's byte, so it goes to the keyboard.
        crate::dev::keyboard::handle_scancode(unsafe { ps2::read_data() });
        return;
    }
    let byte = unsafe { ps2::read_data() };

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

/// Send a command byte to the mouse and check for its 0xFA ack.
unsafe fn mouse_cmd(b: u8) -> bool {
    ps2::ctrl_write(0xD4);     // "the next data byte goes to the aux device"
    ps2::data_write(b);
    ps2::data_read() == 0xFA
}
