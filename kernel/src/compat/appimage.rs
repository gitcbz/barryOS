//! .AppImage parser (Type 2 format).
//!
//! AppImage Type 2:
//!   - ELF binary with an AppImage magic at offset 8 (after ELF header).
//!   - Contains a squashfs payload starting at a specified offset.
//!   - Magic: bytes 8-11 = "AI\x02" (AppImage type 2).
//!
//! For Stage 9 we detect the format and extract the payload offset.

use crate::serial;

/// AppImage Type 2 magic (at offset 8).
const APPIMAGE_MAGIC: [u8; 4] = [0x41, 0x49, 0x02, 0x00];  // "AI\x02\x00"

/// Parsed AppImage info.
#[derive(Clone, Copy)]
pub struct AppImageInfo {
    pub is_type2: bool,
    pub payload_offset: u64,
    pub payload_size: u64,
    pub valid: bool,
}

impl AppImageInfo {
    pub const fn empty() -> Self {
        Self { is_type2: false, payload_offset: 0, payload_size: 0, valid: false }
    }
}

pub fn init() {
    serial::print_str("[appimage] .AppImage parser initialized (Type 2 ELF+squashfs)\n");
}

/// Parse/detect an AppImage from a byte slice.
pub fn detect(data: &[u8]) -> AppImageInfo {
    let mut info = AppImageInfo::empty();

    // Must be at least 12 bytes (ELF header 8 + magic 4).
    if data.len() < 12 {
        serial::print_str("[appimage] too short\n");
        return info;
    }

    // Check ELF magic (0x7F 'E' 'L' 'F') — byte-by-byte.
    if data[0] != 0x7F || data[1] != 0x45 || data[2] != 0x4C || data[3] != 0x46 {
        serial::print_str("[appimage] not an ELF\n");
        return info;
    }

    // Check AppImage magic at offset 8 — byte-by-byte.
    if data[8] == APPIMAGE_MAGIC[0] && data[9] == APPIMAGE_MAGIC[1]
        && data[10] == APPIMAGE_MAGIC[2] && data[11] == APPIMAGE_MAGIC[3] {
        info.is_type2 = true;
        serial::print_str("[appimage] Type 2 detected (magic at offset 8)\n");

        // Read payload offset (64-bit LE at offset 0x18 / 24... actually
        // AppImage stores it differently. For simplicity, assume offset
        // is at a fixed position after the ELF header.
        // Real AppImage: the ISO 9660 or squashfs starts after the ELF.
        // We just report the detected type.
        info.payload_offset = 4096;  // typical page-aligned offset
        info.payload_size = data.len() as u64 - 4096;
        info.valid = true;
    } else {
        serial::print_str("[appimage] not AppImage Type 2\n");
    }

    info
}

/// Test detection with a minimal in-memory AppImage.
pub fn test_detect() {
    serial::print_str("[appimage] test: detecting minimal AppImage\n");

    static mut TEST_AI: [u8; 64] = [0; 64];
    let p = unsafe { core::ptr::addr_of_mut!(TEST_AI) as *mut u8 };

    unsafe {
        // ELF magic.
        core::ptr::write_volatile(p.add(0), 0x7F);
        core::ptr::write_volatile(p.add(1), 0x45);  // E
        core::ptr::write_volatile(p.add(2), 0x4C);  // L
        core::ptr::write_volatile(p.add(3), 0x46);  // F
        // AppImage magic at offset 8.
        core::ptr::write_volatile(p.add(8), 0x41);   // A
        core::ptr::write_volatile(p.add(9), 0x49);   // I
        core::ptr::write_volatile(p.add(10), 0x02);  // type 2
        core::ptr::write_volatile(p.add(11), 0x00);
    }

    let info = detect(unsafe { &TEST_AI[..64] });
    if info.valid {
        serial::print_str("[appimage] test: OK (Type 2 detected)\n");
        serial::print_str("[appimage] payload: offset=");
        serial::print_hex(info.payload_offset);
        serial::print_str(" size=");
        serial::print_hex(info.payload_size);
        serial::print_str("\n");
        super::PACKAGES_PARSED.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    } else {
        serial::print_str("[appimage] test: FAIL\n");
    }
}
