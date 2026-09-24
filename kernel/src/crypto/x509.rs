//! X.509 certificates: parse one, check who signed it, check the name in it.
//!
//! Deliberately not a general certificate library.  What it does is what
//! validating a chain needs: read the fields, say which algorithm signed the
//! certificate, and hand back the public key and the names inside it.  It does
//! not check policy, key usage beyond the CA flag, or anything about
//! revocation — an OCSP or CRL fetch is a network round trip and a cache, and
//! neither exists here.
//!
//! Everything is a borrow into the caller's buffer.  A certificate arrives
//! over the network from whoever answers the connection, so no owned copies
//! and no allocation means nothing to leak and nothing to free.

use crate::crypto::der::{self, Reader, Tlv, TAG_BOOLEAN, TAG_INTEGER, TAG_OCTET_STRING,
                           TAG_OID, TAG_SEQUENCE, TAG_BIT_STRING};
use crate::crypto::ec::{self, Ecdsa};
use crate::crypto::rsa::{HashId, RsaPublic};

// --- algorithm OIDs, as DER contents ---------------------------------------

/// 1.2.840.113549.1.1.1
pub const OID_RSA: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01];
/// 1.2.840.113549.1.1.11 / .12 / .13
pub const OID_SHA256_RSA: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x0b];
pub const OID_SHA384_RSA: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x0c];
pub const OID_SHA512_RSA: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x0d];
/// 1.2.840.113549.1.1.10
pub const OID_RSA_PSS: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x0a];
/// 1.2.840.10045.2.1
pub const OID_EC: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01];
/// 1.2.840.10045.4.3.2 / .3 / .4
pub const OID_ECDSA_SHA256: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03, 0x02];
pub const OID_ECDSA_SHA384: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03, 0x03];
pub const OID_ECDSA_SHA512: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03, 0x04];
/// 1.2.840.10045.3.1.7 (P-256) and 1.3.132.0.34 (P-384)
pub const OID_P256: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07];
pub const OID_P384: &[u8] = &[0x2b, 0x81, 0x04, 0x00, 0x22];
/// 2.5.29.17 and 2.5.29.19
pub const OID_SAN: &[u8] = &[0x55, 0x1d, 0x11];
pub const OID_BASIC_CONSTRAINTS: &[u8] = &[0x55, 0x1d, 0x13];

/// A parsed certificate.  Every field borrows from the DER it was parsed from.
#[derive(Clone, Copy)]
pub struct Cert<'a> {
    /// The TBSCertificate exactly as it appeared — what the signature covers.
    pub tbs: &'a [u8],
    /// The signature algorithm's OID contents.
    pub sig_alg: &'a [u8],
    /// The full AlgorithmIdentifier, needed for RSA-PSS's parameters.
    pub sig_alg_full: &'a [u8],
    pub signature: &'a [u8],
    /// The issuer's Name, as raw DER, for matching against a subject.
    pub issuer: &'a [u8],
    pub subject: &'a [u8],
    pub not_before: i64,
    pub not_after: i64,
    /// The public key algorithm's OID contents.
    pub key_alg: &'a [u8],
    /// The curve OID, when the key is an EC key.
    pub curve: &'a [u8],
    /// The key itself: an RSAPublicKey SEQUENCE, or a point.
    pub key: &'a [u8],
    /// The basicConstraints cA flag.
    pub is_ca: bool,
    /// The subjectAltName extension body, if present.
    pub san: &'a [u8],
}

/// Parse one certificate.
pub fn parse(der_bytes: &[u8]) -> Option<Cert<'_>> {
    let cert = der::parse_one(der_bytes)?;
    if cert.tag != TAG_SEQUENCE {
        return None;
    }
    let mut r = Reader::new(cert.body);
    let tbs = r.expect(TAG_SEQUENCE)?;
    let alg = r.expect(TAG_SEQUENCE)?;
    let sig = r.expect(TAG_BIT_STRING)?;
    let signature = der::bit_string(&sig)?;

    let mut ar = Reader::new(alg.body);
    let sig_alg = ar.expect(TAG_OID)?.body;

    // TBSCertificate.  The version is optional and encoded as [0]; everything
    // after it is positional.
    let mut t = Reader::new(tbs.body);
    let _version = if t.peek_tag() == Some(0xA0) {
        t.next()
    } else {
        None
    };
    let _serial = t.expect(TAG_INTEGER)?;
    let _inner_alg = t.expect(TAG_SEQUENCE)?;
    let issuer = t.expect(TAG_SEQUENCE)?;
    let validity = t.expect(TAG_SEQUENCE)?;
    let subject = t.expect(TAG_SEQUENCE)?;
    let spki = t.expect(TAG_SEQUENCE)?;

    let mut vr = Reader::new(validity.body);
    let nb = vr.next()?;
    let na = vr.next()?;
    let not_before = parse_time(&nb)?;
    let not_after = parse_time(&na)?;

    let mut sr = Reader::new(spki.body);
    let keyalg = sr.expect(TAG_SEQUENCE)?;
    let keybits = sr.expect(TAG_BIT_STRING)?;
    let key = der::bit_string(&keybits)?;
    let mut kr = Reader::new(keyalg.body);
    let key_alg = kr.expect(TAG_OID)?.body;
    let curve: &[u8] = match kr.expect(TAG_OID) {
        Some(o) => o.body,
        None => &[],
    };

    // The extensions are tagged [3] and come after two optional fields that
    // nothing in practice sends.
    let mut is_ca = false;
    let mut san: &[u8] = &[];
    let mut ext = t;
    while let Some(e) = ext.next() {
        if e.tag != 0xA3 {
            continue;
        }
        let Some(exts) = der::parse_one(e.body) else { continue };
        if exts.tag != TAG_SEQUENCE {
            continue;
        }
        let mut xr = Reader::new(exts.body);
        while let Some(x) = xr.expect(TAG_SEQUENCE) {
            let mut one = Reader::new(x.body);
            let Some(oid) = one.expect(TAG_OID) else { continue };
            // critical is an optional BOOLEAN before the value
            if one.peek_tag() == Some(TAG_BOOLEAN) {
                let _ = one.next();
            }
            let Some(value) = one.expect(TAG_OCTET_STRING) else { continue };
            if der::oid_is(oid.body, OID_SAN) {
                san = value.body;
            } else if der::oid_is(oid.body, OID_BASIC_CONSTRAINTS) {
                if let Some(bc) = der::parse_one(value.body) {
                    let mut br = Reader::new(bc.body);
                    if br.peek_tag() == Some(TAG_BOOLEAN) {
                        if let Some(b) = br.next() {
                            is_ca = b.body.first().copied().unwrap_or(0) != 0;
                        }
                    }
                }
            }
        }
    }

    Some(Cert {
        tbs: tbs.full,
        sig_alg,
        sig_alg_full: alg.full,
        signature,
        issuer: issuer.full,
        subject: subject.full,
        not_before,
        not_after,
        key_alg,
        curve,
        key,
        is_ca,
        san,
    })
}

/// Parse a UTCTime or GeneralizedTime into seconds since the Unix epoch.
fn parse_time(t: &Tlv) -> Option<i64> {
    let body = t.body;
    let (year, rest) = match t.tag {
        der::TAG_UTC_TIME => {
            if body.len() < 11 {
                return None;
            }
            let yy = two(&body[0..2])?;
            // RFC 5280: 50..99 is 19xx, 00..49 is 20xx.
            let y = if yy >= 50 { 1900 + yy } else { 2000 + yy };
            (y, &body[2..])
        }
        der::TAG_GENERALIZED_TIME => {
            if body.len() < 13 {
                return None;
            }
            let y = four(&body[0..4])?;
            (y, &body[4..])
        }
        _ => return None,
    };
    // MM DD HH MM SS, and the trailing Z.  Fractional seconds are not used in
    // certificates and would be a reason to refuse rather than guess.
    if rest.len() < 10 + 1 || rest[10] != b'Z' {
        return None;
    }
    let mo = two(&rest[0..2])?;
    let d = two(&rest[2..4])?;
    let h = two(&rest[4..6])?;
    let mi = two(&rest[6..8])?;
    let s = two(&rest[8..10])?;
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || s > 60 {
        return None;
    }
    Some(days_from_civil(year, mo, d) * 86400 + h * 3600 + mi * 60 + s)
}

fn two(b: &[u8]) -> Option<i64> {
    if b.len() < 2 || !b[0].is_ascii_digit() || !b[1].is_ascii_digit() {
        return None;
    }
    Some(((b[0] - b'0') as i64) * 10 + (b[1] - b'0') as i64)
}

fn four(b: &[u8]) -> Option<i64> {
    if b.len() < 4 || !b.iter().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(((b[0] - b'0') as i64) * 1000 + ((b[1] - b'0') as i64) * 100
        + ((b[2] - b'0') as i64) * 10 + (b[3] - b'0') as i64)
}

/// Days since 1970-01-01, by Howard Hinnant's civil-from-days inverse.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// The hash a signature algorithm names.
fn hash_of(oid: &[u8]) -> Option<HashId> {
    if der::oid_is(oid, OID_SHA256_RSA) || der::oid_is(oid, OID_ECDSA_SHA256) {
        Some(HashId::Sha256)
    } else if der::oid_is(oid, OID_SHA384_RSA) || der::oid_is(oid, OID_ECDSA_SHA384) {
        Some(HashId::Sha384)
    } else if der::oid_is(oid, OID_SHA512_RSA) || der::oid_is(oid, OID_ECDSA_SHA512) {
        Some(HashId::Sha512)
    } else {
        None
    }
}

/// Check that `self` was signed by the key in `issuer`.
pub fn verify_signed_by(child: &Cert, issuer: &Cert) -> bool {
    // The signature algorithm has to be one the issuer's key can produce.
    let Some(hash) = hash_of(child.sig_alg) else { return false };

    let mut digest = [0u8; 64];
    let dn = hash.hash(child.tbs, &mut digest);

    if der::oid_is(issuer.key_alg, OID_RSA) {
        // An RSAPublicKey is SEQUENCE { modulus INTEGER, exponent INTEGER }.
        let Some(k) = der::parse_one(issuer.key) else { return false };
        let mut kr = Reader::new(k.body);
        let Some(n_t) = kr.expect(TAG_INTEGER) else { return false };
        let Some(e_t) = kr.expect(TAG_INTEGER) else { return false };
        let Some(n) = der::int_bytes(&n_t) else { return false };
        let Some(e) = der::int_bytes(&e_t) else { return false };
        let Some(key) = RsaPublic::new(n, e) else { return false };

        if der::oid_is(child.sig_alg, OID_RSA_PSS) {
            key.verify_pss(child.signature, hash, &digest[..dn])
        } else {
            key.verify_pkcs1_v15(child.signature, hash, &digest[..dn])
        }
    } else if der::oid_is(issuer.key_alg, OID_EC) {
        let curve = if der::oid_is(issuer.curve, OID_P256) {
            Ecdsa::new(&ec::P256)
        } else if der::oid_is(issuer.curve, OID_P384) {
            Ecdsa::new(&ec::P384)
        } else {
            None
        };
        let Some(curve) = curve else { return false };
        // An ECDSA signature is DER SEQUENCE { r INTEGER, s INTEGER }.
        let Some(s) = der::parse_one(child.signature) else { return false };
        if s.tag != TAG_SEQUENCE {
            return false;
        }
        let mut sr = Reader::new(s.body);
        let (Some(r_t), Some(s_t)) = (sr.expect(TAG_INTEGER), sr.expect(TAG_INTEGER)) else {
            return false;
        };
        let (Some(r), Some(sv)) = (der::int_bytes(&r_t), der::int_bytes(&s_t)) else {
            return false;
        };
        // Zero-pad to the coordinate width; a short r is legal and means a
        // small integer, not a malformed one.
        let w = curve.order_len();
        let mut rp = [0u8; 64];
        let mut sp = [0u8; 64];
        if r.len() > w || sv.len() > w {
            return false;
        }
        rp[w - r.len()..w].copy_from_slice(r);
        sp[w - sv.len()..w].copy_from_slice(sv);
        curve.verify(issuer.key, &rp[..w], &sp[..w], &digest[..dn])
    } else {
        false
    }
}

/// Do two Names have the same DER?  Byte equality is the right test: a
/// certificate names its issuer with the exact encoding the issuer used for
/// its own subject.
pub fn same_name(a: &[u8], b: &[u8]) -> bool {
    a == b
}

/// Does the certificate cover `host`?
///
/// Only subjectAltName is consulted.  The common name was removed from the
/// rules years ago and honouring it is how a certificate for
/// `www.evil.example` with `CN=bank.example` gets accepted.
pub fn hostname_matches(cert: &Cert, host: &str) -> bool {
    if cert.san.is_empty() {
        return false;
    }
    let Some(seq) = der::parse_one(cert.san) else { return false };
    if seq.tag != TAG_SEQUENCE {
        return false;
    }
    let mut r = Reader::new(seq.body);
    while let Some(name) = r.next() {
        // dNSName is context tag 2, primitive.
        if name.tag != 0x82 {
            continue;
        }
        let Ok(pattern) = core::str::from_utf8(name.body) else { continue };
        if name_matches(pattern, host) {
            return true;
        }
    }
    false
}

/// Wildcard matching, with the rules the CAB forum actually requires: a
/// wildcard only in the leftmost label, and only one of them.
fn name_matches(pattern: &str, host: &str) -> bool {
    if !pattern.contains('*') {
        return pattern.eq_ignore_ascii_case(host);
    }
    let Some(rest) = pattern.strip_prefix("*.") else { return false };
    if rest.contains('*') {
        return false;
    }
    let Some(dot) = host.find('.') else { return false };
    // "*" matches at least one label, so "*.example.com" does not match
    // "example.com", and "*." with nothing after it matches nothing.
    let host_rest = &host[dot + 1..];
    !host_rest.is_empty() && rest.eq_ignore_ascii_case(host_rest)
}

/// A readable name for the log, from the subject's commonName.
pub fn common_name(cert: &Cert, out: &mut [u8]) -> usize {
    let mut r = Reader::new(cert.subject);
    let Some(seq) = r.expect(TAG_SEQUENCE) else { return 0 };
    let mut sets = Reader::new(seq.body);
    while let Some(set) = sets.expect(der::TAG_SET) {
        let mut attrs = Reader::new(set.body);
        while let Some(attr) = attrs.expect(TAG_SEQUENCE) {
            let mut a = Reader::new(attr.body);
            let Some(oid) = a.expect(TAG_OID) else { continue };
            // 2.5.4.3 is commonName.
            if !der::oid_is(oid.body, &[0x55, 0x04, 0x03]) {
                continue;
            }
            let Some(v) = a.next() else { continue };
            let n = v.body.len().min(out.len());
            out[..n].copy_from_slice(&v.body[..n]);
            return n;
        }
    }
    0
}

pub fn selftest() -> usize {
    use crate::crypto::{check, unhex};
    use crate::crypto::x509_vectors as v;

    let mut f = 0usize;

    // One buffer per certificate: a parsed certificate borrows the DER it came
    // from, so the buffers have to outlive the parse.
    let mut b0 = [0u8; 2048];
    let mut b1 = [0u8; 2048];
    let mut b2 = [0u8; 2048];
    let mut b3 = [0u8; 2048];
    let mut lens = [0usize; 4];
    for (i, (hexs, buf)) in [
        (v::CERT_LEAF, &mut b0),
        (v::CERT_INTERMEDIATE, &mut b1),
        (v::CERT_ROOT_A, &mut b2),
        (v::CERT_ROOT_B, &mut b3),
    ]
    .into_iter()
    .enumerate()
    {
        lens[i] = unhex(hexs, buf);
    }
    if lens.iter().any(|&n| n == 0) {
        return check("x509 vectors decode", &[], &[1]);
    }

    let bufs: [&[u8]; 4] = [
        &b0[..lens[0]], &b1[..lens[1]], &b2[..lens[2]], &b3[..lens[3]],
    ];
    let mut parsed: [Option<Cert>; 4] = [None, None, None, None];
    for (i, d) in bufs.iter().enumerate() {
        match parse(d) {
            Some(c) => {
                parsed[i] = Some(c);
                f += check("x509 parses a certificate", &[1], &[1]);
            }
            None => f += check("x509 parses a certificate", &[], &[1]),
        }
    }

    let (Some(leaf), Some(inter), Some(root_a)) = (parsed[0], parsed[1], parsed[2]) else {
        return f + 1;
    };

    // The chain: each certificate signed by the one above it.
    f += check("x509 leaf signed by intermediate", &[verify_signed_by(&leaf, &inter) as u8], &[1]);
    f += check("x509 intermediate signed by root", &[verify_signed_by(&inter, &root_a) as u8], &[1]);
    // And the issuer/subject names line up, which is what makes it a chain
    // rather than three certificates that happen to verify.
    f += check("x509 leaf issuer is the intermediate",
               &[same_name(leaf.issuer, inter.subject) as u8], &[1]);
    f += check("x509 intermediate issuer is the root",
               &[same_name(inter.issuer, root_a.subject) as u8], &[1]);
    // The wrong issuer must fail: a verifier that ignores the key it is
    // given passes everything above.
    f += check("x509 rejects the wrong issuer", &[verify_signed_by(&leaf, &root_a) as u8], &[0]);

    // The name in the certificate.
    f += check("x509 san matches example.com", &[hostname_matches(&leaf, "example.com") as u8], &[1]);
    f += check("x509 san matches a wildcard", &[hostname_matches(&leaf, "www.example.com") as u8], &[1]);
    f += check("x509 san rejects another host", &[hostname_matches(&leaf, "evil.example") as u8], &[0]);
    f += check("x509 rejects an empty match", &[hostname_matches(&leaf, "") as u8], &[0]);

    // CA flags: the leaf is not one, the intermediate is.
    f += check("x509 leaf is not a CA", &[leaf.is_ca as u8], &[0]);
    f += check("x509 intermediate is a CA", &[inter.is_ca as u8], &[1]);

    // The validity window, as a range rather than exact dates.
    f += check("x509 validity is sane", &[(leaf.not_after > leaf.not_before) as u8], &[1]);
    f += check("x509 dates are after 2020", &[(leaf.not_before > 1_577_836_800) as u8], &[1]);

    // And a certificate must not verify against itself confused for another:
    // the intermediate's signature does not check out under the leaf's key.
    f += check("x509 rejects a swapped chain", &[verify_signed_by(&inter, &leaf) as u8], &[0]);

    f
}
