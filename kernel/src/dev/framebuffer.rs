//! Framebuffer graphics driver.
//!
//! Uses the GOP (Graphics Output Protocol) framebuffer address from
//! BootInfo (UEFI path) or a fallback VESA VBE address (BIOS path).
//! For Stage 6 we use a simple RGB/BGR 32-bit pixel format and draw
//! primitives: pixel, rect, line, and an 8x16 bitmap font for text.

use crate::serial;
use core::sync::atomic::{AtomicU64, AtomicU32, Ordering};

/// Default framebuffer dimensions (QEMU default VBE mode).
/// These are overwritten by BootInfo GOP values on the UEFI path.
static FB_ADDR: AtomicU64 = AtomicU64::new(0);
static FB_WIDTH: AtomicU32 = AtomicU32::new(0);
static FB_HEIGHT: AtomicU32 = AtomicU32::new(0);
static FB_PITCH: AtomicU32 = AtomicU32::new(0);
static FB_BPP: AtomicU32 = AtomicU32::new(32);

/// BootInfo mirror (must match `bootinfo.rs` + `efi_main.c`).
#[repr(C)]
struct BootInfoC {
    magic: u64,
    framebuffer_addr: u64,
    framebuffer_size: u64,
    width: u32,
    height: u32,
    pixels_per_scanline: u32,
    pixel_format: u32,
    memmap: u64,
    memmap_size: u64,
    memmap_desc_size: u64,
    memmap_desc_version: u64,
}

const BARRYOS_BOOTINFO_MAGIC: u64 = 0x534F_5252_4142;

/// Initialize the framebuffer from BootInfo.
/// On BIOS (no BootInfo), use a fallback: 0xE0000000 (QEMU Bochs VBE),
/// 640×480, 32 bpp — this works in QEMU with `-vga std`.
pub fn init(boot_info: usize) {
    if boot_info == 0 {
        // BIOS path — no GOP.  Use QEMU's default VBE framebuffer.
        // In QEMU with -vga std, VBE framebuffer is at 0xE0000000.
        // We pick 640x480x32 as a safe default.
        FB_ADDR.store(0xE000_0000, Ordering::SeqCst);
        FB_WIDTH.store(640, Ordering::SeqCst);
        FB_HEIGHT.store(480, Ordering::SeqCst);
        FB_PITCH.store(640 * 4, Ordering::SeqCst);
        FB_BPP.store(32, Ordering::SeqCst);
        serial::print_str("[fb] BIOS fallback: 640x480x32 @ 0xE0000000\n");
    } else {
        let bi: &BootInfoC = unsafe { &*(boot_info as *const BootInfoC) };
        if bi.magic == BARRYOS_BOOTINFO_MAGIC && bi.framebuffer_addr != 0 {
            FB_ADDR.store(bi.framebuffer_addr, Ordering::SeqCst);
            FB_WIDTH.store(bi.width, Ordering::SeqCst);
            FB_HEIGHT.store(bi.height, Ordering::SeqCst);
            FB_PITCH.store(bi.pixels_per_scanline * 4, Ordering::SeqCst);
            FB_BPP.store(32, Ordering::SeqCst);
            serial::print_str("[fb] GOP: ");
            serial::print_hex(bi.width as u64);
            serial::print_str("x");
            serial::print_hex(bi.height as u64);
            serial::print_str("x32 @ 0x");
            serial::print_hex(bi.framebuffer_addr);
            serial::print_str("\n");
        } else {
            serial::print_str("[fb] BootInfo invalid, using BIOS fallback\n");
            FB_ADDR.store(0xE000_0000, Ordering::SeqCst);
            FB_WIDTH.store(640, Ordering::SeqCst);
            FB_HEIGHT.store(480, Ordering::SeqCst);
            FB_PITCH.store(640 * 4, Ordering::SeqCst);
            FB_BPP.store(32, Ordering::SeqCst);
        }
    }
}

/// Plot a single pixel at (x, y) with RGB color.
pub fn put_pixel(x: u32, y: u32, r: u8, g: u8, b: u8) {
    let addr = FB_ADDR.load(Ordering::Relaxed);
    let w = FB_WIDTH.load(Ordering::Relaxed);
    let h = FB_HEIGHT.load(Ordering::Relaxed);
    let pitch = FB_PITCH.load(Ordering::Relaxed);
    if addr == 0 || x >= w || y >= h {
        return;
    }
    let off = (y as u64) * (pitch as u64) + (x as u64) * 4;
    // Pixel format: assume BGRX (32-bit) — common for GOP/QEMU.
    let pixel: u32 = 0xFF00_0000 | ((r as u32) << 16) | ((g as u32) << 8) | (b as u32);
    unsafe {
        core::ptr::write_volatile((addr + off) as *mut u32, pixel);
    }
}

/// Fill a rectangle with a solid color.
pub fn fill_rect(x: u32, y: u32, w: u32, h: u32, r: u8, g: u8, b: u8) {
    let max_x = FB_WIDTH.load(Ordering::Relaxed);
    let max_y = FB_HEIGHT.load(Ordering::Relaxed);
    let x2 = (x + w).min(max_x);
    let y2 = (y + h).min(max_y);
    let mut yi = y;
    while yi < y2 {
        let mut xi = x;
        while xi < x2 {
            put_pixel(xi, yi, r, g, b);
            xi += 1;
        }
        yi += 1;
    }
}

/// Draw the barryOS boot screen test pattern.
pub fn draw_test_pattern() {
    let w = FB_WIDTH.load(Ordering::Relaxed);
    let h = FB_HEIGHT.load(Ordering::Relaxed);
    serial::print_str("[fb] drawing test pattern ");
    serial::print_hex(w as u64);
    serial::print_str("x");
    serial::print_hex(h as u64);
    serial::print_str("\n");

    // Fill background dark blue.
    fill_rect(0, 0, w, h, 0x10, 0x14, 0x28);

    // Draw colored bars across the top (40px tall).
    let bar_h = 40;
    let bar_w = w / 8;
    fill_rect(0, 0, bar_w, bar_h, 0xFF, 0x00, 0x00);  // red
    fill_rect(bar_w, 0, bar_w, bar_h, 0xFF, 0x80, 0x00);  // orange
    fill_rect(bar_w * 2, 0, bar_w, bar_h, 0xFF, 0xFF, 0x00);  // yellow
    fill_rect(bar_w * 3, 0, bar_w, bar_h, 0x00, 0xFF, 0x00);  // green
    fill_rect(bar_w * 4, 0, bar_w, bar_h, 0x00, 0xFF, 0xFF);  // cyan
    fill_rect(bar_w * 5, 0, bar_w, bar_h, 0x00, 0x80, 0xFF);  // sky
    fill_rect(bar_w * 6, 0, bar_w, bar_h, 0x80, 0x00, 0xFF);  // purple
    fill_rect(bar_w * 7, 0, bar_w, bar_h, 0xFF, 0x00, 0xFF);  // magenta

    // Draw a centered emerald box (logo placeholder).
    let box_w = 200u32.min(w / 2);
    let box_h = 80u32.min(h / 3);
    let bx = (w - box_w) / 2;
    let by = (h - box_h) / 2 + 20;
    // Box border (emerald).
    fill_rect(bx, by, box_w, 4, 0x10, 0xB9, 0x81);
    fill_rect(bx, by + box_h - 4, box_w, 4, 0x10, 0xB9, 0x81);
    fill_rect(bx, by, 4, box_h, 0x10, 0xB9, 0x81);
    fill_rect(bx + box_w - 4, by, 4, box_h, 0x10, 0xB9, 0x81);
    // Box interior (dark).
    fill_rect(bx + 4, by + 4, box_w - 8, box_h - 8, 0x1A, 0x1F, 0x35);

    // Draw pixel grid dots in the lower area.
    let mut y = h / 2 + 60;
    while y < h - 20 {
        let mut x = 20;
        while x < w - 20 {
            put_pixel(x, y, 0x40, 0x60, 0x80);
            x += 20;
        }
        y += 20;
    }

    serial::print_str("[fb] test pattern drawn (8 color bars + emerald box + dot grid)\n");
}

/// Get framebuffer info (for diagnostics).
pub fn info() -> (u64, u32, u32, u32) {
    (
        FB_ADDR.load(Ordering::Relaxed),
        FB_WIDTH.load(Ordering::Relaxed),
        FB_HEIGHT.load(Ordering::Relaxed),
        FB_BPP.load(Ordering::Relaxed),
    )
}
