//! AES (FIPS 197) and AES-GCM (NIST SP 800-38D).
//!
//! Both TLS 1.3 and the modern half of TLS 1.2 encrypt records with AES-GCM,
//! so this is the one cipher the browser actually needs.  It is included in
//! `crypto::selftest`, which checks it against the published vectors rather
//! than against itself — a cipher that is subtly wrong still round-trips.
//!
//! The GHASH multiply is the bit-at-a-time definition rather than a table
//! lookup.  Tables indexed by secret data are a cache-timing side channel, and
//! at 128 iterations per block this is fast enough for a page of text.

/// AES substitution box.
const SBOX: [u8; 256] = [
    0x63, 0x7c, 0x77, 0x7b, 0xf2, 0x6b, 0x6f, 0xc5, 0x30, 0x01, 0x67, 0x2b, 0xfe, 0xd7, 0xab, 0x76,
    0xca, 0x82, 0xc9, 0x7d, 0xfa, 0x59, 0x47, 0xf0, 0xad, 0xd4, 0xa2, 0xaf, 0x9c, 0xa4, 0x72, 0xc0,
    0xb7, 0xfd, 0x93, 0x26, 0x36, 0x3f, 0xf7, 0xcc, 0x34, 0xa5, 0xe5, 0xf1, 0x71, 0xd8, 0x31, 0x15,
    0x04, 0xc7, 0x23, 0xc3, 0x18, 0x96, 0x05, 0x9a, 0x07, 0x12, 0x80, 0xe2, 0xeb, 0x27, 0xb2, 0x75,
    0x09, 0x83, 0x2c, 0x1a, 0x1b, 0x6e, 0x5a, 0xa0, 0x52, 0x3b, 0xd6, 0xb3, 0x29, 0xe3, 0x2f, 0x84,
    0x53, 0xd1, 0x00, 0xed, 0x20, 0xfc, 0xb1, 0x5b, 0x6a, 0xcb, 0xbe, 0x39, 0x4a, 0x4c, 0x58, 0xcf,
    0xd0, 0xef, 0xaa, 0xfb, 0x43, 0x4d, 0x33, 0x85, 0x45, 0xf9, 0x02, 0x7f, 0x50, 0x3c, 0x9f, 0xa8,
    0x51, 0xa3, 0x40, 0x8f, 0x92, 0x9d, 0x38, 0xf5, 0xbc, 0xb6, 0xda, 0x21, 0x10, 0xff, 0xf3, 0xd2,
    0xcd, 0x0c, 0x13, 0xec, 0x5f, 0x97, 0x44, 0x17, 0xc4, 0xa7, 0x7e, 0x3d, 0x64, 0x5d, 0x19, 0x73,
    0x60, 0x81, 0x4f, 0xdc, 0x22, 0x2a, 0x90, 0x88, 0x46, 0xee, 0xb8, 0x14, 0xde, 0x5e, 0x0b, 0xdb,
    0xe0, 0x32, 0x3a, 0x0a, 0x49, 0x06, 0x24, 0x5c, 0xc2, 0xd3, 0xac, 0x62, 0x91, 0x95, 0xe4, 0x79,
    0xe7, 0xc8, 0x37, 0x6d, 0x8d, 0xd5, 0x4e, 0xa9, 0x6c, 0x56, 0xf4, 0xea, 0x65, 0x7a, 0xae, 0x08,
    0xba, 0x78, 0x25, 0x2e, 0x1c, 0xa6, 0xb4, 0xc6, 0xe8, 0xdd, 0x74, 0x1f, 0x4b, 0xbd, 0x8b, 0x8a,
    0x70, 0x3e, 0xb5, 0x66, 0x48, 0x03, 0xf6, 0x0e, 0x61, 0x35, 0x57, 0xb9, 0x86, 0xc1, 0x1d, 0x9e,
    0xe1, 0xf8, 0x98, 0x11, 0x69, 0xd9, 0x8e, 0x94, 0x9b, 0x1e, 0x87, 0xe9, 0xce, 0x55, 0x28, 0xdf,
    0x8c, 0xa1, 0x89, 0x0d, 0xbf, 0xe6, 0x42, 0x68, 0x41, 0x99, 0x2d, 0x0f, 0xb0, 0x54, 0xbb, 0x16,
];

/// Round constants for key expansion.
const RCON: [u8; 11] = [
    0x00, 0x01, 0x02, 0x04, 0x08, 0x10, 0x20, 0x40, 0x80, 0x1b, 0x36,
];

/// An AES key schedule.  16- and 32-byte keys only — the two TLS uses.
pub struct Aes {
    /// Round keys, one 16-byte block each: Nr + 1 of them.
    rk: [[u8; 16]; 15],
    rounds: usize,
}

impl Aes {
    /// Expand a 16- or 32-byte key.  Returns None for any other length.
    pub fn new(key: &[u8]) -> Option<Self> {
        let nk = match key.len() {
            16 => 4,
            32 => 8,
            _ => return None,
        };
        let rounds = nk + 6;
        let total_words = 4 * (rounds + 1);

        let mut w = [0u8; 240];             // 60 words, big-endian
        w[..key.len()].copy_from_slice(key);

        for i in nk..total_words {
            let mut temp = [w[(i - 1) * 4], w[(i - 1) * 4 + 1], w[(i - 1) * 4 + 2], w[(i - 1) * 4 + 3]];
            if i % nk == 0 {
                temp = [temp[1], temp[2], temp[3], temp[0]];      // RotWord
                for b in temp.iter_mut() {
                    *b = SBOX[*b as usize];                       // SubWord
                }
                temp[0] ^= RCON[i / nk];
            } else if nk > 6 && i % nk == 4 {
                for b in temp.iter_mut() {
                    *b = SBOX[*b as usize];
                }
            }
            for k in 0..4 {
                w[i * 4 + k] = w[(i - nk) * 4 + k] ^ temp[k];
            }
        }

        let mut rk = [[0u8; 16]; 15];
        for r in 0..=rounds {
            rk[r].copy_from_slice(&w[r * 16..r * 16 + 16]);
        }
        Some(Self { rk, rounds })
    }

    /// Encrypt one 16-byte block in place.
    pub fn encrypt_block(&self, block: &mut [u8; 16]) {
        add_round_key(block, &self.rk[0]);
        for r in 1..self.rounds {
            sub_bytes(block);
            shift_rows(block);
            mix_columns(block);
            add_round_key(block, &self.rk[r]);
        }
        sub_bytes(block);
        shift_rows(block);
        add_round_key(block, &self.rk[self.rounds]);
    }
}

/// The state is column-major: byte `i` is row `i % 4`, column `i / 4`.
fn add_round_key(s: &mut [u8; 16], rk: &[u8; 16]) {
    for i in 0..16 {
        s[i] ^= rk[i];
    }
}

fn sub_bytes(s: &mut [u8; 16]) {
    for b in s.iter_mut() {
        *b = SBOX[*b as usize];
    }
}

fn shift_rows(s: &mut [u8; 16]) {
    let t = *s;
    for c in 0..4 {
        for r in 0..4 {
            // Row r rotates left by r, which pulls from column (c + r) % 4.
            s[4 * c + r] = t[4 * ((c + r) % 4) + r];
        }
    }
}

fn xtime(x: u8) -> u8 {
    (x << 1) ^ if x & 0x80 != 0 { 0x1b } else { 0 }
}

fn mix_columns(s: &mut [u8; 16]) {
    for c in 0..4 {
        let i = 4 * c;
        let (a0, a1, a2, a3) = (s[i], s[i + 1], s[i + 2], s[i + 3]);
        let all = a0 ^ a1 ^ a2 ^ a3;
        s[i] ^= all ^ xtime(a0 ^ a1);
        s[i + 1] ^= all ^ xtime(a1 ^ a2);
        s[i + 2] ^= all ^ xtime(a2 ^ a3);
        s[i + 3] ^= all ^ xtime(a3 ^ a0);
    }
}

// ---------------------------------------------------------------------------
//  GCM
// ---------------------------------------------------------------------------

/// Multiplication in GF(2^128) as GCM defines it: the right shift, and the
/// reduction polynomial x^128 + x^7 + x^2 + x + 1.
fn gf_mul(x: &mut [u8; 16], y: &[u8; 16]) {
    let mut z = [0u8; 16];
    let mut v = *y;
    for i in 0..128 {
        // Bit i of x, most significant bit of byte 0 first.
        if x[i / 8] & (0x80 >> (i % 8)) != 0 {
            for k in 0..16 {
                z[k] ^= v[k];
            }
        }
        let lsb = v[15] & 1;
        // v >>= 1
        for k in (1..16).rev() {
            v[k] = (v[k] >> 1) | ((v[k - 1] & 1) << 7);
        }
        v[0] >>= 1;
        if lsb != 0 {
            v[0] ^= 0xe1;
        }
    }
    *x = z;
}

/// GHASH over an already-padded input: every 16 bytes is one block.
fn ghash(h: &[u8; 16], data: &[u8], y: &mut [u8; 16]) {
    let mut i = 0;
    while i + 16 <= data.len() {
        for k in 0..16 {
            y[k] ^= data[i + k];
        }
        gf_mul(y, h);
        i += 16;
    }
}

fn pad_to_block(out: &mut [u8], len: &mut usize) {
    while *len % 16 != 0 {
        out[*len] = 0;
        *len += 1;
    }
}

/// The GCM authentication tag over a ciphertext.
///
/// Factored out because both the encrypt and the decrypt path need exactly
/// this, and two copies of a tag computation is two chances to get it wrong.
fn gcm_tag(key: &[u8], iv: &[u8], aad: &[u8], ct: &[u8]) -> Option<[u8; 16]> {
    if iv.len() != 12 {
        return None;
    }
    let aes = Aes::new(key)?;

    // H = E_K(0^128)
    let mut h = [0u8; 16];
    aes.encrypt_block(&mut h);

    // J0 = IV || 0^31 || 1, for the 96-bit IV TLS always uses.
    let mut j0 = [0u8; 16];
    j0[..12].copy_from_slice(iv);
    j0[15] = 1;

    // GHASH(A || pad || C || pad || len(A) || len(C)), each 16 bytes at a time.
    let mut y = [0u8; 16];
    let mut pad = [0u8; 16];
    ghash(&h, aad, &mut y);
    let rem = aad.len() % 16;
    if rem != 0 {
        pad[..rem].copy_from_slice(&aad[aad.len() - rem..]);
        ghash(&h, &pad, &mut y);
    }
    let crem = ct.len() % 16;
    let whole = ct.len() - crem;
    ghash(&h, &ct[..whole], &mut y);
    if crem != 0 {
        let mut tail = [0u8; 16];
        tail[..crem].copy_from_slice(&ct[whole..]);
        ghash(&h, &tail, &mut y);
    }
    let mut lens = [0u8; 16];
    lens[..8].copy_from_slice(&((aad.len() as u64) * 8).to_be_bytes());
    lens[8..].copy_from_slice(&((ct.len() as u64) * 8).to_be_bytes());
    ghash(&h, &lens, &mut y);

    // T = GHASH ^ E_K(J0)
    aes.encrypt_block(&mut j0);
    for k in 0..16 {
        y[k] ^= j0[k];
    }
    Some(y)
}

/// XOR `data` with the AES counter keystream starting at inc32(J0).
fn gctr(key: &[u8], iv: &[u8], data: &[u8], out: &mut [u8]) {
    let Some(aes) = Aes::new(key) else { return };
    let mut ctr = [0u8; 16];
    ctr[..12].copy_from_slice(iv);
    ctr[15] = 1;
    inc32(&mut ctr);
    for (i, chunk) in data.chunks(16).enumerate() {
        let mut ks = ctr;
        aes.encrypt_block(&mut ks);
        for (k, &b) in chunk.iter().enumerate() {
            out[i * 16 + k] = b ^ ks[k];
        }
        inc32(&mut ctr);
    }
}

/// AES-GCM authenticated encryption.
///
/// `out` receives `data.len()` bytes of ciphertext followed by the 16-byte
/// tag.  Returns false if the key length is wrong or the buffer is too small.
pub fn gcm_encrypt(key: &[u8], iv: &[u8], aad: &[u8], data: &[u8], out: &mut [u8]) -> bool {
    if out.len() < data.len() + 16 || iv.len() != 12 || Aes::new(key).is_none() {
        return false;
    }
    gctr(key, iv, data, out);
    let Some(tag) = gcm_tag(key, iv, aad, &out[..data.len()]) else {
        return false;
    };
    out[data.len()..data.len() + 16].copy_from_slice(&tag);
    true
}

/// AES-GCM authenticated decryption.
///
/// The tag is verified before anything is written to `out`, and the comparison
/// does not return early: a tag check that stops at the first wrong byte tells
/// an attacker how much of a forged tag was right.
pub fn gcm_decrypt(key: &[u8], iv: &[u8], aad: &[u8], data: &[u8], out: &mut [u8]) -> Option<usize> {
    if data.len() < 16 || out.len() < data.len() - 16 || iv.len() != 12 {
        return None;
    }
    let ct = &data[..data.len() - 16];
    let tag = &data[data.len() - 16..];

    let expect = gcm_tag(key, iv, aad, ct)?;
    let mut diff = 0u8;
    for k in 0..16 {
        diff |= expect[k] ^ tag[k];
    }
    if diff != 0 {
        return None;
    }

    gctr(key, iv, ct, out);
    Some(ct.len())
}

fn inc32(block: &mut [u8; 16]) {
    for i in (12..16).rev() {
        block[i] = block[i].wrapping_add(1);
        if block[i] != 0 {
            break;
        }
    }
}

// ---------------------------------------------------------------------------
//  Test vectors
// ---------------------------------------------------------------------------

/// FIPS 197 Appendix C and the NIST GCM test cases, checked at boot.
///
/// These are the published answers, not this code's own output.  A round trip
/// would pass with a wrong S-box, a wrong reduction polynomial or a transposed
/// state; only comparing against the standard catches those.
pub fn selftest() -> usize {
    use crate::crypto::{check, unhex, unhex64};
    let mut f = 0usize;

    // FIPS 197 C.1 — AES-128, one block.
    {
        let mut key = [0u8; 16];
        unhex(b"000102030405060708090a0b0c0d0e0f", &mut key);
        let mut b16 = [0u8; 16];
        b16.copy_from_slice(&unhex64(b"00112233445566778899aabbccddeeff")[..16]);
        let mut want = [0u8; 16];
        unhex(b"69c4e0d86a7b0430d8cdb78070b4c55a", &mut want);
        Aes::new(&key).unwrap().encrypt_block(&mut b16);
        f += check("aes128 fips197", &b16, &want);
    }

    // FIPS 197 C.3 — AES-256, one block.
    {
        let mut key = [0u8; 32];
        unhex(b"000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f", &mut key);
        let mut b16 = [0u8; 16];
        b16.copy_from_slice(&unhex64(b"00112233445566778899aabbccddeeff")[..16]);
        let mut want = [0u8; 16];
        unhex(b"8ea2b7ca516745bfeafc49904b496089", &mut want);
        Aes::new(&key).unwrap().encrypt_block(&mut b16);
        f += check("aes256 fips197", &b16, &want);
    }

    // NIST GCM test case 2 — all-zero key and IV, one zero block.
    {
        let key = [0u8; 16];
        let iv = [0u8; 12];
        let pt = [0u8; 16];
        let mut out = [0u8; 32];
        let mut want = [0u8; 16];
        unhex(b"0388dace60b6a392f328c2b971b2fe78", &mut want);
        let mut wantt = [0u8; 16];
        unhex(b"ab6e47d42cec13bdf53a67b21257bddf", &mut wantt);
        if gcm_encrypt(&key, &iv, &[], &pt, &mut out) {
            f += check("gcm case2 ciphertext", &out[..16], &want);
            f += check("gcm case2 tag", &out[16..], &wantt);
        } else {
            f += check("gcm case2 encrypt", &[], &[1]);
        }
    }

    // NIST GCM test case 4 — 64 bytes of plaintext and 20 bytes of AAD, so
    // both the padding paths and the length block are exercised.
    {
        let mut key = [0u8; 16];
        unhex(b"feffe9928665731c6d6a8f9467308308", &mut key);
        let mut iv = [0u8; 12];
        unhex(b"cafebabefacedbaddecaf888", &mut iv);
        let mut pt = [0u8; 60];
        let n = unhex(b"d9313225f88406e5a55909c5aff5269a86a7a9531534f7da2e4c303d8a318a721c3c0c95956809532fcf0e2449a6b525b16aedf5aa0de657ba637b39", &mut pt);
        let mut aad = [0u8; 20];
        unhex(b"feedfacedeadbeeffeedfacedeadbeefabaddad2", &mut aad);
        let mut want = [0u8; 60];
        unhex(b"42831ec2217774244b7221b784d0d49ce3aa212f2c02a4e035c17e2329aca12e21d514b25466931c7d8f6a5aac84aa051ba30b396a0aac973d58e091", &mut want);
        let mut wantt = [0u8; 16];
        unhex(b"5bc94fbc3221a5db94fae95ae7121a47", &mut wantt);

        let mut out = [0u8; 96];
        if gcm_encrypt(&key, &iv, &aad, &pt[..n], &mut out) {
            f += check("gcm case4 ciphertext", &out[..n], &want[..n]);
            f += check("gcm case4 tag", &out[n..n + 16], &wantt);

            // And back again, with a flipped tag bit that must be rejected.
            let mut back = [0u8; 64];
            match gcm_decrypt(&key, &iv, &aad, &out[..n + 16], &mut back) {
                Some(m) => f += check("gcm case4 decrypt", &back[..m], &pt[..n]),
                None => f += check("gcm case4 decrypt", &[], &[1]),
            }
            let mut tampered = [0u8; 96];
            tampered[..n + 16].copy_from_slice(&out[..n + 16]);
            tampered[n] ^= 0x01;
            if gcm_decrypt(&key, &iv, &aad, &tampered[..n + 16], &mut back).is_some() {
                f += check("gcm rejects a bad tag", &[], &[1]);
            } else {
                f += check("gcm rejects a bad tag", &[0], &[0]);
            }
        } else {
            f += check("gcm case4 encrypt", &[], &[1]);
        }
    }

    f
}
