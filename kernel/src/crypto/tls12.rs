//! TLS 1.2 (RFC 5246), client side — the other half of the handshake.
//!
//! TLS 1.3 is the protocol this client prefers and the one nearly every
//! server offers, but not all of them: baidu.com answers a 1.3-only hello
//! with `protocol_version` and closes.  A browser that cannot reach a third
//! of the sites someone is likely to type is not finished, so this exists.
//!
//! The two versions are different protocols sharing a record header, and the
//! differences that matter are all in the places that are hard to test:
//!
//!   * the key schedule is a PRF over two random values rather than an HKDF
//!     ladder over a transcript, and the PRF is `HMAC(A(i) || seed)` chained;
//!   * the ephemeral key is signed by the server (ServerKeyExchange) instead
//!     of proved by a CertificateVerify, so the signature covers the two
//!     randoms and it is the only thing binding the exchange to this session;
//!   * AEAD records carry an explicit nonce and authenticate a header that
//!     includes a sequence number, rather than folding the sequence number
//!     into the nonce and authenticating their own header;
//!   * Finished is 12 bytes, not a full hash.
//!
//! Nothing here is shared with `tls.rs` except the PRF's building blocks and
//! the record layer's buffers.  Everything that could be shared is different
//! enough that sharing it would be two versions of each function anyway.

use crate::crypto::ec::{self, Ec};
use crate::crypto::hmac::{hmac, hmac_parts};
use crate::crypto::x25519;
use crate::sha256::{hash as sha256, Sha256};

/// The AEAD suites this client offers.  GCM only, and ECDHE only: CBC is the
/// padding-oracle family, and RSA key transport means the session key is
/// encrypted to a long-term key rather than agreed, which is the property 1.3
/// was written to remove.  Both are still widely offered and neither is worth
/// implementing to read a home page.
pub const SUITE_ECDHE_ECDSA_AES128_GCM_SHA256: u16 = 0xC02B;
pub const SUITE_ECDHE_RSA_AES128_GCM_SHA256: u16 = 0xC02F;

/// The version this module speaks, and the one written into every record.
pub const VERSION_TLS12: u16 = 0x0303;

/// Named groups, as they appear in `supported_groups`.
pub const GROUP_SECP256R1: u16 = 0x0017;
pub const GROUP_X25519: u16 = 0x001d;

/// Handshake message types this module reads.
const HS_SERVER_HELLO: u8 = 2;
const HS_CERTIFICATE: u8 = 11;
const HS_SERVER_KEY_EXCHANGE: u8 = 12;
const HS_CERTIFICATE_REQUEST: u8 = 13;
const HS_SERVER_HELLO_DONE: u8 = 14;
const HS_FINISHED: u8 = 20;

/// Signature algorithms, in the two-byte form TLS 1.2 uses.
const SIG_RSA_PKCS1_SHA256: u16 = 0x0401;
const SIG_RSA_PKCS1_SHA384: u16 = 0x0501;
const SIG_RSA_PKCS1_SHA512: u16 = 0x0601;
const SIG_ECDSA_SHA256: u16 = 0x0403;
const SIG_ECDSA_SHA384: u16 = 0x0503;
const SIG_ECDSA_SHA512: u16 = 0x0603;

// ---------------------------------------------------------------------------
//  The PRF
// ---------------------------------------------------------------------------

/// P_hash (RFC 5246 §5): the chained HMAC that everything in TLS 1.2 is
/// derived through.
///
///     A(0) = seed
///     A(i) = HMAC(secret, A(i-1))
///     P    = HMAC(secret, A(1) || seed) || HMAC(secret, A(2) || seed) || ...
///
/// The counter is implicit in the chain, which is what makes this different
/// from HKDF: there is no salt, no extract step, and the secret is used
/// directly as an HMAC key.
fn p_hash(secret: &[u8], seed: &[u8], out: &mut [u8]) {
    let mut a = hmac(secret, seed);
    let mut written = 0usize;
    while written < out.len() {
        let block = hmac_parts(secret, &[&a, seed]);
        let take = (out.len() - written).min(block.len());
        out[written..written + take].copy_from_slice(&block[..take]);
        written += take;
        a = hmac(secret, &a);
    }
}

/// PRF(secret, label, seed) = P_hash(secret, label || seed).
pub fn prf(secret: &[u8], label: &[u8], seed: &[u8], out: &mut [u8]) {
    let mut label_seed = [0u8; 128];
    let n = (label.len() + seed.len()).min(label_seed.len());
    let split = label.len().min(n);
    label_seed[..split].copy_from_slice(&label[..split]);
    label_seed[split..n].copy_from_slice(&seed[..n - split]);
    p_hash(secret, &label_seed[..n], out);
}

/// The two randoms, in the order each derivation wants them.
fn seed_of(client_random: &[u8; 32], server_random: &[u8; 32]) -> [u8; 64] {
    let mut s = [0u8; 64];
    s[..32].copy_from_slice(client_random);
    s[32..].copy_from_slice(server_random);
    s
}

// ---------------------------------------------------------------------------
//  Handshake state
// ---------------------------------------------------------------------------

/// What the server chose, and what came out of it.
pub struct Handshake {
    pub suite: u16,
    /// The group the server's ephemeral key is on.
    pub group: u16,
    pub client_random: [u8; 32],
    pub server_random: [u8; 32],
    /// The ECDHE shared secret, before it is hashed into the master secret.
    pub pre_master: [u8; 32],
    pub pre_master_len: usize,
    pub master: [u8; 48],
    /// AEAD keys and implicit nonces, from the key block.
    pub client_key: [u8; 16],
    pub server_key: [u8; 16],
    pub client_iv: [u8; 4],
    pub server_iv: [u8; 4],
    /// Our ephemeral private value, kept until the key exchange goes out.
    pub our_private: [u8; 32],
    pub our_private_len: usize,
    pub our_public: [u8; 65],
    pub our_public_len: usize,
    /// Set once the server's Finished has been checked.
    pub done: bool,
    /// Set when the server asked for a certificate, which we answer with none.
    pub want_client_cert: bool,
    pub failed: bool,
    pub why: &'static str,
}

impl Handshake {
    pub const EMPTY: Handshake = Handshake {
        suite: 0,
        group: 0,
        client_random: [0; 32],
        server_random: [0; 32],
        pre_master: [0; 32],
        pre_master_len: 0,
        master: [0; 48],
        client_key: [0; 16],
        server_key: [0; 16],
        client_iv: [0; 4],
        server_iv: [0; 4],
        our_private: [0; 32],
        our_private_len: 0,
        our_public: [0; 65],
        our_public_len: 0,
        done: false,
        want_client_cert: false,
        failed: false,
        why: "",
    };

    fn fail(&mut self, why: &'static str) -> bool {
        self.failed = true;
        self.why = why;
        false
    }

    /// Is `suite` one this client can actually do?
    pub fn suite_supported(suite: u16) -> bool {
        suite == SUITE_ECDHE_ECDSA_AES128_GCM_SHA256
            || suite == SUITE_ECDHE_RSA_AES128_GCM_SHA256
    }

    pub fn wants_ecdsa_certificate(&self) -> bool {
        self.suite == SUITE_ECDHE_ECDSA_AES128_GCM_SHA256
    }

    /// Read the ServerHello.  This is where the version is decided: a server
    /// that picked anything but 1.2 here is not this module's business.
    pub fn server_hello(&mut self, body: &[u8]) -> bool {
        let mut at = 0usize;
        if body.len() < 38 {
            return self.fail("the ServerHello is too short");
        }
        if u16::from_be_bytes([body[0], body[1]]) != VERSION_TLS12 {
            return self.fail("the server chose a version this client does not speak");
        }
        self.server_random.copy_from_slice(&body[2..34]);
        at = 34;
        let sid = body[at] as usize;
        at += 1 + sid;
        if at + 3 > body.len() {
            return self.fail("the ServerHello is malformed");
        }
        let suite = u16::from_be_bytes([body[at], body[at + 1]]);
        at += 2;
        if body[at] != 0 {
            return self.fail("the server chose a compression method");
        }
        if !Self::suite_supported(suite) {
            return self.fail("the server chose a cipher suite this client did not offer");
        }
        self.suite = suite;
        true
    }

    /// The server's ephemeral key, and the signature binding it to this
    /// session.
    ///
    /// The signature is over the two randoms and the parameters, and it is the
    /// only thing that stops anyone on the path from substituting their own
    /// key — the certificate says who the server is, but nothing about it is
    /// tied to *this* exchange without this check.
    pub fn server_key_exchange(&mut self, body: &[u8], leaf_key_alg: &[u8], leaf_curve: &[u8],
                               leaf_key: &[u8]) -> bool {
        let mut at = 0usize;
        if body.len() < 5 {
            return self.fail("the key exchange is too short");
        }
        let curve_type = body[at];
        at += 1;
        if curve_type != 3 {
            // 3 is named_curve, which is the only form anyone sends.
            return self.fail("the server named an explicit curve");
        }
        let group = u16::from_be_bytes([body[at], body[at + 1]]);
        at += 2;
        let point_len = body[at] as usize;
        at += 1;
        if at + point_len + 4 > body.len() {
            return self.fail("the key exchange is malformed");
        }
        let point = &body[at..at + point_len];
        at += point_len;
        let params = &body[..at];

        let scheme = u16::from_be_bytes([body[at], body[at + 1]]);
        at += 2;
        let sig_len = u16::from_be_bytes([body[at], body[at + 1]]) as usize;
        at += 2;
        if at + sig_len > body.len() {
            return self.fail("the key exchange signature is truncated");
        }
        let sig = &body[at..at + sig_len];

        // signed = client_random || server_random || params
        let mut signed = [0u8; 64 + 256];
        let n = 64 + params.len();
        if n > signed.len() {
            return self.fail("the key exchange parameters are too long");
        }
        signed[..32].copy_from_slice(&self.client_random);
        signed[32..64].copy_from_slice(&self.server_random);
        signed[64..n].copy_from_slice(params);

        if !verify_handshake_signature(
            &signed[..n], sig, scheme, leaf_key_alg, leaf_curve, leaf_key,
        ) {
            crate::serial::print_str("[tls] key exchange signature: scheme 0x");
            crate::serial::print_hex(scheme as u64);
            crate::serial::print_str(", signed ");
            crate::serial::print_dec(n as u64);
            crate::serial::print_str(" bytes, sig ");
            crate::serial::print_dec(sig_len as u64);
            crate::serial::print_str(" bytes, key alg ");
            crate::serial::print_dec(leaf_key_alg.len() as u64);
            crate::serial::print_str(" bytes\n");
            return self.fail("the key exchange signature did not verify");
        }

        self.group = group;
        // Our own ephemeral pair is made here rather than at the ClientHello,
        // because the group is the server's choice and it is only known now.
        // TLS 1.3 sends a share speculatively; 1.2 asks.
        let Some((plen, private, publen, public)) =
            generate_keypair(group, &mut crate::crypto::tls::random_bytes)
        else {
            return self.fail("could not generate a key for the group the server chose");
        };
        self.our_private[..plen].copy_from_slice(&private[..plen]);
        self.our_private_len = plen;
        self.our_public[..publen].copy_from_slice(&public[..publen]);
        self.our_public_len = publen;

        match group {
            GROUP_X25519 => {
                if point_len != 32 {
                    return self.fail("the server's x25519 key is the wrong length");
                }
                let mut peer = [0u8; 32];
                peer.copy_from_slice(point);
                let mut ours = [0u8; 32];
                ours.copy_from_slice(&self.our_private[..32]);
                let shared = x25519::scalarmult(&ours, &peer);
                if shared.iter().all(|&b| b == 0) {
                    return self.fail("the server's key share is a low-order point");
                }
                self.pre_master[..32].copy_from_slice(&shared);
                self.pre_master_len = 32;
            }
            GROUP_SECP256R1 => {
                let Some(p256) = Ec::new(&ec::P256) else {
                    return self.fail("could not set up P-256");
                };
                let mut secret = [0u8; 64];
                let Some(n) =
                    p256.shared_secret(&self.our_private[..self.our_private_len], point, &mut secret)
                else {
                    return self.fail("the server's P-256 point is not usable");
                };
                self.pre_master[..n].copy_from_slice(&secret[..n]);
                self.pre_master_len = n;
            }
            _ => return self.fail("the server chose a group this client did not offer"),
        }
        true
    }

    /// Everything the Finished messages are computed over, once the key
    /// exchange is done: the master secret and the block of keys from it.
    pub fn derive_keys(&mut self) -> bool {
        if self.pre_master_len == 0 {
            return self.fail("no key exchange happened");
        }
        let seed = seed_of(&self.client_random, &self.server_random);
        prf(&self.pre_master[..self.pre_master_len], b"master secret", &seed,
            &mut self.master);

        let mut expand = [0u8; 64];
        expand[..32].copy_from_slice(&self.server_random);
        expand[32..].copy_from_slice(&self.client_random);
        // client_write_key, server_write_key, client_write_IV, server_write_IV.
        // No MAC keys: GCM authenticates the record itself.
        let mut block = [0u8; 40];
        prf(&self.master, b"key expansion", &expand, &mut block);
        self.client_key.copy_from_slice(&block[0..16]);
        self.server_key.copy_from_slice(&block[16..32]);
        self.client_iv.copy_from_slice(&block[32..36]);
        self.server_iv.copy_from_slice(&block[36..40]);
        true
    }

    /// The 12 bytes a Finished message carries.
    ///
    /// `transcript` is the hash of every handshake message so far — the
    /// Finished itself is not included, and the label says which direction
    /// this is, so the two Finished messages are not the same value.
    pub fn finished_verify_data(&self, label: &[u8], transcript: &[u8; 32],
                                out: &mut [u8; 12]) {
        let mut full = [0u8; 12];
        prf(&self.master, label, transcript, &mut full);
        out.copy_from_slice(&full);
    }
}

// ---------------------------------------------------------------------------
//  The signature on ServerKeyExchange
// ---------------------------------------------------------------------------

/// Check the server's signature over the key exchange.
///
/// TLS 1.2's own RSA scheme is PKCS#1 v1.5 — the DigestInfo form, and the
/// padding the Bleichenbacher attack targets in TLS 1.0/1.1, which is why the
/// whole structure is compared rather than searched for a digest.  But PSS is
/// also valid here: RFC 8446 gives it numbers in the same signature-scheme
/// space and says a client that offers them must expect a 1.2 server to use
/// one.  The padding is the scheme's, not the version's, so it is chosen from
/// the scheme — the two are not interchangeable and a verifier that guesses
/// fails on exactly the servers that pick the newer one.
fn verify_handshake_signature(signed: &[u8], sig: &[u8], scheme: u16,
                              key_alg: &[u8], curve: &[u8], key: &[u8]) -> bool {
    use crate::crypto::der::{self, Reader, TAG_INTEGER, TAG_SEQUENCE};
    use crate::crypto::rsa::{HashId, RsaPublic};
    use crate::crypto::x509;

    // RSA-PSS, then the PKCS#1 schemes, then ECDSA.
    const PSS_SHA256: u16 = 0x0804;
    const PSS_SHA384: u16 = 0x0805;
    const PSS_SHA512: u16 = 0x0806;

    let (pss, hash) = match scheme {
        PSS_SHA256 => (true, HashId::Sha256),
        PSS_SHA384 => (true, HashId::Sha384),
        PSS_SHA512 => (true, HashId::Sha512),
        SIG_RSA_PKCS1_SHA256 => (false, HashId::Sha256),
        SIG_RSA_PKCS1_SHA384 => (false, HashId::Sha384),
        SIG_RSA_PKCS1_SHA512 => (false, HashId::Sha512),
        SIG_ECDSA_SHA256 => (false, HashId::Sha256),
        SIG_ECDSA_SHA384 => (false, HashId::Sha384),
        SIG_ECDSA_SHA512 => (false, HashId::Sha512),
        // SHA-1 is not implemented and is not offered, so a server using it
        // has ignored the extension rather than negotiated with it.
        _ => return false,
    };
    let is_ecdsa = matches!(scheme, SIG_ECDSA_SHA256 | SIG_ECDSA_SHA384 | SIG_ECDSA_SHA512);

    let mut digest = [0u8; 64];
    let dn = hash.hash(signed, &mut digest);

    if der::oid_is(key_alg, x509::OID_RSA) {
        if is_ecdsa {
            return false;               // the scheme and the key disagree
        }
        let Some(k) = der::parse_one(key) else { return false };
        let mut kr = Reader::new(k.body);
        let (Some(nt), Some(et)) = (kr.expect(TAG_INTEGER), kr.expect(TAG_INTEGER)) else {
            return false;
        };
        let (Some(n), Some(e)) = (der::int_bytes(&nt), der::int_bytes(&et)) else {
            return false;
        };
        let Some(pubkey) = RsaPublic::new(n, e) else { return false };
        if pss {
            pubkey.verify_pss(sig, hash, &digest[..dn])
        } else {
            pubkey.verify_pkcs1_v15(sig, hash, &digest[..dn])
        }
    } else if der::oid_is(key_alg, x509::OID_EC) {
        if pss || matches!(scheme, SIG_RSA_PKCS1_SHA256 | SIG_RSA_PKCS1_SHA384 | SIG_RSA_PKCS1_SHA512) {
            return false;
        }
        let c = if der::oid_is(curve, x509::OID_P256) {
            Ec::new(&ec::P256)
        } else if der::oid_is(curve, x509::OID_P384) {
            Ec::new(&ec::P384)
        } else {
            None
        };
        let Some(c) = c else { return false };
        let Some(s) = der::parse_one(sig) else { return false };
        if s.tag != TAG_SEQUENCE {
            return false;
        }
        let mut sr = Reader::new(s.body);
        let (Some(rt), Some(st)) = (sr.expect(TAG_INTEGER), sr.expect(TAG_INTEGER)) else {
            return false;
        };
        let (Some(r), Some(sv)) = (der::int_bytes(&rt), der::int_bytes(&st)) else {
            return false;
        };
        let w = c.order_len();
        if r.len() > w || sv.len() > w {
            return false;
        }
        let mut rp = [0u8; 64];
        let mut sp = [0u8; 64];
        rp[w - r.len()..w].copy_from_slice(r);
        sp[w - sv.len()..w].copy_from_slice(sv);
        c.verify(key, &rp[..w], &sp[..w], &digest[..dn])
    } else {
        false
    }
}

// ---------------------------------------------------------------------------
//  Building our own messages
// ---------------------------------------------------------------------------

/// An ephemeral key pair for a group.  P-256 by default: a server that will
/// not do X25519 is exactly the server this module exists for, and the group
/// offered first in `supported_groups` is the one a 1.3 server takes — so the
/// order cannot simply be changed for 1.2's benefit.
pub fn generate_keypair(group: u16, random: &mut dyn FnMut(&mut [u8])) -> Option<(usize, [u8; 32], usize, [u8; 65])> {
    match group {
        GROUP_X25519 => {
            let mut private = [0u8; 32];
            random(&mut private);
            let public = x25519::public_key(&private);
            let mut out = [0u8; 65];
            out[..32].copy_from_slice(&public);
            Some((32, private, 32, out))
        }
        GROUP_SECP256R1 => {
            let p256 = Ec::new(&ec::P256)?;
            // A scalar in [1, n-1].  `public_from_scalar` refuses anything
            // else, so this loops rather than reducing: reducing a random
            // value modulo the order is not a uniform draw.
            for _ in 0..8 {
                let mut private = [0u8; 32];
                random(&mut private);
                let mut public = [0u8; 65];
                if let Some(n) = p256.public_from_scalar(&private, &mut public) {
                    return Some((32, private, n, public));
                }
            }
            None
        }
        _ => None,
    }
}

/// ClientKeyExchange: our ephemeral public value.
pub fn client_key_exchange(body: &mut [u8], group: u16, public: &[u8]) -> usize {
    let _ = group;
    if body.is_empty() || 1 + public.len() > body.len() {
        return 0;
    }
    body[0] = public.len() as u8;
    body[1..1 + public.len()].copy_from_slice(public);
    1 + public.len()
}

/// Wrap a handshake message in its four-byte header.
pub fn wrap(kind: u8, body: &[u8], out: &mut [u8]) -> Option<usize> {
    let n = 4 + body.len();
    if out.len() < n {
        return None;
    }
    out[0] = kind;
    out[1] = (body.len() >> 16) as u8;
    out[2] = (body.len() >> 8) as u8;
    out[3] = body.len() as u8;
    out[4..n].copy_from_slice(body);
    Some(n)
}

/// SHA-256 of a handshake transcript, cloned so the running hash survives.
pub fn transcript_hash(transcript: &Sha256, extra: Option<&[u8]>) -> [u8; 32] {
    let mut h = transcript.clone();
    if let Some(e) = extra {
        h.update(e);
    }
    h.finish()
}

/// One message type this module cares about.
pub fn is_ours(kind: u8) -> bool {
    matches!(kind, HS_SERVER_HELLO | HS_CERTIFICATE | HS_SERVER_KEY_EXCHANGE
                  | HS_CERTIFICATE_REQUEST | HS_SERVER_HELLO_DONE | HS_FINISHED)
}

pub fn selftest() -> usize {
    use crate::crypto::check;
    use crate::crypto::vectors::TLS12_PRF;

    let mut f = 0usize;
    for v in TLS12_PRF {
        let mut sbuf = [0u8; 48];
        let mut seed = [0u8; 64];
        let mut want = [0u8; 96];
        let sn = crate::crypto::unhex(v.secret, &mut sbuf);
        let dn = crate::crypto::unhex(v.seed, &mut seed);
        let wn = crate::crypto::unhex(v.want, &mut want);
        if sn == 0 || dn == 0 || wn == 0 {
            f += check("tls 1.2 prf vectors decode", &[], &[1]);
            continue;
        }
        let mut got = [0u8; 96];
        prf(&sbuf[..sn], v.label.as_bytes(), &seed[..dn], &mut got[..wn]);
        f += check(v.name, &got[..wn], &want[..wn]);
    }
    f
}
