//! The trust store: which certificate authorities this browser believes.
//!
//! A TLS client that does not check the chain is encrypting to whoever
//! answered the connection, which is worse than plaintext because it looks
//! safe.  So this exists, and a chain that does not end here is refused.
//!
//! Each entry is a root's subject name and its public key — not the whole
//! certificate.  The key is what verifying a signature needs, and the name is
//! what finds it; everything else in a root certificate (validity,
//! extensions, its own signature) is either irrelevant once the root is
//! trusted or would cost a third of the kernel's size budget to carry.  What
//! is not stored is deliberately not checked.
//!
//! Two ways a chain can arrive at an entry here, and both are needed:
//!
//!   * the server sent the root itself, and it is byte for byte the one we
//!     have — or a cross-signed twin of it, which has the same subject and the
//!     same key and is therefore the same trust decision;
//!   * the server stopped at an intermediate, as most do, and the entry is
//!     what the intermediate's signature has to verify against.
//!
//! The list is generated from the Mozilla CA bundle by
//! `scripts/gen-truststore.py`, which is also where the pruning rule lives.

use crate::crypto::der::{self, Reader, TAG_OID, TAG_SEQUENCE};
use crate::crypto::x509::{self, Cert};
use crate::serial;

// Relative rather than through CARGO_MANIFEST_DIR: the same source is compiled
// by the kernel build and by tests/tls-host.rs, which is plain rustc and has no
// such variable.  `include!` resolves against the file it appears in, which is
// the one path that means the same thing in both builds.
include!("../../../build/truststore.rs");

/// Is `cert` one of the roots we trust, by name and by key?
pub fn is_trusted(cert: &Cert) -> bool {
    for e in ROOTS {
        if !name_eq(e.subject, cert.subject) {
            continue;
        }
        // The name matched; the key has to match too, or a certificate that
        // merely calls itself "ISRG Root X1" would be trusted.
        if key_matches(e.spki, cert.key_alg, cert.curve, cert.key) {
            log_match("chain ends at ", cert);
            return true;
        }
    }
    false
}

/// Does `cert` carry a signature made by a root in the store?
///
/// Servers routinely send a chain that stops at an intermediate and leave the
/// root out, on the grounds that the client is expected to have it.  The root
/// is the one certificate in the chain that cannot be checked by looking at
/// the chain, so this is where having it pays off.  It also answers for a
/// chain of one, which is what a site hosted directly on a root's key looks
/// like.
pub fn signed_by_stored_root(cert: &Cert) -> bool {
    for e in ROOTS {
        if !name_eq(e.subject, cert.issuer) {
            continue;
        }
        let Some((alg, curve, key)) = spki_parts(e.spki) else { continue };
        // The name goes in too, so a log line can say which root it was.
        // `verify_signed_by` reads only the key.
        let issuer = Cert {
            subject: e.subject,
            key_alg: alg,
            curve,
            key,
            is_ca: true,
            ..Cert::EMPTY
        };
        if x509::verify_signed_by(cert, &issuer) {
            log_match("chain is signed by ", &issuer);
            return true;
        }
    }
    false
}

fn log_match(prefix: &str, cert: &Cert) {
    serial::print_str("[trust] ");
    serial::print_str(prefix);
    let mut buf = [0u8; 96];
    let n = x509::common_name(cert, &mut buf);
    serial::print_str(core::str::from_utf8(&buf[..n]).unwrap_or("?"));
    serial::print_str("\n");
}

/// The three fields of a SubjectPublicKeyInfo that an issuer has to supply.
fn spki_parts(spki: &[u8]) -> Option<(&[u8], &[u8], &[u8])> {
    let s = der::parse_one(spki)?;
    if s.tag != TAG_SEQUENCE {
        return None;
    }
    let mut r = Reader::new(s.body);
    let alg = r.expect(TAG_SEQUENCE)?;
    let bits = r.expect(der::TAG_BIT_STRING)?;
    let key = der::bit_string(&bits)?;

    let mut ar = Reader::new(alg.body);
    let key_alg = ar.expect(TAG_OID)?.body;
    let curve: &[u8] = match ar.expect(TAG_OID) {
        Some(c) => c.body,
        None => &[],
    };
    Some((key_alg, curve, key))
}

/// Compare the SubjectPublicKeyInfo we stored against the one in the
/// certificate, field by field rather than byte for byte: the stored copy is
/// re-encoded and DER has more than one way to write the same key.
fn key_matches(spki: &[u8], key_alg: &[u8], curve: &[u8], key: &[u8]) -> bool {
    let Some((stored_alg, stored_curve, stored_key)) = spki_parts(spki) else {
        return false;
    };
    if !der::oid_is(stored_alg, key_alg) {
        return false;
    }
    if !der::oid_is(stored_curve, curve) {
        return false;
    }

    // For RSA the two INTEGERs inside the SEQUENCE are compared; for EC the
    // point is.
    if der::oid_is(key_alg, x509::OID_RSA) {
        let (Some(a), Some(b)) = (der::parse_one(stored_key), der::parse_one(key)) else {
            return false;
        };
        let mut ak = Reader::new(a.body);
        let mut bk = Reader::new(b.body);
        let (Some(an), Some(ae)) = (ak.expect(der::TAG_INTEGER), ak.expect(der::TAG_INTEGER))
            else { return false };
        let (Some(bn), Some(be)) = (bk.expect(der::TAG_INTEGER), bk.expect(der::TAG_INTEGER))
            else { return false };
        // Leading zeros are stripped by int_bytes, so the magnitudes compare.
        der::int_bytes(&an) == der::int_bytes(&bn) && der::int_bytes(&ae) == der::int_bytes(&be)
    } else {
        stored_key == key
    }
}

/// Compare two Names as DER, without the outer SEQUENCE header, which can be
/// encoded in more than one length form for the same name.
fn name_eq(a: &[u8], b: &[u8]) -> bool {
    let (Some(x), Some(y)) = (der::parse_one(a), der::parse_one(b)) else {
        return false;
    };
    x.tag == TAG_SEQUENCE && y.tag == TAG_SEQUENCE && x.body == y.body
}

/// How many roots are compiled in.
pub fn count() -> usize {
    ROOTS.len()
}

pub fn selftest() -> usize {
    use crate::crypto::check;
    use crate::crypto::x509_vectors as v;

    // The vectors carry a real Cloudflare chain for example.com:
    //
    //   leaf  CN=example.com
    //    ^    Cloudflare TLS Issuing ECC CA 3
    //     ^   SSL.com TLS Transit ECC CA R2          (CERT_ROOT_A)
    //      ^  SSL.com TLS ECC Root CA 2022           (the one in the store)
    //
    // Only the last is a trust anchor.  A and B are both named ROOT_ in the
    // vectors because they were the two ends of the chain when it was captured;
    // what matters is who issued what.
    let mut bufs: [([u8; 2048], usize); 3] = [([0u8; 2048], 0); 3];
    for (i, hexs) in [v::CERT_LEAF, v::CERT_INTERMEDIATE, v::CERT_ROOT_A]
        .into_iter()
        .enumerate()
    {
        bufs[i].1 = crate::crypto::unhex(hexs, &mut bufs[i].0);
    }
    if bufs.iter().any(|(_, n)| *n == 0) {
        return check("trust store vectors decode", &[], &[1]);
    }
    let (Some(leaf), Some(inter), Some(transit)) = (
        x509::parse(&bufs[0].0[..bufs[0].1]),
        x509::parse(&bufs[1].0[..bufs[1].1]),
        x509::parse(&bufs[2].0[..bufs[2].1]),
    ) else {
        return check("trust store parses a vector certificate", &[], &[1]);
    };

    let mut f = 0usize;

    // A certificate issued by a root that is in the store, where the root
    // itself was never sent — the shape nearly every server produces.
    f += check("trust store verifies a certificate the root issued",
               &[signed_by_stored_root(&transit) as u8], &[1]);

    // The two that must not pass, or this would say yes to anything.
    f += check("trust store rejects a leaf signed by an intermediate",
               &[signed_by_stored_root(&leaf) as u8], &[0]);
    f += check("trust store rejects an intermediate as a root",
               &[is_trusted(&inter) as u8], &[0]);
    f += check("trust store rejects a transit CA as a root",
               &[is_trusted(&transit) as u8], &[0]);

    // The cross-signed shape: a certificate that calls itself the root we
    // hold, and carries the same key, but was issued by someone else.  Same
    // subject and same key is the same trust decision, so this has to pass —
    // it is how GlobalSign and DigiCert serve their chains.
    let mut cb = [0u8; 2048];
    let cn = crate::crypto::unhex(v::CERT_ROOT_B, &mut cb);
    if cn != 0 {
        if let Some(cross) = x509::parse(&cb[..cn]) {
            f += check("trust store accepts a cross-signed twin of a root",
                       &[is_trusted(&cross) as u8], &[1]);
        }
    }

    f += check("trust store is not empty", &[(count() > 0) as u8], &[1]);
    f
}
