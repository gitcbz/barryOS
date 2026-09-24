//! The trust store: which certificate authorities this browser believes.
//!
//! A TLS client that does not check the chain is encrypting to whoever
//! answered the connection, which is worse than plaintext because it looks
//! safe.  So this exists, and a chain that does not end here is refused.
//!
//! Each entry is a root's subject name and its public key — not the whole
//! certificate.  The key is what verifying the last signature needs, and the
//! name is what finds it; everything else in a root certificate (validity,
//! extensions, its own signature) is either irrelevant once it is trusted or
//! would cost a third of the kernel's size budget to carry.  What is not
//! stored is deliberately not checked, and `is_trusted` says so.
//!
//! The list is generated from the Mozilla CA bundle by
//! `scripts/gen-truststore.py`, which is also where the pruning rule lives.

use crate::crypto::der::{self, Reader, TAG_OID, TAG_SEQUENCE};
use crate::crypto::x509::{self, Cert};
use crate::serial;

include!(concat!(env!("CARGO_MANIFEST_DIR"), "/../build/truststore.rs"));

/// Is `cert` one of the roots we trust, by name and by key?
pub fn is_trusted(cert: &Cert) -> bool {
    for e in ROOTS {
        if !der::oid_is(&[], &[]) {
            // unreachable; keeps the import used if the table is empty
        }
        if !name_eq(e.subject, cert.subject) {
            continue;
        }
        // The name matched; the key has to match too, or a certificate that
        // merely calls itself "ISRG Root X1" would be trusted.
        if key_matches(e.spki, cert.key_alg, cert.curve, cert.key) {
            serial::print_str("[trust] chain ends at ");
            let mut buf = [0u8; 96];
            let n = x509::common_name(cert, &mut buf);
            serial::print_str(core::str::from_utf8(&buf[..n]).unwrap_or("?"));
            serial::print_str("\n");
            return true;
        }
    }
    false
}

/// Compare the SubjectPublicKeyInfo we stored against the one in the
/// certificate, field by field rather than byte for byte: the stored copy is
/// re-encoded and DER has more than one way to write the same key.
fn key_matches(spki: &[u8], key_alg: &[u8], curve: &[u8], key: &[u8]) -> bool {
    let Some(s) = der::parse_one(spki) else { return false };
    if s.tag != TAG_SEQUENCE {
        return false;
    }
    let mut r = Reader::new(s.body);
    let Some(alg) = r.expect(TAG_SEQUENCE) else { return false };
    let Some(bits) = r.expect(der::TAG_BIT_STRING) else { return false };
    let Some(stored_key) = der::bit_string(&bits) else { return false };

    let mut ar = Reader::new(alg.body);
    let Some(aoid) = ar.expect(TAG_OID) else { return false };
    if !der::oid_is(aoid.body, key_alg) {
        return false;
    }
    // The curve, when there is one.
    match ar.expect(TAG_OID) {
        Some(c) => {
            if !der::oid_is(c.body, curve) {
                return false;
            }
        }
        None => {
            if !curve.is_empty() {
                return false;
            }
        }
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

    let mut buf = [0u8; 2048];
    let n = crate::crypto::unhex(v::CERT_ROOT_A, &mut buf);
    if n == 0 {
        return check("trust store vectors", &[], &[1]);
    }
    let Some(root) = x509::parse(&buf[..n]) else {
        return check("trust store parses a root", &[], &[1]);
    };

    let mut f = 0usize;
    // The chain used for the rest of the tests ends at this root, so it has to
    // be in the store or nothing above this can verify.
    f += check("trust store knows the test root", &[is_trusted(&root) as u8], &[1]);

    // A leaf is not a root, and must not be accepted as one just because it
    // parsed.
    let mut lb = [0u8; 2048];
    let ln = crate::crypto::unhex(v::CERT_LEAF, &mut lb);
    if ln != 0 {
        if let Some(leaf) = x509::parse(&lb[..ln]) {
            f += check("trust store rejects a leaf", &[is_trusted(&leaf) as u8], &[0]);
        }
    }

    f += check("trust store is not empty", &[(count() > 0) as u8], &[1]);
    f
}
