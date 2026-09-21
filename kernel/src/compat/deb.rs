//! .deb package parser (ar archive format).
//!
//! A .deb file is an `ar` archive containing:
//!   - debian-binary    (version string "2.0\n")
//!   - control.tar.gz   (package metadata: control, md5sums, etc.)
//!   - data.tar.gz      (actual package files)
//!
//! Format: `!<arch>\n` magic (8 bytes) + file entries (60-byte header + data).

use crate::serial;
use core::sync::atomic::AtomicU64;

/// AR archive magic.
const AR_MAGIC: &[u8] = b"!<arch>\n";

/// Parsed deb package info.
#[derive(Clone, Copy)]
pub struct DebPackage {
    pub name: [u8; 32],
    pub name_len: usize,
    pub version: [u8; 16],
    pub version_len: usize,
    pub arch: [u8; 8],
    pub arch_len: usize,
    pub control_size: u64,
    pub data_size: u64,
    pub valid: bool,
}

impl DebPackage {
    pub const fn empty() -> Self {
        Self {
            name: [0; 32], name_len: 0,
            version: [0; 16], version_len: 0,
            arch: [0; 8], arch_len: 0,
            control_size: 0, data_size: 0,
            valid: false,
        }
    }
}

/// Byte-by-byte comparison (avoid memcmp which causes #UD).
fn bytes_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    for i in 0..a.len() {
        if a[i] != b[i] {
            return false;
        }
    }
    true
}

pub fn init() {
    serial::print_str("[deb] .deb parser initialized (ar archive format)\n");
}

/// Parse a .deb file from a byte slice.
pub fn parse(data: &[u8]) -> DebPackage {
    let mut pkg = DebPackage::empty();

    // Check AR magic.
    if data.len() < 8 || &data[0..8] != AR_MAGIC {
        serial::print_str("[deb] not an ar archive (no magic)\n");
        return pkg;
    }

    // Iterate ar entries (each: 60-byte header + data, padded to 2 bytes).
    let mut offset = 8usize;
    let mut entries = 0;
    while offset + 60 <= data.len() {
        let header = &data[offset..offset + 60];

        // File name (16 bytes, space-padded).
        let name_raw = &header[0..16];
        // Find name end (space or null) — byte-by-byte (avoid memcmp).
        let mut name_len = 16;
        for i in 0..16 {
            let b = name_raw[i];
            if b == b' ' || b == b'/' || b == 0 {
                name_len = i;
                break;
            }
        }
        let name = &name_raw[..name_len];

        // File size (10 bytes decimal, starting at offset 48).
        let size_str = &header[48..58];
        let mut size: u64 = 0;
        for &b in size_str {
            if b >= b'0' && b <= b'9' {
                size = size * 10 + (b - b'0') as u64;
            }
        }

        serial::print_str("[deb] entry: \"");
        for &b in name { serial::write_char(b); }
        serial::print_str("\" size=");
        serial::print_hex(size);
        serial::print_str("\n");

        // Store entry sizes (byte-by-byte comparison, avoid memcmp).
        if bytes_eq(name, b"control.tar") || bytes_eq(name, b"control.tar.gz") {
            pkg.control_size = size;
        } else if bytes_eq(name, b"data.tar") || bytes_eq(name, b"data.tar.gz") {
            pkg.data_size = size;
        } else if bytes_eq(name, b"debian-binary") {
            // Read version (should be "2.0\n").
            let vstart = offset + 60;
            let vend = (vstart + size as usize).min(data.len());
            let vdata = &data[vstart..vend];
            // Find newline — byte-by-byte.
            let mut vlen = vdata.len().min(15);
            for i in 0..vdata.len() {
                if vdata[i] == b'\n' || vdata[i] == 0 {
                    vlen = i.min(15);
                    break;
                }
            }
            // Copy version bytes one by one (avoid memcpy).
            for i in 0..vlen {
                pkg.version[i] = vdata[i];
            }
            pkg.version_len = vlen;
        }

        entries += 1;
        // Advance to next entry (header + data, padded to 2 bytes).
        let entry_size = 60 + size as usize;
        let padded = (entry_size + 1) & !1;  // round up to even
        offset += padded;
    }

    pkg.valid = entries >= 3;
    pkg
}

/// Test parsing with a minimal in-memory .deb.
pub fn test_parse() {
    serial::print_str("[deb] test: parsing minimal .deb\n");

    // Build a minimal .deb in a static buffer.
    // ar magic + debian-binary entry + control.tar entry + data.tar entry.
    static mut TEST_DEB: [u8; 256] = [0; 256];
    let p = unsafe { core::ptr::addr_of_mut!(TEST_DEB) as *mut u8 };

    unsafe {
        // AR magic.
        for (i, &b) in AR_MAGIC.iter().enumerate() {
            core::ptr::write_volatile(p.add(i), b);
        }
        let mut off = 8;

        // Entry 1: debian-binary (60-byte header + 4 bytes data "2.0\n").
        let header = b"debian-binary    0           0     0     100644  4         `\n";
        for (i, &b) in header.iter().enumerate() {
            core::ptr::write_volatile(p.add(off + i), b);
        }
        off += 60;
        let data1 = b"2.0\n";
        for (i, &b) in data1.iter().enumerate() {
            core::ptr::write_volatile(p.add(off + i), b);
        }
        off += 4;

        // Entry 2: control.tar (header + 128 bytes placeholder).
        let header2 = b"control.tar      0           0     0     100644  128       `\n";
        for (i, &b) in header2.iter().enumerate() {
            core::ptr::write_volatile(p.add(off + i), b);
        }
        off += 60;
        // control data (just zeros for test).
        for i in 0..128 {
            core::ptr::write_volatile(p.add(off + i), 0);
        }
        off += 128;

        // Entry 3: data.tar (header + 64 bytes placeholder).
        let header3 = b"data.tar         0           0     0     100644  64        `\n";
        for (i, &b) in header3.iter().enumerate() {
            core::ptr::write_volatile(p.add(off + i), b);
        }
        off += 60;
        for i in 0..64 {
            core::ptr::write_volatile(p.add(off + i), 0);
        }
        off += 64;
    }

    let pkg = parse(unsafe { &TEST_DEB[..256] });
    if pkg.valid {
        serial::print_str("[deb] test: OK (3 entries parsed)\n");
        serial::print_str("[deb] version: \"");
        for i in 0..pkg.version_len {
            serial::write_char(pkg.version[i]);
        }
        serial::print_str("\"\n");
        serial::print_str("[deb] control: ");
        serial::print_hex(pkg.control_size);
        serial::print_str(" bytes, data: ");
        serial::print_hex(pkg.data_size);
        serial::print_str(" bytes\n");
        super::PACKAGES_PARSED.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    } else {
        serial::print_str("[deb] test: FAIL\n");
    }
}
