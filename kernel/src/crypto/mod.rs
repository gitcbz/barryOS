//! Cryptography for TLS.
//!
//! Everything here is checked against published test vectors rather than
//! against itself.  A cipher or a hash that is subtly wrong still round-trips
//! its own output, so a round-trip test proves nothing; FIPS 197, the NIST
//! GCM vectors and RFC 7748 all publish inputs with known answers, and
//! `selftest` checks those.  It runs at boot and prints one line per family,
//! like the rest of this kernel's self-tests.
//!
//! This is not a general-purpose crypto library.  It implements exactly what
//! the TLS client below uses and nothing else — no random number generation
//! beyond what a key share needs, no key storage, no streaming AEAD.

pub mod aes;
pub mod bignum;
pub mod der;
pub mod ec;
pub mod ec_vectors;
pub mod hkdf;
pub mod hmac;
pub mod rsa;
pub mod sha512;
pub mod tls;
pub mod x25519;
pub mod vectors;
pub mod trust;
pub mod x509;
pub mod x509_vectors;

use crate::serial;
use core::sync::atomic::{AtomicBool, Ordering};

pub static INITIALIZED: AtomicBool = AtomicBool::new(false);
/// Set when every vector passed.  The browser checks it before offering to
/// speak TLS: better to refuse than to encrypt with something unverified.
pub static SOUND: AtomicBool = AtomicBool::new(false);

/// Decode a hex string into `out`; returns the number of bytes written, or 0
/// if the input is not a whole number of bytes.
///
/// The refusal matters.  Reading two characters at a time silently drops a
/// trailing odd digit, so `b"10001"` — RSA's 65537, five characters — becomes
/// 0x1000, and a perfectly correct signature fails to verify with nothing to
/// say why.  That happened here twice, so it is now an error.
pub fn unhex(s: &[u8], out: &mut [u8]) -> usize {
    let digits = s.iter().filter(|c| c.is_ascii_hexdigit()).count();
    if digits % 2 != 0 {
        return 0;
    }
    let mut n = 0usize;
    let mut hi: Option<u8> = None;
    for &c in s {
        let v = match c {
            b'0'..=b'9' => c - b'0',
            b'a'..=b'f' => c - b'a' + 10,
            b'A'..=b'F' => c - b'A' + 10,
            _ => continue,
        };
        match hi {
            None => hi = Some(v),
            Some(h) => {
                if n < out.len() {
                    out[n] = (h << 4) | v;
                    n += 1;
                }
                hi = None;
            }
        }
    }
    n
}

/// Decode a hex string into a local array of exactly 64 bytes.
pub fn unhex64(s: &[u8]) -> [u8; 64] {
    let mut b = [0u8; 64];
    unhex(s, &mut b);
    b
}

pub fn init() {
    serial::print_str("[crypto] self-test\n");
    let mut failures = 0usize;

    failures += aes::selftest();
    failures += sha512::selftest();
    failures += hmac::selftest();
    failures += hkdf::selftest();
    failures += x25519::selftest();
    failures += bignum::selftest();
    failures += rsa::selftest();
    failures += ec::selftest();
    failures += der::selftest();
    failures += x509::selftest();
    failures += trust::selftest();

    if failures == 0 {
        SOUND.store(true, Ordering::Release);
        serial::print_str("[crypto] all vectors pass\n");
    } else {
        serial::print_str("[crypto] ");
        serial::print_dec(failures as u64);
        serial::print_str(" vector(s) FAILED — TLS will be refused\n");
    }
    INITIALIZED.store(true, Ordering::Release);
}

/// Report one vector.
pub fn check(name: &str, got: &[u8], want: &[u8]) -> usize {
    if got == want {
        serial::print_str("[crypto]   ok   ");
        serial::print_str(name);
        serial::print_str("\n");
        0
    } else {
        serial::print_str("[crypto]   FAIL ");
        serial::print_str(name);
        serial::print_str("\n[crypto]     got  ");
        hex(got);
        serial::print_str("\n[crypto]     want ");
        hex(want);
        serial::print_str("\n");
        1
    }
}

fn hex(b: &[u8]) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for &x in b.iter().take(32) {
        let pair = [HEX[(x >> 4) as usize], HEX[(x & 0xF) as usize]];
        serial::print_str(core::str::from_utf8(&pair).unwrap_or("??"));
    }
}
