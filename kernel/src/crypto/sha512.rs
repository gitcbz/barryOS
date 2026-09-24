//! SHA-512 and SHA-384 (FIPS 180-4).
//!
//! Certificate chains use whatever hash the issuer chose, and the roots in
//! particular are often SHA-384.  A verifier that only knows SHA-256 cannot
//! check them, and "cannot check" has to mean "reject", so the two extra
//! functions here are what let a real chain be verified rather than a subset
//! of one.
//!
//! SHA-384 is SHA-512 with a different initial state, a different truncation
//! and no room for the length in 128 bits; the compression function is shared.

/// Round constants: the first 64 bits of the fractional parts of the cube
/// roots of the first 80 primes.
const K: [u64; 80] = [
    0x428a2f98d728ae22, 0x7137449123ef65cd, 0xb5c0fbcfec4d3b2f, 0xe9b5dba58189dbbc,
    0x3956c25bf348b538, 0x59f111f1b605d019, 0x923f82a4af194f9b, 0xab1c5ed5da6d8118,
    0xd807aa98a3030242, 0x12835b0145706fbe, 0x243185be4ee4b28c, 0x550c7dc3d5ffb4e2,
    0x72be5d74f27b896f, 0x80deb1fe3b1696b1, 0x9bdc06a725c71235, 0xc19bf174cf692694,
    0xe49b69c19ef14ad2, 0xefbe4786384f25e3, 0x0fc19dc68b8cd5b5, 0x240ca1cc77ac9c65,
    0x2de92c6f592b0275, 0x4a7484aa6ea6e483, 0x5cb0a9dcbd41fbd4, 0x76f988da831153b5,
    0x983e5152ee66dfab, 0xa831c66d2db43210, 0xb00327c898fb213f, 0xbf597fc7beef0ee4,
    0xc6e00bf33da88fc2, 0xd5a79147930aa725, 0x06ca6351e003826f, 0x142929670a0e6e70,
    0x27b70a8546d22ffc, 0x2e1b21385c26c926, 0x4d2c6dfc5ac42aed, 0x53380d139d95b3df,
    0x650a73548baf63de, 0x766a0abb3c77b2a8, 0x81c2c92e47edaee6, 0x92722c851482353b,
    0xa2bfe8a14cf10364, 0xa81a664bbc423001, 0xc24b8b70d0f89791, 0xc76c51a30654be30,
    0xd192e819d6ef5218, 0xd69906245565a910, 0xf40e35855771202a, 0x106aa07032bbd1b8,
    0x19a4c116b8d2d0c8, 0x1e376c085141ab53, 0x2748774cdf8eeb99, 0x34b0bcb5e19b48a8,
    0x391c0cb3c5c95a63, 0x4ed8aa4ae3418acb, 0x5b9cca4f7763e373, 0x682e6ff3d6b2b8a3,
    0x748f82ee5defb2fc, 0x78a5636f43172f60, 0x84c87814a1f0ab72, 0x8cc702081a6439ec,
    0x90befffa23631e28, 0xa4506cebde82bde9, 0xbef9a3f7b2c67915, 0xc67178f2e372532b,
    0xca273eceea26619c, 0xd186b8c721c0c207, 0xeada7dd6cde0eb1e, 0xf57d4f7fee6ed178,
    0x06f067aa72176fba, 0x0a637dc5a2c898a6, 0x113f9804bef90dae, 0x1b710b35131c471b,
    0x28db77f523047d84, 0x32caab7b40c72493, 0x3c9ebe0a15c9bebc, 0x431d67c49c100d4c,
    0x4cc5d4becb3e42b6, 0x597f299cfc657e2a, 0x5fcb6fab3ad6faec, 0x6c44198c4a475817,
];

/// Initial state for SHA-512.
const H512: [u64; 8] = [
    0x6a09e667f3bcc908, 0xbb67ae8584caa73b, 0x3c6ef372fe94f82b, 0xa54ff53a5f1d36f1,
    0x510e527fade682d1, 0x9b05688c2b3e6c1f, 0x1f83d9abfb41bd6b, 0x5be0cd19137e2179,
];

/// Initial state for SHA-384: the same constants as SHA-512, but taken from
/// the square roots of the ninth through sixteenth primes.
const H384: [u64; 8] = [
    0xcbbb9d5dc1059ed8, 0x629a292a367cd507, 0x9159015a3070dd17, 0x152fecd8f70e5939,
    0x67332667ffc00b31, 0x8eb44a8768581511, 0xdb0c2e0d64f98fa7, 0x47b5481dbefa4fa4,
];

pub struct Sha512 {
    h: [u64; 8],
    buf: [u8; 128],
    buf_len: usize,
    total: u128,
    /// 384 truncates its output and starts from a different state.
    truncate: bool,
}

impl Sha512 {
    pub const fn new512() -> Self {
        Self { h: H512, buf: [0; 128], buf_len: 0, total: 0, truncate: false }
    }

    pub const fn new384() -> Self {
        Self { h: H384, buf: [0; 128], buf_len: 0, total: 0, truncate: true }
    }

    pub fn update(&mut self, mut data: &[u8]) {
        self.total = self.total.wrapping_add(data.len() as u128);
        if self.buf_len > 0 {
            let need = 128 - self.buf_len;
            let take = data.len().min(need);
            self.buf[self.buf_len..self.buf_len + take].copy_from_slice(&data[..take]);
            self.buf_len += take;
            data = &data[take..];
            if self.buf_len == 128 {
                let block = self.buf;
                self.compress(&block);
                self.buf_len = 0;
            }
        }
        while data.len() >= 128 {
            let mut block = [0u8; 128];
            block.copy_from_slice(&data[..128]);
            self.compress(&block);
            data = &data[128..];
        }
        if !data.is_empty() {
            self.buf[..data.len()].copy_from_slice(data);
            self.buf_len = data.len();
        }
    }

    fn finish_bytes(mut self, out: &mut [u8]) {
        let bit_len = self.total.wrapping_mul(8);
        // Padding: 0x80, zeroes, then the 128-bit big-endian length.
        let mut pad = [0u8; 144];
        pad[0] = 0x80;
        let pad_len = if self.buf_len < 112 { 112 - self.buf_len } else { 240 - self.buf_len };
        pad[pad_len..pad_len + 16].copy_from_slice(&bit_len.to_be_bytes());
        for &b in &pad[..pad_len + 16] {
            self.buf[self.buf_len] = b;
            self.buf_len += 1;
            if self.buf_len == 128 {
                let block = self.buf;
                self.compress(&block);
                self.buf_len = 0;
            }
        }
        for (i, v) in self.h.iter().enumerate() {
            let b = v.to_be_bytes();
            for k in 0..8 {
                if i * 8 + k < out.len() {
                    out[i * 8 + k] = b[k];
                }
            }
        }
    }

    pub fn finish512(mut self) -> [u8; 64] {
        let mut out = [0u8; 64];
        self.finish_bytes(&mut out);
        out
    }

    pub fn finish384(mut self) -> [u8; 48] {
        let mut out = [0u8; 64];
        self.finish_bytes(&mut out);
        let mut t = [0u8; 48];
        t.copy_from_slice(&out[..48]);
        t
    }

    fn compress(&mut self, block: &[u8; 128]) {
        let mut w = [0u64; 80];
        for i in 0..16 {
            let mut v = [0u8; 8];
            v.copy_from_slice(&block[i * 8..i * 8 + 8]);
            w[i] = u64::from_be_bytes(v);
        }
        for i in 16..80 {
            let s0 = w[i - 15].rotate_right(1) ^ w[i - 15].rotate_right(8) ^ (w[i - 15] >> 7);
            let s1 = w[i - 2].rotate_right(19) ^ w[i - 2].rotate_right(61) ^ (w[i - 2] >> 6);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let (mut a, mut b, mut c, mut d) = (self.h[0], self.h[1], self.h[2], self.h[3]);
        let (mut e, mut f, mut g, mut h) = (self.h[4], self.h[5], self.h[6], self.h[7]);

        for i in 0..80 {
            let s1 = e.rotate_right(14) ^ e.rotate_right(18) ^ e.rotate_right(41);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(28) ^ a.rotate_right(34) ^ a.rotate_right(39);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);

            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }

        self.h[0] = self.h[0].wrapping_add(a);
        self.h[1] = self.h[1].wrapping_add(b);
        self.h[2] = self.h[2].wrapping_add(c);
        self.h[3] = self.h[3].wrapping_add(d);
        self.h[4] = self.h[4].wrapping_add(e);
        self.h[5] = self.h[5].wrapping_add(f);
        self.h[6] = self.h[6].wrapping_add(g);
        self.h[7] = self.h[7].wrapping_add(h);
    }
}

/// One-shot SHA-512.
pub fn hash512(data: &[u8]) -> [u8; 64] {
    let mut s = Sha512::new512();
    s.update(data);
    s.finish512()
}

/// One-shot SHA-384.
pub fn hash384(data: &[u8]) -> [u8; 48] {
    let mut s = Sha512::new384();
    s.update(data);
    s.finish384()
}

pub fn selftest() -> usize {
    use crate::crypto::{check, unhex};
    let mut f = 0usize;

    // FIPS 180-4 examples: "abc".
    {
        let mut want = [0u8; 64];
        unhex(
            b"ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a\\
              2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f",
            &mut want,
        );
        f += check("sha512 abc", &hash512(b"abc"), &want);
    }
    {
        let mut want = [0u8; 48];
        unhex(
            b"cb00753f45a35e8bb5a03d699ac65007272c32ab0eded1631a8b605a43ff5bed\\
              8086072ba1e7cc2358baeca134c825a7",
            &mut want,
        );
        f += check("sha384 abc", &hash384(b"abc"), &want);
    }

    // The two-block message, so the length encoding past 64 bytes is covered.
    {
        let msg = b"abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmn\
                    hijklmnoijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu";
        let mut want = [0u8; 64];
        unhex(
            b"8e959b75dae313da8cf4f72814fc143f8f7779c6eb9f7fa17299aeadb6889018\\
              501d289e4900f7e4331b99dec4b5433ac7d329eeb6dd26545e96e55b874be909",
            &mut want,
        );
        f += check("sha512 two blocks", &hash512(msg), &want);
    }

    f
}
