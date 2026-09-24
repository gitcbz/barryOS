//! RSA signature verification (PKCS#1 v1.5 and PSS).
//!
//! Verification only.  There is no signing, no key generation and no private
//! key anywhere in this kernel, which is also why none of the usual warnings
//! about RSA padding apply: an attacker who can choose our padding gets
//! nothing, because we never produce a signature.
//!
//! Two paddings, because both are in use where this is called.  Certificate
//! chains and the RSA suites of TLS 1.2 sign with PKCS#1 v1.5; TLS 1.3 signs
//! its key exchange with PSS.  A verifier that knows only one of them fails on
//! half of the internet.

use crate::crypto::bignum::{from_be_bytes, to_be_bytes, Big, Modulus, MAX_LIMBS};
use crate::crypto::sha512::{hash384, hash512};
use crate::sha256::hash as hash256;

/// Which hash a signature was made with.  Chains use whichever the issuer
/// chose, and roots in particular are often SHA-384.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HashId {
    Sha256,
    Sha384,
    Sha512,
}

impl HashId {
    pub fn digest_len(self) -> usize {
        match self {
            HashId::Sha256 => 32,
            HashId::Sha384 => 48,
            HashId::Sha512 => 64,
        }
    }

    /// The DER prefix of a PKCS#1 v1.5 DigestInfo for this hash: an
    /// AlgorithmIdentifier naming the hash, followed by the OCTET STRING
    /// header the digest goes into.
    pub fn digest_info_prefix(self) -> &'static [u8] {
        match self {
            // SEQUENCE { SEQUENCE { OID 2.16.840.1.101.3.4.2.1, NULL }, OCTET STRING }
            HashId::Sha256 => &[
                0x30, 0x31, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01,
                0x65, 0x03, 0x04, 0x02, 0x01, 0x05, 0x00, 0x04, 0x20,
            ],
            // ... 2.2
            HashId::Sha384 => &[
                0x30, 0x41, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01,
                0x65, 0x03, 0x04, 0x02, 0x02, 0x05, 0x00, 0x04, 0x30,
            ],
            // ... 2.3
            HashId::Sha512 => &[
                0x30, 0x51, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01,
                0x65, 0x03, 0x04, 0x02, 0x03, 0x05, 0x00, 0x04, 0x40,
            ],
        }
    }

    /// Hash `data` into `out`; returns the length written.
    pub fn hash(self, data: &[u8], out: &mut [u8; 64]) -> usize {
        match self {
            HashId::Sha256 => {
                out[..32].copy_from_slice(&hash256(data));
                32
            }
            HashId::Sha384 => {
                out[..48].copy_from_slice(&hash384(data));
                48
            }
            HashId::Sha512 => {
                out[..64].copy_from_slice(&hash512(data));
                64
            }
        }
    }

    /// Hash a two-part message, for the certificate case where the signed
    /// bytes are a slice of a larger buffer.
    pub fn hash2(self, a: &[u8], b: &[u8], out: &mut [u8; 64]) -> usize {
        match self {
            HashId::Sha256 => {
                let mut h = crate::sha256::Sha256::new();
                h.update(a);
                h.update(b);
                out[..32].copy_from_slice(&h.finish());
                32
            }
            HashId::Sha384 => {
                let mut h = crate::crypto::sha512::Sha512::new384();
                h.update(a);
                h.update(b);
                out[..48].copy_from_slice(&h.finish384());
                48
            }
            HashId::Sha512 => {
                let mut h = crate::crypto::sha512::Sha512::new512();
                h.update(a);
                h.update(b);
                out[..64].copy_from_slice(&h.finish512());
                64
            }
        }
    }
}

/// An RSA public key, ready to check signatures.
pub struct RsaPublic {
    md: Modulus,
    n: usize,
    exp: [u8; 8],
    exp_len: usize,
}

impl RsaPublic {
    /// From the DER INTEGER contents of a SubjectPublicKeyInfo: the modulus
    /// and the exponent, both big-endian without their DER headers.
    pub fn new(modulus_be: &[u8], exp_be: &[u8]) -> Option<Self> {
        if modulus_be.is_empty() || exp_be.is_empty() || exp_be.len() > 8 {
            return None;
        }
        // A leading zero byte is DER's way of keeping a positive number
        // positive; the limb converter does not need it but must not choke.
        let m_be = if modulus_be[0] == 0 { &modulus_be[1..] } else { modulus_be };
        let (m, n) = from_be_bytes(m_be);
        if n == 0 || n > MAX_LIMBS {
            return None;
        }
        let md = Modulus::new(m, n)?;
        let mut exp = [0u8; 8];
        let e = if exp_be[0] == 0 { &exp_be[1..] } else { exp_be };
        let e = &e[e.len().saturating_sub(8)..];
        exp[8 - e.len()..].copy_from_slice(e);
        Some(Self { md, n, exp, exp_len: e.len() })
    }

    /// The modulus size in bytes.
    pub fn modulus_bytes(&self) -> usize {
        self.n * 8
    }

    /// The modulus size in bits, counting only significant bits.
    fn modulus_bits(&self) -> usize {
        for i in (0..self.n).rev() {
            if self.md.limb(i) != 0 {
                return i * 64 + (64 - self.md.limb(i).leading_zeros() as usize);
            }
        }
        0
    }

    /// s^e mod n, or None if the signature is out of range.
    ///
    /// A signature that is not smaller than the modulus is rejected rather
    /// than reduced: silently reducing it is precisely the fault that lets a
    /// signature be forged.
    fn recover(&self, sig: &[u8]) -> Option<[u8; MAX_LIMBS * 8]> {
        let (s, sn) = from_be_bytes(sig);
        if sn > self.n || (sn == self.n && self.md.ge(&s)) {
            return None;
        }
        let m = self.md.pow(&s, &self.exp[8 - self.exp_len..]);
        let mut out = [0u8; MAX_LIMBS * 8];
        to_be_bytes(&m, self.n, &mut out);
        Some(out)
    }

    /// The recovered encoded message, for the self-test to print when a
    /// verification fails.  Not used by any verification path.
    pub fn debug_recover(&self, sig: &[u8], out: &mut [u8]) -> bool {
        match self.recover(sig) {
            Some(em) => {
                let n = out.len().min(self.modulus_bytes());
                out[..n].copy_from_slice(&em[..n]);
                true
            }
            None => false,
        }
    }

    /// PKCS#1 v1.5: 0x00 0x01 FF..FF 0x00 || DigestInfo.
    ///
    /// The padding check is not optional and the whole structure is compared:
    /// accepting a signature with the digest anywhere in the block is the
    /// Bleichenbacher e=3 forgery.
    pub fn verify_pkcs1_v15(&self, sig: &[u8], hash: HashId, digest: &[u8]) -> bool {
        let n = self.modulus_bytes();
        if sig.len() != n || digest.len() != hash.digest_len() {
            return false;
        }
        let Some(em) = self.recover(sig) else { return false };

        let prefix = hash.digest_info_prefix();
        let t_len = prefix.len() + digest.len();
        // The padding must be at least 8 bytes of 0xFF.
        if n < t_len + 11 {
            return false;
        }
        if em[0] != 0x00 || em[1] != 0x01 {
            return false;
        }
        let mut i = 2usize;
        while i < n && em[i] == 0xFF {
            i += 1;
        }
        // At least 8 bytes of 0xFF, then a zero separator.
        if i < 10 || i >= n || em[i] != 0x00 {
            return false;
        }
        i += 1;
        if n - i != t_len {
            return false;
        }
        if em[i..i + prefix.len()] != *prefix {
            return false;
        }
        em[i + prefix.len()..i + prefix.len() + digest.len()] == *digest
    }

    /// RSASSA-PSS with MGF1 over the same hash, which is what TLS 1.3 signs
    /// its key exchange with.
    pub fn verify_pss(&self, sig: &[u8], hash: HashId, digest: &[u8]) -> bool {
        let n = self.modulus_bytes();
        if sig.len() != n {
            return false;
        }
        let h_len = hash.digest_len();
        let Some(em) = self.recover(sig) else { return false };

        // emBits is one less than the modulus bit length, which is what makes
        // the first byte of the encoding have spare high bits.  Taking it as
        // 8*n instead would accept an encoding the standard does not.
        let em_bits = self.modulus_bits() - 1;
        let em_len = (em_bits + 7) / 8;
        if em_len > n || em_len < h_len + 2 {
            return false;
        }
        if em[em_len - 1] != 0xbc {
            return false;
        }
        let unused = 8 * em_len - em_bits;
        if em[0] >> (8 - unused) != 0 {
            return false;
        }

        let db_len = em_len - h_len - 1;
        // maskedDB || H || 0xbc
        let mut db = [0u8; MAX_LIMBS * 8];
        let h_at = db_len;
        let mut mgf_in = [0u8; 64 + 4];
        mgf_in[..h_len].copy_from_slice(&em[h_at..h_at + h_len]);

        // MGF1: T = Hash(seed || counter)
        let mut masked_len = 0usize;
        let mut counter = 0u32;
        while masked_len < db_len {
            mgf_in[h_len..h_len + 4].copy_from_slice(&counter.to_be_bytes());
            let mut block = [0u8; 64];
            let bl = hash.hash(&mgf_in[..h_len + 4], &mut block);
            let take = (db_len - masked_len).min(bl);
            for k in 0..take {
                db[masked_len + k] = em[masked_len + k] ^ block[k];
            }
            masked_len += take;
            counter += 1;
        }
        // Clear the bits that are not part of the value.
        if unused > 0 {
            db[0] &= 0xFFu8 >> unused;
        }

        // DB = PS || 0x01 || salt, so find the 0x01 that ends the zero run.
        let mut i = 0usize;
        while i < db_len && db[i] == 0 {
            i += 1;
        }
        if i >= db_len || db[i] != 0x01 {
            return false;
        }
        let salt_at = i + 1;
        let salt_len = db_len - salt_at;

        // H' = Hash(0x00 * 8 || digest || salt)
        let mut hbuf = [0u8; 64];
        let mut hh = crate::sha256::Sha256::new();
        match hash {
            HashId::Sha256 => {
                hh.update(&[0u8; 8]);
                hh.update(digest);
                hh.update(&db[salt_at..db_len]);
                hbuf[..32].copy_from_slice(&hh.finish());
            }
            _ => {
                let mut h = if hash == HashId::Sha384 {
                    crate::crypto::sha512::Sha512::new384()
                } else {
                    crate::crypto::sha512::Sha512::new512()
                };
                h.update(&[0u8; 8]);
                h.update(digest);
                h.update(&db[salt_at..db_len]);
                if hash == HashId::Sha384 {
                    hbuf[..48].copy_from_slice(&h.finish384());
                } else {
                    hbuf[..64].copy_from_slice(&h.finish512());
                }
            }
        }
        let _ = salt_len;

        // Compared without an early exit, like every other tag check here.
        let mut diff = 0u8;
        for k in 0..h_len {
            diff |= hbuf[k] ^ em[h_at + k];
        }
        diff == 0
    }
}

pub fn selftest() -> usize {
    use crate::crypto::vectors as v;
    use crate::crypto::{check, unhex};
    let mut f = 0usize;

    let mut n = [0u8; 256];
    unhex(v::RSA_N, &mut n);
    let mut e = [0u8; 8];
    let en = unhex(v::RSA_E, &mut e);
    let Some(key) = RsaPublic::new(&n, &e[..en]) else {
        return check("rsa key setup", &[], &[1]);
    };

    // PKCS#1 v1.5 under all three hashes.  The signatures come from OpenSSL;
    // this verifies them and hashes the message itself, so a wrong DigestInfo
    // prefix or a wrong digest length shows up here.
    for (name, hash, hex) in [
        ("rsa pkcs1 sha256", HashId::Sha256, v::RSA_PKCS1_SHA256_SIG),
        ("rsa pkcs1 sha384", HashId::Sha384, v::RSA_PKCS1_SHA384_SIG),
        ("rsa pkcs1 sha512", HashId::Sha512, v::RSA_PKCS1_SHA512_SIG),
    ] {
        let mut sig = [0u8; 256];
        unhex(hex, &mut sig);
        let mut digest = [0u8; 64];
        let dn = hash.hash(v::RSA_MSG, &mut digest);
        let ok = key.verify_pkcs1_v15(&sig, hash, &digest[..dn]);
        f += check(name, &[ok as u8], &[1]);
    }

    // The same under PSS, which is what TLS 1.3 signs with.
    for (name, hash, hex) in [
        ("rsa pss sha256", HashId::Sha256, v::RSA_PSS_SHA256_SIG),
        ("rsa pss sha384", HashId::Sha384, v::RSA_PSS_SHA384_SIG),
        ("rsa pss sha512", HashId::Sha512, v::RSA_PSS_SHA512_SIG),
    ] {
        let mut sig = [0u8; 256];
        unhex(hex, &mut sig);
        let mut digest = [0u8; 64];
        let dn = hash.hash(v::RSA_MSG, &mut digest);
        let ok = key.verify_pss(&sig, hash, &digest[..dn]);
        f += check(name, &[ok as u8], &[1]);
    }

    // And the two that must fail.  A verifier that accepts everything passes
    // every test above.
    {
        // Print what the signature actually decodes to.  Without this, "it
        // returned false" says nothing about which half is wrong.
        let mut sig = [0u8; 256];
        unhex(v::RSA_PKCS1_SHA256_SIG, &mut sig);
        let mut em = [0u8; 256];
        let got = key.debug_recover(&sig, &mut em);
        crate::serial::print_str("[crypto]   debug em: ok=");
        crate::serial::print_dec(got as u64);
        crate::serial::print_str(" head ");
        for i in 0..12 {
            crate::serial::print_hex(em[i] as u64);
            crate::serial::print_str(" ");
        }
        crate::serial::print_str("tail ");
        for i in 224..236 {
            crate::serial::print_hex(em[i] as u64);
            crate::serial::print_str(" ");
        }
        crate::serial::print_str("
");
    }

    {
        let mut sig = [0u8; 256];
        unhex(v::RSA_PKCS1_SHA256_SIG_TAMPERED, &mut sig);
        let mut digest = [0u8; 64];
        let dn = HashId::Sha256.hash(v::RSA_MSG, &mut digest);
        let ok = key.verify_pkcs1_v15(&sig, HashId::Sha256, &digest[..dn]);
        f += check("rsa rejects a tampered v1.5 signature", &[ok as u8], &[0]);
    }
    {
        let mut sig = [0u8; 256];
        unhex(v::RSA_PSS_SHA256_SIG_TAMPERED, &mut sig);
        let mut digest = [0u8; 64];
        let dn = HashId::Sha256.hash(v::RSA_MSG, &mut digest);
        let ok = key.verify_pss(&sig, HashId::Sha256, &digest[..dn]);
        f += check("rsa rejects a tampered PSS signature", &[ok as u8], &[0]);
    }
    {
        // A valid PKCS#1 signature checked as if it were PSS: the paddings
        // are not interchangeable and a verifier that ignores the algorithm
        // identifier would take it.
        let mut sig = [0u8; 256];
        unhex(v::RSA_PKCS1_SHA256_SIG, &mut sig);
        let mut digest = [0u8; 64];
        let dn = HashId::Sha256.hash(v::RSA_MSG, &mut digest);
        let ok = key.verify_pss(&sig, HashId::Sha256, &digest[..dn]);
        f += check("rsa does not mix paddings", &[ok as u8], &[0]);
    }

    f
}
