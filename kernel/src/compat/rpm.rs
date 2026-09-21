//! .rpm package parser (RPM v3 format).
//!
//! RPM file format:
//!   - Lead (96 bytes): magic 0xED 0xAB 0xEE 0xDB + name + version.
//!   - Signature header (variable).
//!   - Header (variable): tag-index + data.
//!   - Payload (cpio.gz).
//!
//! For Stage 9 we parse the lead + detect the signature header size.

use crate::serial;

/// RPM magic bytes.
const RPM_MAGIC: [u8; 4] = [0xED, 0xAB, 0xEE, 0xDB];

/// Parsed RPM package info.
#[derive(Clone, Copy)]
pub struct RpmPackage {
    pub name: [u8; 66],
    pub name_len: usize,
    pub major: u16,
    pub minor: u16,
    pub type_: u16,
    pub archnum: u16,
    pub valid: bool,
}

impl RpmPackage {
    pub const fn empty() -> Self {
        Self {
            name: [0; 66], name_len: 0,
            major: 0, minor: 0, type_: 0, archnum: 0,
            valid: false,
        }
    }
}

pub fn init() {
    serial::print_str("[rpm] .rpm parser initialized (RPM v3 format)\n");
}

/// Parse a .rpm file from a byte slice.
pub fn parse(data: &[u8]) -> RpmPackage {
    let mut pkg = RpmPackage::empty();

    if data.len() < 96 {
        serial::print_str("[rpm] too short (< 96 bytes)\n");
        return pkg;
    }

    // Check magic — byte-by-byte.
    if data[0] != RPM_MAGIC[0] || data[1] != RPM_MAGIC[1]
        || data[2] != RPM_MAGIC[2] || data[3] != RPM_MAGIC[3] {
        serial::print_str("[rpm] not an RPM (no magic)\n");
        return pkg;
    }

    // Major + minor version (bytes 4-5).
    pkg.major = data[4] as u16;
    pkg.minor = data[5] as u16;
    serial::print_str("[rpm] magic OK, reading version...\n");

    // Type (bytes 6-7, big-endian u16).
    pkg.type_ = ((data[6] as u16) << 8) | (data[7] as u16);

    // Archnum (bytes 8-9, big-endian u16).
    pkg.archnum = ((data[8] as u16) << 8) | (data[9] as u16);
    serial::print_str("[rpm] version + arch OK, reading name...\n");

    // Name (bytes 10..76, null-terminated, 66 bytes).
    // Read byte-by-byte to avoid slice creation (which triggers #UD).
    let mut nlen = 66;
    for i in 0..66 {
        let b = data[10 + i];
        if b == 0 {
            nlen = i;
            break;
        }
        pkg.name[i] = b;
    }
    pkg.name_len = nlen;
    serial::print_str("[rpm] name read OK\n");

    pkg.valid = true;

    serial::print_str("[rpm] lead: name=\"");
    for i in 0..pkg.name_len {
        serial::write_char(pkg.name[i]);
    }
    serial::print_str("\" v");
    serial::print_hex(pkg.major as u64);
    serial::print_str(".");
    serial::print_hex(pkg.minor as u64);
    serial::print_str(" type=");
    serial::print_hex(pkg.type_ as u64);
    serial::print_str(" arch=");
    serial::print_hex(pkg.archnum as u64);
    serial::print_str("\n");

    pkg
}

/// Test parsing with a minimal in-memory .rpm.
pub fn test_parse() {
    serial::print_str("[rpm] test: parsing minimal .rpm\n");

    static mut TEST_RPM: [u8; 128] = [0; 128];
    let p = unsafe { core::ptr::addr_of_mut!(TEST_RPM) as *mut u8 };

    unsafe {
        // Magic.
        core::ptr::write_volatile(p.add(0), 0xED);
        core::ptr::write_volatile(p.add(1), 0xAB);
        core::ptr::write_volatile(p.add(2), 0xEE);
        core::ptr::write_volatile(p.add(3), 0xDB);
        // Major 3, minor 0.
        core::ptr::write_volatile(p.add(4), 3);
        core::ptr::write_volatile(p.add(5), 0);
        // Type = 0 (binary), big-endian.
        core::ptr::write_volatile(p.add(6), 0);
        core::ptr::write_volatile(p.add(7), 0);
        // Archnum = 1 (i386), big-endian.
        core::ptr::write_volatile(p.add(8), 0);
        core::ptr::write_volatile(p.add(9), 1);
        // Name: "test-pkg" (8 chars + null).
        let name = b"test-pkg";
        for (i, &b) in name.iter().enumerate() {
            core::ptr::write_volatile(p.add(10 + i), b);
        }
        core::ptr::write_volatile(p.add(10 + name.len()), 0);
    }

    let pkg = parse(unsafe { &TEST_RPM[..128] });
    if pkg.valid {
        serial::print_str("[rpm] test: OK (lead parsed)\n");
        super::PACKAGES_PARSED.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    } else {
        serial::print_str("[rpm] test: FAIL\n");
    }
}
