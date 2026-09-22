//! ATA (PIO) block device driver.
//!
//! Programmed I/O on the legacy IDE ports: no DMA, no interrupts, just poll
//! the status register and move 256 words per sector through the data port.
//! Slow, but it is the one interface every x86 machine — and every hypervisor
//! pretending to be one — has supported for thirty years.
//!
//! The kernel identity-maps the low 4 GiB writable, so a buffer can be handed
//! straight to `rep insw` without a bounce buffer.

use crate::serial;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

/// Command block and control ports for the two IDE channels.
const PRIMARY_IO: u16 = 0x1F0;
const PRIMARY_CTRL: u16 = 0x3F6;
const SECONDARY_IO: u16 = 0x170;
const SECONDARY_CTRL: u16 = 0x376;

// Register offsets from the channel's I/O base.
const REG_DATA: u16 = 0;
const REG_ERROR: u16 = 1;
const REG_FEATURES: u16 = 1;
const REG_SECCOUNT: u16 = 2;
const REG_LBA0: u16 = 3;
const REG_LBA1: u16 = 4;
const REG_LBA2: u16 = 5;
const REG_DRIVE: u16 = 6;
const REG_STATUS: u16 = 7;      // read
const REG_COMMAND: u16 = 7;     // write

// Status bits.
const ST_BSY: u8 = 1 << 7;
const ST_DRDY: u8 = 1 << 6;
const ST_DF: u8 = 1 << 5;
const ST_DRQ: u8 = 1 << 3;
const ST_ERR: u8 = 1 << 0;

// Commands.
const CMD_READ: u8 = 0x20;      // read sectors, with retry
const CMD_WRITE: u8 = 0x30;     // write sectors, with retry
const CMD_FLUSH: u8 = 0xE7;     // flush write cache
const CMD_IDENTIFY: u8 = 0xEC;

/// Sector size on every ATA disk.
pub const SECTOR_SIZE: usize = 512;

/// How long to wait for the drive, in poll iterations.  A timeout matters:
/// without one, a missing or wedged drive hangs the whole machine.
const POLL_LIMIT: u32 = 4_000_000;

/// One discovered drive.
#[derive(Clone, Copy)]
pub struct Drive {
    pub channel: u8,        // 0 = primary, 1 = secondary
    pub slave: bool,
    /// 28-bit LBA capacity in sectors (LBA48 is not used).
    pub sectors: u32,
    pub model: [u8; 24],
    pub model_len: usize,
}

impl Drive {
    const fn empty() -> Self {
        Self { channel: 0, slave: false, sectors: 0, model: [0; 24], model_len: 0 }
    }
    pub fn model_str(&self) -> &str {
        core::str::from_utf8(&self.model[..self.model_len]).unwrap_or("?")
    }
}

const MAX_DRIVES: usize = 4;
static mut DRIVES: [Drive; MAX_DRIVES] = [Drive::empty(); MAX_DRIVES];
static DRIVE_COUNT: AtomicU32 = AtomicU32::new(0);
static PRESENT: AtomicBool = AtomicBool::new(false);
/// Sectors read and written, for diagnostics.
pub static READS: AtomicU64 = AtomicU64::new(0);
pub static WRITES: AtomicU64 = AtomicU64::new(0);

pub fn present() -> bool {
    PRESENT.load(Ordering::Relaxed)
}

pub fn count() -> usize {
    DRIVE_COUNT.load(Ordering::Relaxed) as usize
}

pub fn drive(i: usize) -> Option<Drive> {
    if i >= count() {
        return None;
    }
    unsafe { Some(*(core::ptr::addr_of!(DRIVES) as *const Drive).add(i)) }
}

/// Reserve a range of sectors, so two subsystems cannot both think a region is
/// theirs.  Layout is fixed by the installer, not discovered.
pub fn init() {
    serial::print_str("[ata] probing IDE channels...\n");

    let mut n = 0usize;
    for channel in 0u8..2 {
        for slave in [false, true] {
            if n >= MAX_DRIVES {
                break;
            }
            if let Some(d) = identify(channel, slave) {
                unsafe {
                    let p = core::ptr::addr_of_mut!(DRIVES) as *mut Drive;
                    core::ptr::write_volatile(p.add(n), d);
                }
                n += 1;
                serial::print_str("[ata] ");
                serial::print_str(if channel == 0 { "primary" } else { "secondary" });
                serial::print_str(if slave { " slave  " } else { " master " });
                serial::print_str(d.model_str());
                serial::print_str(", ");
                serial::print_hex((d.sectors as u64) / 2048);
                serial::print_str(" MiB\n");
            }
        }
    }

    DRIVE_COUNT.store(n as u32, Ordering::Release);
    PRESENT.store(n > 0, Ordering::Release);
    serial::print_str("[ata] ");
    serial::print_hex(n as u64);
    serial::print_str(" drive(s) online\n");
}

/// Ask a drive to identify itself.  Returns None if nothing answers.
fn identify(channel: u8, slave: bool) -> Option<Drive> {
    let io = if channel == 0 { PRIMARY_IO } else { SECONDARY_IO };
    let ctrl = if channel == 0 { PRIMARY_CTRL } else { SECONDARY_CTRL };

    unsafe {
        // Selecting the slave also floats the master, so give the bus time.
        port_out(ctrl, 0);
        port_out(io + REG_DRIVE, if slave { 0xB0 } else { 0xA0 });
        delay_400ns(ctrl);

        port_out(io + REG_SECCOUNT, 0);
        port_out(io + REG_LBA0, 0);
        port_out(io + REG_LBA1, 0);
        port_out(io + REG_LBA2, 0);
        port_out(io + REG_COMMAND, CMD_IDENTIFY);

        // A non-ATA device clears the LBA registers to zero.
        let st = poll_status(io, 0)?;
        if st == 0 {
            return None;
        }
        if port_in(io + REG_LBA1) != 0 || port_in(io + REG_LBA2) != 0 {
            return None;                    // ATAPI, not a disk
        }

        // Wait for the data to be ready.
        if !wait_drq(io) {
            return None;
        }

        let mut buf = [0u16; 256];
        port_read_words(io, &mut buf, 256);

        // Words 60..86 hold the model string, byte-swapped within each word.
        let mut d = Drive { channel, slave, sectors: 0, model: [0; 24], model_len: 0 };
        let mut k = 0usize;
        for i in 27..47 {
            let w = buf[i];
            for b in [(w >> 8) as u8, (w & 0xFF) as u8] {
                if b == 0 || b == b' ' && k == 0 {
                    continue;
                }
                if k < d.model.len() {
                    d.model[k] = b;
                    k += 1;
                }
            }
        }
        // Trim trailing spaces.
        while k > 0 && d.model[k - 1] == b' ' {
            k -= 1;
        }
        d.model_len = k;

        // Words 60 and 61 are the 28-bit LBA capacity, low word first.
        d.sectors = (buf[60] as u32) | ((buf[61] as u32) << 16);
        Some(d)
    }
}

/// Read `count` sectors starting at `lba` into `buf`.
///
/// LBA28 only: every disk this will meet in a VM is far below the 128 GiB
/// ceiling, and extended addressing would double the driver for no benefit
/// today.
pub fn read_sectors(drive_idx: usize, lba: u32, count: u8, buf: &mut [u8]) -> bool {
    let Some(d) = drive(drive_idx) else { return false };
    let need = count as usize * SECTOR_SIZE;
    if buf.len() < need || count == 0 {
        return false;
    }
    let io = if d.channel == 0 { PRIMARY_IO } else { SECONDARY_IO };
    let ctrl = if d.channel == 0 { PRIMARY_CTRL } else { SECONDARY_CTRL };

    unsafe {
        if !issue(d, io, ctrl, lba, count, CMD_READ) {
            return false;
        }
        for s in 0..count as usize {
            if !wait_drq(io) {
                return false;
            }
            let dst = buf.as_mut_ptr().add(s * SECTOR_SIZE) as *mut u16;
            port_read_words(io, core::slice::from_raw_parts_mut(dst, 256), 256);
        }
    }
    READS.fetch_add(count as u64, Ordering::Relaxed);
    true
}

/// Write `count` sectors starting at `lba` from `buf`.
pub fn write_sectors(drive_idx: usize, lba: u32, count: u8, buf: &[u8]) -> bool {
    let Some(d) = drive(drive_idx) else { return false };
    let need = count as usize * SECTOR_SIZE;
    if buf.len() < need || count == 0 {
        return false;
    }
    let io = if d.channel == 0 { PRIMARY_IO } else { SECONDARY_IO };
    let ctrl = if d.channel == 0 { PRIMARY_CTRL } else { SECONDARY_CTRL };

    unsafe {
        if !issue(d, io, ctrl, lba, count, CMD_WRITE) {
            return false;
        }
        for s in 0..count as usize {
            if !wait_drq(io) {
                return false;
            }
            let src = buf.as_ptr().add(s * SECTOR_SIZE) as *const u16;
            port_write_words(io, core::slice::from_raw_parts(src, 256), 256);
        }
        // The drive acknowledges a write before the data is on the platter.
        port_out(io + REG_COMMAND, CMD_FLUSH);
        let _ = wait_not_busy(io);
    }
    WRITES.fetch_add(count as u64, Ordering::Relaxed);
    true
}

/// Select the drive and issue a read/write command.  Leaves the drive ready
/// for the data transfer.
unsafe fn issue(d: Drive, io: u16, ctrl: u16, lba: u32, count: u8, cmd: u8) -> bool {
    if lba + count as u32 > d.sectors {
        serial::print_str("[ata] refusing access past the end of the disk\n");
        return false;
    }
    port_out(ctrl, 0);
    // Bits 24..27 of the LBA go in the drive/head register, plus the master/
    // slave bit.  0xE0 selects LBA mode.
    let head = 0xE0 | (if d.slave { 0x10 } else { 0 }) | (((lba >> 24) & 0x0F) as u8);
    port_out(io + REG_DRIVE, head);
    delay_400ns(ctrl);

    // If a previous command left the drive busy, bail out rather than hang.
    if !wait_not_busy(io) {
        return false;
    }

    port_out(io + REG_SECCOUNT, count);
    port_out(io + REG_LBA0, (lba & 0xFF) as u8);
    port_out(io + REG_LBA1, ((lba >> 8) & 0xFF) as u8);
    port_out(io + REG_LBA2, ((lba >> 16) & 0xFF) as u8);
    port_out(io + REG_COMMAND, cmd);

    // 0x00 in the status register means the drive does not exist.
    let st = poll_status(io, ctrl);
    match st {
        Some(s) if s & ST_ERR == 0 => true,
        _ => {
            serial::print_str("[ata] command failed, status/error:\n");
            false
        }
    }
}

/// Poll until the drive is not busy, with a timeout.
unsafe fn wait_not_busy(io: u16) -> bool {
    let mut n = 0u32;
    while n < POLL_LIMIT {
        let st = port_in(io + REG_STATUS);
        if st == 0 {
            return false;                   // drive vanished
        }
        if st & ST_BSY == 0 {
            return true;
        }
        n += 1;
    }
    serial::print_str("[ata] timeout waiting for BSY to clear\n");
    false
}

/// Wait for the drive to be ready to transfer.
unsafe fn wait_drq(io: u16) -> bool {
    let mut n = 0u32;
    while n < POLL_LIMIT {
        let st = port_in(io + REG_STATUS);
        if st & ST_BSY == 0 && st & ST_DRQ != 0 {
            return true;
        }
        if st & (ST_ERR | ST_DF) != 0 {
            serial::print_str("[ata] drive reported an error\n");
            return false;
        }
        n += 1;
    }
    serial::print_str("[ata] timeout waiting for DRQ\n");
    false
}

/// Initial status read after a command.
unsafe fn poll_status(io: u16, _ctrl: u16) -> Option<u8> {
    let mut n = 0u32;
    while n < POLL_LIMIT {
        let st = port_in(io + REG_STATUS);
        if st == 0 {
            return None;
        }
        if st & ST_BSY == 0 {
            return Some(st);
        }
        n += 1;
    }
    None
}

/// The 400 ns settle time after selecting a drive, done by reading the
/// alternate status register four times.
unsafe fn delay_400ns(ctrl: u16) {
    for _ in 0..4 {
        let _ = port_in(ctrl);
    }
}

// ---------------------------------------------------------------------------
//  Port I/O
// ---------------------------------------------------------------------------

#[inline]
unsafe fn port_in(port: u16) -> u8 {
    let v: u8;
    core::arch::asm!("in al, dx", out("al") v, in("dx") port,
                     options(nostack, nomem, preserves_flags));
    v
}

#[inline]
unsafe fn port_out(port: u16, val: u8) {
    core::arch::asm!("out dx, al", in("dx") port, in("al") val,
                     options(nostack, nomem, preserves_flags));
}

/// Move `n` words from the data port into `buf`.
unsafe fn port_read_words(port: u16, buf: &mut [u16], n: usize) {
    for i in 0..n {
        let v: u16;
        core::arch::asm!("in ax, dx", out("ax") v, in("dx") port,
                         options(nostack, nomem, preserves_flags));
        buf[i] = v;
    }
}

/// Move `n` words from `buf` to the data port.
unsafe fn port_write_words(port: u16, buf: &[u16], n: usize) {
    for i in 0..n {
        core::arch::asm!("out dx, ax", in("dx") port, in("ax") buf[i],
                         options(nostack, nomem, preserves_flags));
    }
}
