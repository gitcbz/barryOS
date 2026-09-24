//! HKDF (RFC 5869), and the TLS 1.3 label wrapping (RFC 8446 §7.1).
//!
//! TLS 1.3 derives every key from the shared secret through this: extract,
//! then expand with a label that names what the key is for.  The label exists
//! so that two different uses of the same secret cannot produce the same key.

use crate::crypto::hmac::{hmac, hmac_parts};

/// HKDF-Extract: the shared secret salted into a pseudorandom key.
pub fn extract(salt: &[u8], ikm: &[u8]) -> [u8; 32] {
    // An absent salt is a string of zeroes the length of the hash.
    if salt.is_empty() {
        hmac(&[0u8; 32], ikm)
    } else {
        hmac(salt, ikm)
    }
}

/// HKDF-Expand into `out`.
pub fn expand(prk: &[u8], info: &[u8], out: &mut [u8]) {
    let mut t: [u8; 32] = [0u8; 32];
    let mut t_len = 0usize;             // 0 means "no previous block"
    let mut written = 0usize;
    let mut counter = 1u8;

    while written < out.len() {
        // T(n) = HMAC(prk, T(n-1) || info || n), with T(0) empty.
        let n = [counter];
        let block = if t_len == 0 {
            hmac_parts(prk, &[info, &n])
        } else {
            hmac_parts(prk, &[&t, info, &n])
        };
        t = block;
        t_len = 32;
        let take = (out.len() - written).min(32);
        out[written..written + take].copy_from_slice(&t[..take]);
        written += take;
        counter = counter.wrapping_add(1);
        if counter == 0 {
            break;                      // 255 blocks is the RFC's limit
        }
    }
}

/// TLS 1.3's HKDF-Expand-Label.
///
/// The `info` is a struct rather than a byte string, and getting its shape
/// wrong is a silent failure: both ends derive a key, neither matches, and all
/// that shows is a decryption error later.
pub fn expand_label(secret: &[u8], label: &[u8], context: &[u8], out: &mut [u8]) {
    //  HkdfLabel ::= struct {
    //      uint16 length;
    //      opaque label<7..255> = "tls13 " + Label;
    //      opaque context<0..255>;
    //  }
    let mut info = [0u8; 2 + 1 + 6 + 64 + 1 + 255];
    let mut n = 0usize;
    info[n..n + 2].copy_from_slice(&(out.len() as u16).to_be_bytes());
    n += 2;
    let full = 6 + label.len();         // "tls13 " prefix
    info[n] = full as u8;
    n += 1;
    info[n..n + 6].copy_from_slice(b"tls13 ");
    n += 6;
    let l = label.len().min(255 - 6);
    info[n..n + l].copy_from_slice(&label[..l]);
    n += l;
    let c = context.len().min(255);
    info[n] = c as u8;
    n += 1;
    info[n..n + c].copy_from_slice(&context[..c]);
    n += c;

    expand(secret, &info[..n], out);
}

/// The key schedule's two "derived" labels, which take an empty context.
pub fn derive_secret(secret: &[u8], label: &[u8], transcript: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    expand_label(secret, label, transcript, &mut out);
    out
}

pub fn selftest() -> usize {
    use crate::crypto::{check, unhex};
    let mut f = 0usize;

    // RFC 5869 test case 1.
    {
        let mut ikm = [0u8; 22];
        unhex(b"0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b", &mut ikm);
        let mut salt = [0u8; 13];
        unhex(b"000102030405060708090a0b0c", &mut salt);
        let mut info = [0u8; 10];
        unhex(b"f0f1f2f3f4f5f6f7f8f9", &mut info);

        let prk = extract(&salt, &ikm);
        let mut want_prk = [0u8; 32];
        unhex(b"077709362c2e32df0ddc3f0dc47bba6390b6c73bb50f9c3122ec844ad7c2b3e5", &mut want_prk);
        f += check("hkdf rfc5869 prk", &prk, &want_prk);

        let mut okm = [0u8; 42];
        expand(&prk, &info, &mut okm);
        let mut want_okm = [0u8; 42];
        unhex(b"3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf34007208d5b887185865", &mut want_okm);
        f += check("hkdf rfc5869 okm", &okm, &want_okm);
    }

    f
}
