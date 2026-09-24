//! HMAC-SHA256 (RFC 2104).
//!
//! The MAC under TLS 1.2's PRF and TLS 1.3's whole key schedule.  SHA-256 and
//! HMAC both have published vectors, and `selftest` checks them; an HMAC with
//! the pads swapped still round-trips its own output.

use crate::sha256::Sha256;

const BLOCK: usize = 64;

/// HMAC-SHA256 over a sequence of chunks.
///
/// The chunked form exists because TLS 1.3's transcript is a running hash fed
/// in as one piece per key derivation, and concatenating it each time would
/// copy the whole handshake over and over.
pub fn hmac_parts(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    let mut k = [0u8; BLOCK];
    if key.len() > BLOCK {
        // A key longer than the block is hashed down to one first.
        k[..32].copy_from_slice(&crate::sha256::hash(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }

    let mut ipad = [0x36u8; BLOCK];
    let mut opad = [0x5cu8; BLOCK];
    for i in 0..BLOCK {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }

    // H(K ^ ipad || chunks...)
    let mut inner = Sha256::new();
    inner.update(&ipad);
    for p in parts {
        inner.update(p);
    }
    let ih = inner.finish();

    // H(K ^ opad || H(K ^ ipad || chunks...))
    let mut outer = Sha256::new();
    outer.update(&opad);
    outer.update(&ih);
    outer.finish()
}

/// HMAC-SHA256 of `data` under `key`.
pub fn hmac(key: &[u8], data: &[u8]) -> [u8; 32] {
    hmac_parts(key, &[data])
}

pub fn selftest() -> usize {
    use crate::crypto::{check, unhex};
    let mut f = 0usize;

    // RFC 4231 test case 1: 20 bytes of 0x0b, "Hi There".
    {
        let key = [0x0bu8; 20];
        let mut want = [0u8; 32];
        unhex(b"b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7", &mut want);
        f += check("hmac rfc4231-1", &hmac(&key, b"Hi There"), &want);
    }

    // RFC 4231 test case 3: the key is longer than the block, so it is hashed
    // down first — the one branch a short-key test would never reach.
    {
        let key = [0xaau8; 131];
        let data = b"Test Using Larger Than Block-Size Key - Hash Key First";
        let mut want = [0u8; 32];
        unhex(b"60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54", &mut want);
        f += check("hmac rfc4231-3", &hmac(&key, data), &want);
    }

    f
}
