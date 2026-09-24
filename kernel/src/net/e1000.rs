//! Intel 82540EM ("e1000") Ethernet driver.
//!
//! Brings the adapter up (PCI, EEPROM MAC, reset, link) and gives the layers
//! above a frame-level send/receive pair.  The descriptor rings are the
//! classic legacy layout: 16-byte entries, a ring of buffer addresses that
//! both the CPU and the card walk with their own head/tail cursors.

use crate::dev::pci;
use crate::mem;
use crate::serial;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

const INTEL: u16 = 0x8086;
const DEV_82540EM: u16 = 0x100E;
const DEV_82545EM: u16 = 0x100F;
const DEV_82574L:  u16 = 0x10D3;

// --- registers -------------------------------------------------------------
const REG_CTRL:   u32 = 0x0000;
const REG_STATUS: u32 = 0x0008;
const REG_EERD:   u32 = 0x0014;
const REG_ICR:    u32 = 0x00C0;
const REG_IMC:    u32 = 0x00D8;
const REG_RCTL:   u32 = 0x0100;
const REG_TCTL:   u32 = 0x0400;
const REG_RDBAL:  u32 = 0x2800;
const REG_RDBAH:  u32 = 0x2804;
const REG_RDLEN:  u32 = 0x2808;
const REG_RDH:    u32 = 0x2810;
const REG_RDT:    u32 = 0x2818;
const REG_TDBAL:  u32 = 0x3800;
const REG_TDBAH:  u32 = 0x3804;
const REG_TDLEN:  u32 = 0x3808;
const REG_TDH:    u32 = 0x3810;
const REG_TDT:    u32 = 0x3818;
const REG_MTA:    u32 = 0x5200;   // multicast table, 128 dwords
const REG_RAL0:   u32 = 0x5400;
const REG_RAH0:   u32 = 0x5404;

const CTRL_RST:  u32 = 1 << 26;
const CTRL_SLU:  u32 = 1 << 6;
const STATUS_LU: u32 = 1 << 1;

const RCTL_EN:       u32 = 1 << 1;
const RCTL_UPE:      u32 = 1 << 3;    // unicast promiscuous
const RCTL_MPE:      u32 = 1 << 4;    // multicast promiscuous
const RCTL_BAM:      u32 = 1 << 15;   // accept broadcast
const RCTL_SECRC:    u32 = 1 << 26;   // strip CRC
const RCTL_BSIZE_2K: u32 = 0 << 16;   // 2048-byte buffers

const TCTL_EN:   u32 = 1 << 1;
const TCTL_PSP:  u32 = 1 << 3;
const TCTL_CT:   u32 = 0x10 << 4;     // collision threshold
const TCTL_COLD: u32 = 0x40 << 12;    // collision distance

/// Descriptor counts and buffer size.  Small on purpose: enough to send an
/// ARP request and see the reply.
pub const RX_DESC: usize = 8;
pub const TX_DESC: usize = 8;
pub const BUF_SIZE: usize = 2048;

/// EEPROM read bits.
const EERD_START: u32 = 1 << 0;
const EERD_DONE:  u32 = 1 << 4;

/// RX descriptor status bits.
const RXD_STAT_DD: u8 = 1 << 0;       // descriptor done
const RXD_STAT_EOP: u8 = 1 << 1;      // end of packet revision

/// TX descriptor command bits (legacy layout, byte 11 of the descriptor).
///
/// These are not optional.  EOP is what tells the card the descriptor holds a
/// complete packet — without it the transmit engine keeps waiting for the rest
/// of one and nothing ever leaves the adapter.  IFCS appends the Ethernet FCS,
/// and RS asks for the done bit to be set in the status byte.
///
/// The first version of this driver wrote zero here, so `send_frame` filled
/// buffers, bumped TDT and reported success while the wire stayed silent:
/// every ARP request and every DHCP DISCOVER went into the ring and stopped
/// there.
const TXD_CMD_EOP: u8 = 1 << 0;
const TXD_CMD_IFCS: u8 = 1 << 1;
const TXD_CMD_RS: u8 = 1 << 3;
const TXD_STAT_DD: u8 = 1 << 0;

/// Transmits whose descriptor never came back marked done.
pub static TX_STALLS: core::sync::atomic::AtomicU32 =
    core::sync::atomic::AtomicU32::new(0);

static PRESENT: AtomicBool = AtomicBool::new(false);
static MMIO_BASE: AtomicU32 = AtomicU32::new(0);
static MAC_LO: AtomicU32 = AtomicU32::new(0);
static MAC_HI: AtomicU32 = AtomicU32::new(0);

/// Physical (== virtual, the kernel identity-maps) addresses of the rings and
/// their buffers, and the cursors we own.
static RX_RING: AtomicU32 = AtomicU32::new(0);
static RX_BUFS: AtomicU32 = AtomicU32::new(0);
static RX_CUR: AtomicU32 = AtomicU32::new(0);   // next descriptor to inspect

static TX_RING: AtomicU32 = AtomicU32::new(0);
static TX_BUFS: AtomicU32 = AtomicU32::new(0);
static TX_TAIL: AtomicU32 = AtomicU32::new(0);

pub static TX_FRAMES: AtomicU32 = AtomicU32::new(0);
pub static RX_FRAMES: AtomicU32 = AtomicU32::new(0);

pub fn present() -> bool {
    PRESENT.load(Ordering::Relaxed)
}

pub fn mac() -> Option<[u8; 6]> {
    if !present() {
        return None;
    }
    let lo = MAC_LO.load(Ordering::Relaxed);
    let hi = MAC_HI.load(Ordering::Relaxed);
    Some([
        lo as u8,
        (lo >> 8) as u8,
        (lo >> 16) as u8,
        (lo >> 24) as u8,
        hi as u8,
        (hi >> 8) as u8,
    ])
}

/// Is the link up (cable/port connected)?
pub fn link_up() -> bool {
    present() && (mmio_read(REG_STATUS) & STATUS_LU) != 0
}

// ---------------------------------------------------------------------------
//  Init
// ---------------------------------------------------------------------------

pub fn init() {
    let dev = match find_adapter() {
        Some(d) => d,
        None => {
            serial::print_str("[e1000] no Intel Ethernet adapter on the PCI bus\n");
            return;
        }
    };

    let bar = dev.bar0_addr as u32;
    if bar == 0 {
        serial::print_str("[e1000] adapter found but BAR0 is not a memory BAR\n");
        return;
    }
    MMIO_BASE.store(bar, Ordering::SeqCst);

    serial::print_str("[e1000] found ");
    serial::print_hex(dev.vendor as u64);
    serial::print_str(":");
    serial::print_hex(dev.device as u64);
    serial::print_str(" at ");
    serial::print_hex(dev.bus as u64);
    serial::print_str(":");
    serial::print_hex(dev.dev as u64);
    serial::print_str(".");
    serial::print_hex(dev.func as u64);
    serial::print_str(", MMIO 0x");
    serial::print_hex(bar as u64);
    serial::print_str("\n");

    // Memory space (bit 1) + bus master (bit 2).  The command register is the
    // low half of the dword at 0x04; the high half is the status register,
    // whose bits are write-1-to-clear, so we write it back as zero.
    let cmd = pci::config_read_u16(dev.bus, dev.dev, dev.func, 0x04) | 0x0006;
    pci::config_write_u32(dev.bus, dev.dev, dev.func, 0x04, cmd as u32);

    // Mask every interrupt source: we poll.
    mmio_write(REG_IMC, 0xFFFF_FFFF);

    // Reset, then wait for the card to clear the bit itself.
    let ctrl = mmio_read(REG_CTRL);
    mmio_write(REG_CTRL, ctrl | CTRL_RST);
    for _ in 0..1_000_000 {
        if mmio_read(REG_CTRL) & CTRL_RST == 0 {
            break;
        }
    }
    // Reset clears the interrupt mask, so set it again.
    mmio_write(REG_IMC, 0xFFFF_FFFF);
    mmio_write(REG_MTA, 0);          // (the array is zeroed below)

    // MAC: RAL0/RAH0 are loaded from the EEPROM by hardware, but only when the
    // EEPROM is present and auto-read ran, so fall back to reading it directly.
    let lo = mmio_read(REG_RAL0);
    let hi = mmio_read(REG_RAH0);
    let (lo, hi) = if lo == 0 && (hi & 0xFFFF) == 0 {
        match eeprom_mac() {
            Some((l, h)) => (l, h),
            None => {
                serial::print_str("[e1000] MAC unavailable (no EEPROM response)\n");
                return;
            }
        }
    } else {
        (lo, hi)
    };
    MAC_LO.store(lo, Ordering::SeqCst);
    MAC_HI.store(hi, Ordering::SeqCst);
    PRESENT.store(true, Ordering::SeqCst);

    let m = mac().unwrap();
    serial::print_str("[e1000] MAC ");
    print_mac(&m);
    serial::print_str("\n");

    // Clear the multicast table (128 dwords).
    for i in 0..128 {
        mmio_write(REG_MTA + i * 4, 0);
    }

    if !rings_init() {
        serial::print_str("[e1000] descriptor rings could not be allocated\n");
        PRESENT.store(false, Ordering::SeqCst);
        return;
    }

    // Bring the link up.
    let ctrl = mmio_read(REG_CTRL);
    mmio_write(REG_CTRL, ctrl | CTRL_SLU);
    for _ in 0..1_000_000 {
        if mmio_read(REG_STATUS) & STATUS_LU != 0 {
            break;
        }
    }

    serial::print_str("[e1000] link ");
    if link_up() {
        serial::print_str("up");
    } else {
        serial::print_str("down");
    }
    serial::print_str(", status 0x");
    serial::print_hex(mmio_read(REG_STATUS) as u64);
    serial::print_str(", rings ready\n");
}

fn find_adapter() -> Option<pci::PciDevice> {
    for id in [DEV_82540EM, DEV_82545EM, DEV_82574L] {
        if let Some(d) = pci::find(INTEL, id) {
            return Some(d);
        }
    }
    pci::find_class(0x02, 0x00)
}

/// Allocate and program the RX and TX descriptor rings.
fn rings_init() -> bool {
    let fa = mem::frame_allocator();

    // Rings: 8 entries x 16 bytes fits in one 4 KiB frame each.
    let rx_ring = fa.alloc_contig(1);
    let tx_ring = fa.alloc_contig(1);
    // Buffers: 8 x 2048 = 16 KiB = 4 frames each.
    let rx_bufs = fa.alloc_contig(4);
    let tx_bufs = fa.alloc_contig(4);
    if rx_ring == 0 || tx_ring == 0 || rx_bufs == 0 || tx_bufs == 0 {
        return false;
    }
    RX_RING.store(rx_ring as u32, Ordering::SeqCst);
    TX_RING.store(tx_ring as u32, Ordering::SeqCst);
    RX_BUFS.store(rx_bufs as u32, Ordering::SeqCst);
    TX_BUFS.store(tx_bufs as u32, Ordering::SeqCst);

    unsafe {
        for i in 0..RX_DESC {
            let d = (rx_ring as *mut u8).add(i * 16);
            // buffer address
            core::ptr::write_volatile(d as *mut u64, rx_bufs + (i * BUF_SIZE) as u64);
            // length, checksum, status, errors, special
            for b in 8..16 {
                core::ptr::write_volatile(d.add(b), 0);
            }
        }
        for i in 0..TX_DESC {
            let d = (tx_ring as *mut u8).add(i * 16);
            core::ptr::write_volatile(d as *mut u64, tx_bufs + (i * BUF_SIZE) as u64);
            for b in 8..16 {
                core::ptr::write_volatile(d.add(b), 0);
            }
        }
    }

    // RX: ring base/length, then enable.  RDT points at the last descriptor
    // the card may use, which is how it knows where the CPU's cursor is.
    mmio_write(REG_RDBAL, rx_ring as u32);
    mmio_write(REG_RDBAH, 0);
    mmio_write(REG_RDLEN, (RX_DESC * 16) as u32);
    mmio_write(REG_RDH, 0);
    mmio_write(REG_RDT, (RX_DESC - 1) as u32);
    mmio_write(REG_RCTL, RCTL_EN | RCTL_BAM | RCTL_UPE | RCTL_MPE | RCTL_SECRC | RCTL_BSIZE_2K);

    // TX: ring base/length, then enable.
    mmio_write(REG_TDBAL, tx_ring as u32);
    mmio_write(REG_TDBAH, 0);
    mmio_write(REG_TDLEN, (TX_DESC * 16) as u32);
    mmio_write(REG_TDH, 0);
    mmio_write(REG_TDT, 0);
    mmio_write(REG_TCTL, TCTL_EN | TCTL_PSP | TCTL_CT | TCTL_COLD);

    RX_CUR.store(0, Ordering::SeqCst);
    TX_TAIL.store(0, Ordering::SeqCst);
    true
}

// ---------------------------------------------------------------------------
//  Frame path
// ---------------------------------------------------------------------------

/// Copy a frame into the next TX buffer and hand the descriptor to the card.
pub fn send_frame(frame: &[u8]) -> bool {
    if !present() || frame.is_empty() || frame.len() > BUF_SIZE {
        return false;
    }
    let ring = TX_RING.load(Ordering::Relaxed);
    let bufs = TX_BUFS.load(Ordering::Relaxed);
    if ring == 0 {
        return false;
    }
    let tail = TX_TAIL.load(Ordering::Relaxed) as usize;

    unsafe {
        let buf = (bufs + (tail * BUF_SIZE) as u32) as *mut u8;
        for (i, &b) in frame.iter().enumerate() {
            core::ptr::write_volatile(buf.add(i), b);
        }
        let d = (ring as *mut u8).add(tail * 16);
        // length
        core::ptr::write_volatile(d.add(8) as *mut u16, frame.len() as u16);
        // cso = 0 (no checksum offload), then the command byte, then status,
        // css and special all cleared.
        core::ptr::write_volatile(d.add(10), 0);
        core::ptr::write_volatile(d.add(11), TXD_CMD_EOP | TXD_CMD_IFCS | TXD_CMD_RS);
        for b in 12..16 {
            core::ptr::write_volatile(d.add(b), 0);
        }
    }

    let next = ((tail + 1) % TX_DESC) as u32;
    TX_TAIL.store(next, Ordering::Relaxed);
    mmio_write(REG_TDT, next);
    TX_FRAMES.fetch_add(1, Ordering::Relaxed);

    // Wait for the card to mark the descriptor done.  This is not needed to
    // transmit — the ring works either way — but without it "the frame went
    // out" is an assumption, and an assumption is what let a zeroed command
    // byte sit here unnoticed while every packet was dropped on the floor.
    let mut spins = 0u32;
    loop {
        let status = unsafe {
            core::ptr::read_volatile((ring as *const u8).add(tail * 16).add(12))
        };
        if status & TXD_STAT_DD != 0 {
            break;
        }
        spins += 1;
        if spins > 1_000_000 {
            TX_STALLS.fetch_add(1, Ordering::Relaxed);
            break;
        }
    }
    true
}

/// Take one received frame, if any is waiting.  Returns its length.
pub fn recv_frame(out: &mut [u8]) -> Option<usize> {
    if !present() {
        return None;
    }
    let ring = RX_RING.load(Ordering::Relaxed);
    let bufs = RX_BUFS.load(Ordering::Relaxed);
    if ring == 0 {
        return None;
    }
    let cur = RX_CUR.load(Ordering::Relaxed) as usize;

    let (len, status) = unsafe {
        let d = (ring as *const u8).add(cur * 16);
        let status = core::ptr::read_volatile(d.add(12));
        let len = core::ptr::read_volatile(d.add(8) as *const u16) as usize;
        (len, status)
    };
    // The card sets DD when it has filled this descriptor.
    if status & RXD_STAT_DD == 0 {
        return None;
    }
    let n = len.min(out.len());
    unsafe {
        let buf = (bufs + (cur * BUF_SIZE) as u32) as *const u8;
        for i in 0..n {
            out[i] = core::ptr::read_volatile(buf.add(i));
        }
        // Hand the descriptor back: clear status, then move the card's tail.
        let d = (ring as *mut u8).add(cur * 16);
        core::ptr::write_volatile(d.add(12), 0);
    }
    let next = (cur + 1) % RX_DESC;
    RX_CUR.store(next as u32, Ordering::Relaxed);
    mmio_write(REG_RDT, cur as u32);
    RX_FRAMES.fetch_add(1, Ordering::Relaxed);
    let _ = RXD_STAT_EOP;
    Some(n)
}

// ---------------------------------------------------------------------------
//  EEPROM
// ---------------------------------------------------------------------------

fn eeprom_read(addr: u8) -> Option<u16> {
    mmio_write(REG_EERD, ((addr as u32) << 8) | EERD_START);
    for _ in 0..100_000 {
        let v = mmio_read(REG_EERD);
        if v & EERD_DONE != 0 {
            return Some((v >> 16) as u16);
        }
    }
    None
}

fn eeprom_mac() -> Option<(u32, u32)> {
    let w0 = eeprom_read(0)? as u32;
    let w1 = eeprom_read(1)? as u32;
    let w2 = eeprom_read(2)? as u32;
    Some(((w1 << 16) | w0, w2))
}

fn print_mac(m: &[u8; 6]) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for (i, b) in m.iter().enumerate() {
        if i > 0 {
            serial::print_str(":");
        }
        let s = [HEX[(b >> 4) as usize], HEX[(b & 0xF) as usize]];
        serial::print_str(core::str::from_utf8(&s).unwrap_or("??"));
    }
}

// ---------------------------------------------------------------------------
//  MMIO
// ---------------------------------------------------------------------------

#[inline]
fn mmio_read(reg: u32) -> u32 {
    let base = MMIO_BASE.load(Ordering::Relaxed) as u64;
    if base == 0 {
        return 0;
    }
    unsafe { core::ptr::read_volatile((base + reg as u64) as *const u32) }
}

#[inline]
fn mmio_write(reg: u32, val: u32) {
    let base = MMIO_BASE.load(Ordering::Relaxed) as u64;
    if base == 0 {
        return;
    }
    unsafe { core::ptr::write_volatile((base + reg as u64) as *mut u32, val) }
}
