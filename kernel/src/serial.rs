//! COM1 serial port driver (0x3F8), 115200 8N1.
//!
//! Self-developed raw port I/O (no `x86_64` crate).  Used for headless
//! boot verification: QEMU `-serial stdio` and VMware default serial both
//! capture this output, so `barryOS booted` is assertable.

const PORT: u16 = 0x3F8;

#[inline]
pub unsafe fn outb(port: u16, val: u8) {
    core::arch::asm!(
        "out dx, al",
        in("dx") port,
        in("al") val,
        options(nomem, nostack, preserves_flags),
    );
}

#[inline]
pub unsafe fn inb(port: u16) -> u8 {
    let val: u8;
    core::arch::asm!(
        "in al, dx",
        out("al") val,
        in("dx") port,
        options(nomem, nostack, preserves_flags),
    );
    val
}

/// Initialize COM1 to 115200 baud, 8 data bits, no parity, 1 stop bit.
pub fn init() {
    unsafe {
        outb(PORT + 1, 0x00); // disable interrupts
        outb(PORT + 3, 0x80); // enable DLAB
        outb(PORT + 0, 0x01); // divisor low  = 1 -> 115200 baud
        outb(PORT + 1, 0x00); // divisor high = 0
        outb(PORT + 3, 0x03); // 8N1, disable DLAB
        outb(PORT + 2, 0xC7); // enable FIFO, clear, 14-byte threshold
        outb(PORT + 4, 0x0B); // IRQs enabled, RTS/DSR set
    }
}

fn transmit_empty() -> bool {
    unsafe { (inb(PORT + 5) & 0x20) != 0 }
}

pub fn write_byte(b: u8) {
    unsafe {
        while !transmit_empty() {
            core::hint::spin_loop();
        }
        outb(PORT, b);
    }
}

/// Write a byte, expanding '\n' to '\r' '\n' (terminal friendliness).
pub fn write_char(c: u8) {
    if c == b'\n' {
        write_byte(b'\r');
    }
    write_byte(c);
}

pub fn print_str(s: &str) {
    for &b in s.as_bytes() {
        write_char(b);
    }
}

pub fn print_hex(v: u64) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut printed = false;
    for i in (0..16).rev() {
        let nib = ((v >> (i * 4)) & 0xF) as usize;
        if nib != 0 || printed || i == 0 {
            write_byte(HEX[nib]);
            printed = true;
        }
    }
}

/// Print `v` in decimal.
///
/// Worth having here rather than in each caller: `print_hex` was the only
/// thing on offer, so several modules grew private decimal printers and the
/// log ended up mixing "0x1E63 bytes" with "30 seconds" in the same line.
pub fn print_dec(v: u64) {
    let mut buf = [0u8; 20];
    let mut i = buf.len();
    let mut n = v;
    if n == 0 {
        i -= 1;
        buf[i] = b'0';
    }
    while n > 0 {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    for &b in &buf[i..] {
        write_byte(b);
    }
}
