//! Installer — writes the running system onto a hard disk.
//!
//! Runs before the login screen, because it has to work on a machine that has
//! no accounts yet and is about to have its disk overwritten.
//!
//! What gets written:
//!
//!   LBA 0        MBR                 (embedded in the kernel image)
//!   LBA 1..40    stage2 loader       (embedded, header patched with the size
//!                                     of the kernel we are writing)
//!   LBA 41..     the running kernel  (read straight out of this address space)
//!   LBA 2048..   barryFS             (the current in-memory filesystem)
//!
//! The boot chain is carried inside the kernel rather than read back off the
//! boot medium: a CD would need an ATAPI driver we do not have, so an
//! installer that only worked when booted from a hard disk would not be much
//! of an installer.

use crate::bootimg;
use crate::dev::{ata, framebuffer};
use crate::fs::{diskfs, vfs};
use crate::serial;
use crate::wm::font;
use crate::wm::window;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

const WIN_X: u32 = 60;
const WIN_Y: u32 = 40;
const WIN_W: u32 = 540;
const WIN_H: u32 = 380;

const LIST_Y: u32 = 56;
const ROW_H: u32 = 20;
const MAX_ROWS: usize = 4;
const BTN_Y: u32 = 156;
const BTN_H: u32 = 26;
const PROG_Y: u32 = 196;
const LOG_Y: u32 = 226;
const LOG_LINES: usize = 7;

/// Where the kernel is loaded, and where the kernel ends (the linker puts
/// `.bss` right after the loaded image).
const KERNEL_LOAD: usize = 0x0010_0000;
extern "C" {
    static __bss_start: u8;
}

/// LBA of the kernel on an installed disk — must match `KERNEL_DISK_LBA` in
/// stage2.asm.
const KERNEL_LBA: u32 = 41;

/// Sector offset of the kernel-size field in stage2's header.
const STAGE2_SIZE_OFFSET: usize = 4;

static WIN_ID: AtomicU64 = AtomicU64::new(0);
static SELECTED: AtomicU64 = AtomicU64::new(0);
static BUSY: AtomicBool = AtomicBool::new(false);
static DONE: AtomicBool = AtomicBool::new(false);
/// 0..100
static PROGRESS: AtomicU64 = AtomicU64::new(0);

static mut LOG: [[u8; 64]; LOG_LINES] = [[0; 64]; LOG_LINES];
static mut LOG_LEN: [usize; LOG_LINES] = [0; LOG_LINES];
static mut LOG_HEAD: usize = 0;

fn log(msg: &str) {
    serial::print_str("[install] ");
    serial::print_str(msg);
    serial::print_str("\n");
    unsafe {
        let h = LOG_HEAD;
        let n = msg.len().min(64);
        let line = &mut LOG[h];
        line[..n].copy_from_slice(&msg.as_bytes()[..n]);
        LOG_LEN[h] = n;
        LOG_HEAD = (h + 1) % LOG_LINES;
    }
}

pub fn window_id() -> u64 {
    WIN_ID.load(Ordering::Relaxed)
}

/// Open the installer window.
pub fn open() -> u64 {
    let id = window::create(WIN_X, WIN_Y, WIN_W, WIN_H, "Install barryOS");
    WIN_ID.store(id, Ordering::SeqCst);
    // Default to the largest disk: the spare one, not the small image we may
    // well be running from.
    let mut best = 0usize;
    for i in 0..ata::count() {
        if let (Some(a), Some(b)) = (ata::drive(i), ata::drive(best)) {
            if a.sectors > b.sectors {
                best = i;
            }
        }
    }
    SELECTED.store(best as u64, Ordering::SeqCst);
    log("ready. choose a disk and press Install.");
    id
}

fn geometry() -> (u32, u32, u32, u32) {
    window::geometry(window_id()).unwrap_or((WIN_X, WIN_Y, WIN_W, WIN_H))
}

fn row_rect(i: usize) -> (u32, u32, u32, u32) {
    (8, LIST_Y + (i as u32) * ROW_H, WIN_W - 16, ROW_H - 2)
}

fn button_rect(i: usize) -> (u32, u32, u32, u32) {
    let w = 130u32;
    (8 + (i as u32) * (w + 8), BTN_Y, w, BTN_H)
}

// ---------------------------------------------------------------------------
//  Input
// ---------------------------------------------------------------------------

pub fn on_click(px: u32, py: u32) -> bool {
    let (wx, wy, w, h) = geometry();
    if px < wx || py < wy || px >= wx + w || py >= wy + h {
        return false;
    }
    let lx = px - wx;
    let ly = py - wy;

    if BUSY.load(Ordering::Relaxed) {
        return true;                        // ignore clicks mid-install
    }

    // Disk list.
    if ly >= LIST_Y && ly < LIST_Y + (MAX_ROWS as u32) * ROW_H {
        let row = ((ly - LIST_Y) / ROW_H) as usize;
        if row < ata::count() {
            SELECTED.store(row as u64, Ordering::SeqCst);
        }
        return true;
    }

    // Buttons.
    if ly >= BTN_Y && ly < BTN_Y + BTN_H {
        for i in 0..3 {
            let (bx, by, bw, bh) = button_rect(i);
            if lx >= bx && lx < bx + bw && ly >= by && ly < by + bh {
                match i {
                    0 => start_install(),
                    1 => reboot(),
                    2 => { /* Quit: nothing to quit to yet. */ }
                    _ => {}
                }
                return true;
            }
        }
    }
    true
}

/// Ask the keyboard controller to pulse the reset line.
fn reboot() -> ! {
    log("rebooting...");
    unsafe {
        // 0xFE on the 8042 command port is the classic CPU reset.
        core::arch::asm!("out dx, al", in("dx") 0x64u16, in("al") 0xFEu8);
    }
    // If that did not take, spin rather than execute whatever follows.
    loop {
        unsafe { core::arch::asm!("hlt", options(nostack, nomem, preserves_flags)) };
    }
}

// ---------------------------------------------------------------------------
//  The install itself
// ---------------------------------------------------------------------------

fn start_install() {
    if !BUSY.swap(true, Ordering::SeqCst) {
        // The work is done on the next render pass, so the UI can show the
        // progress bar before the first write blocks the loop.
        PENDING.store(true, Ordering::SeqCst);
    }
}

static PENDING: AtomicBool = AtomicBool::new(false);

/// Called from the input loop; performs the install once, between frames.
pub fn tick() -> bool {
    if !PENDING.swap(false, Ordering::SeqCst) {
        return false;
    }
    run_install();
    BUSY.store(false, Ordering::SeqCst);
    true
}

fn set_progress(p: u64) {
    PROGRESS.store(p.min(100), Ordering::Relaxed);
}

fn run_install() {
    let idx = SELECTED.load(Ordering::Relaxed) as usize;
    let Some(d) = ata::drive(idx) else {
        log("no such disk");
        return;
    };

    log("installing...");
    set_progress(2);

    // 1. Format the filesystem and write the current tree into it.
    if !diskfs::format(idx) {
        log("FAILED: could not format");
        return;
    }
    set_progress(10);

    if !diskfs::save(idx) {
        log("FAILED: could not write the filesystem");
        return;
    }
    set_progress(35);
    log("filesystem written");

    // 2. The kernel, straight out of this address space.
    let end = core::ptr::addr_of!(__bss_start) as usize;
    let size = end.saturating_sub(KERNEL_LOAD);
    if size == 0 || size > 8 * 1024 * 1024 {
        log("FAILED: implausible kernel size");
        return;
    }
    if !write_image(idx, KERNEL_LBA, KERNEL_LOAD, size) {
        log("FAILED: could not write the kernel");
        return;
    }
    set_progress(75);
    log("kernel written");

    // 3. stage2, with its kernel-size header filled in.
    let mut loader = bootimg::STAGE2;
    let sz = size as u32;
    loader[STAGE2_SIZE_OFFSET..STAGE2_SIZE_OFFSET + 4].copy_from_slice(&sz.to_le_bytes());
    if !write_image(idx, 1, loader.as_ptr() as usize, loader.len()) {
        log("FAILED: could not write the loader");
        return;
    }
    set_progress(92);

    // 4. The MBR, last: until this lands the disk is not bootable, which is
    //    the right way round for an interrupted install.
    if !write_image(idx, 0, bootimg::MBR.as_ptr() as usize, bootimg::MBR.len()) {
        log("FAILED: could not write the boot sector");
        return;
    }

    set_progress(100);
    DONE.store(true, Ordering::SeqCst);
    log("done. press Reboot and remove the install medium.");
    serial::print_str("[install] completed on drive ");
    serial::print_hex(idx as u64);
    serial::print_str(", kernel ");
    serial::print_hex(size as u64);
    serial::print_str(" bytes\n");
    let _ = d;
}

/// Copy `len` bytes from physical `src` to `lba` on the drive.
fn write_image(idx: usize, lba: u32, src: usize, len: usize) -> bool {
    // 128 sectors per call: within the ATA sector-count register's range and
    // large enough to not be silly about it.
    const CHUNK_SECTORS: usize = 128;
    const CHUNK_BYTES: usize = CHUNK_SECTORS * ata::SECTOR_SIZE;

    let mut done = 0usize;
    while done < len {
        let n = CHUNK_BYTES.min(len - done);
        let sectors = n.div_ceil(ata::SECTOR_SIZE);
        let buf = unsafe {
            core::slice::from_raw_parts((src + done) as *const u8, sectors * ata::SECTOR_SIZE)
        };
        if !ata::write_sectors(idx, lba + (done / ata::SECTOR_SIZE) as u32, sectors as u8, buf) {
            return false;
        }
        done += sectors * ata::SECTOR_SIZE;
    }
    true
}

// ---------------------------------------------------------------------------
//  Rendering
// ---------------------------------------------------------------------------

fn draw_clipped(text: &str, x: u32, y: u32, max_x: u32, r: u8, g: u8, b: u8) {
    let mut cx = x;
    for &ch in text.as_bytes() {
        if cx + font::GLYPH_WIDTH > max_x {
            break;
        }
        font::draw_char(ch, cx, y, r, g, b);
        cx += font::GLYPH_WIDTH;
    }
}

pub fn render() {
    let (wx, wy, w, _h) = geometry();
    let (_br, bg, bb) = window::background(window_id()).unwrap_or((0x1A, 0x1F, 0x35));

    font::draw_str("Install barryOS onto a hard disk", wx + 8, wy + 30, 0x10, 0xB9, 0x81);
    font::draw_str("this overwrites the selected disk", wx + 8, wy + 46, 0x88, 0x90, 0xA0);

    // Disk list.
    let selected = SELECTED.load(Ordering::Relaxed) as usize;
    for i in 0..ata::count().min(MAX_ROWS) {
        let (rx, ry, rw, rh) = row_rect(i);
        if i == selected {
            framebuffer::fill_rect(wx + rx, wy + ry, rw, rh, 0x2E, 0x6E, 0x58);
        }
        let Some(d) = ata::drive(i) else { continue };
        let mut line = [0u8; 72];
        let mut k = 0usize;
        let ch = if d.channel == 0 { b'0' } else { b'1' };
        let sl = if d.slave { b'1' } else { b'0' };
        for b in [b'h', b'd', ch, b'.', sl, b' ', b' '] {
            line[k] = b;
            k += 1;
        }
        for &b in d.model_str().as_bytes() {
            if k < line.len() {
                line[k] = b;
                k += 1;
            }
        }
        for &b in b"  (" {
            if k < line.len() { line[k] = b; k += 1; }
        }
        k += put_dec((d.sectors as u64) / 2048, &mut line[k..]);
        for &b in b" MiB)" {
            if k < line.len() { line[k] = b; k += 1; }
        }
        if let Ok(s) = core::str::from_utf8(&line[..k]) {
            draw_clipped(s, wx + rx + 8, wy + ry + 2, wx + rx + rw - 4, 0xFF, 0xFF, 0xFF);
        }
    }
    if ata::count() == 0 {
        font::draw_str("no ATA disks found", wx + 16, wy + LIST_Y + 2, 0xFF, 0x55, 0x55);
    }

    // Buttons.
    let busy = BUSY.load(Ordering::Relaxed);
    for (i, label) in ["Install", "Reboot", "Quit"].iter().enumerate() {
        let (bx, by, bw, bh) = button_rect(i);
        let (r, g, b) = if i == 0 { (0x1C, 0x7A, 0x58) } else { (0x24, 0x2C, 0x42) };
        framebuffer::fill_rect(wx + bx, wy + by, bw, bh, r, g, b);
        framebuffer::fill_rect(wx + bx, wy + by, bw, 1, 0x10, 0xB9, 0x81);
        let tw = font::text_width(label, 1);
        draw_clipped(label, wx + bx + bw.saturating_sub(tw) / 2, wy + by + 5,
                     wx + bx + bw - 4, 0xFF, 0xFF, 0xFF);
    }
    if busy {
        font::draw_str("working...", wx + 8 + 3 * 138, wy + BTN_Y + 5, 0xE0, 0xB0, 0x40);
    }

    // Progress bar.
    let pct = PROGRESS.load(Ordering::Relaxed);
    framebuffer::fill_rect(wx + 8, wy + PROG_Y, WIN_W - 16, 16, 0x10, 0x14, 0x24);
    let filled = ((WIN_W - 20) as u64 * pct / 100) as u32;
    if filled > 0 {
        framebuffer::fill_rect(wx + 10, wy + PROG_Y + 2, filled, 12, 0x10, 0xB9, 0x81);
    }
    let mut pb = [0u8; 8];
    let n = put_dec(pct, &mut pb);
    if let Ok(s) = core::str::from_utf8(&pb[..n]) {
        draw_clipped(s, wx + WIN_W - 46, wy + PROG_Y + 1, wx + WIN_W - 10, 0xFF, 0xFF, 0xFF);
    }

    // Log, oldest first.
    unsafe {
        let head = LOG_HEAD;
        let count = LOG_LINES.min(if DONE.load(Ordering::Relaxed) { LOG_LINES } else { head + 1 });
        for i in 0..count {
            let idx = (head + LOG_LINES - count + i) % LOG_LINES;
            if LOG_LEN[idx] == 0 {
                continue;
            }
            if let Ok(s) = core::str::from_utf8(&LOG[idx][..LOG_LEN[idx]]) {
                draw_clipped(s, wx + 8, wy + LOG_Y + (i as u32) * 16, wx + WIN_W - 8, 0xC0, 0xC8, 0xD0);
            }
        }
    }

    serial::print_str("[apps] installer: rendered\n");
}

fn put_dec(mut v: u64, out: &mut [u8]) -> usize {
    let mut tmp = [0u8; 20];
    let mut i = tmp.len();
    if v == 0 {
        i -= 1;
        tmp[i] = b'0';
    }
    while v > 0 {
        i -= 1;
        tmp[i] = b'0' + (v % 10) as u8;
        v /= 10;
    }
    let n = (tmp.len() - i).min(out.len());
    out[..n].copy_from_slice(&tmp[i..i + n]);
    n
}
