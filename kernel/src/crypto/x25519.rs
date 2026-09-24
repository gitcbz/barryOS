//! X25519 (RFC 7748) — the elliptic-curve key exchange.
//!
//! Both TLS 1.3 and the ECDHE suites of TLS 1.2 use this to agree on a secret
//! in the open.  It is a Montgomery curve, so the scalar multiplication is a
//! ladder over x-coordinates only: no point addition formulae, no y, and no
//! special cases for the point at infinity to get wrong.
//!
//! The field is 2^255 - 19, held as five 51-bit limbs so that every product
//! fits in a u128 without carrying between limbs.

/// 51 bits set.
const MASK: u64 = (1 << 51) - 1;

/// 2p in limb form, for the branch-free subtraction: adding it first keeps the
/// result positive so the borrow never has to be reasoned about.
const TWO_P0: u64 = 0xFFFFFFFFFFFDA;   // 2 * (2^51 - 19)
const TWO_P1: u64 = 0xFFFFFFFFFFFFE;   // 2 * (2^51 - 1)

/// (a - 2) / 4 for the Montgomery ladder, which RFC 7748 names a24.
const A24: u64 = 121665;

type Fe = [u64; 5];

/// Reduce the limbs back into range after an addition or a multiplication.
fn carry(h: &mut Fe) {
    let mut c = h[0] >> 51;
    h[0] &= MASK;
    h[1] += c;
    c = h[1] >> 51;
    h[1] &= MASK;
    h[2] += c;
    c = h[2] >> 51;
    h[2] &= MASK;
    h[3] += c;
    c = h[3] >> 51;
    h[3] &= MASK;
    h[4] += c;
    c = h[4] >> 51;
    h[4] &= MASK;
    // The top limb wraps around into limb 0, multiplied by 19 because
    // 2^255 = 19 (mod 2^255 - 19).
    h[0] += c * 19;
}

fn add(a: &Fe, b: &Fe) -> Fe {
    let mut r = [0u64; 5];
    for i in 0..5 {
        r[i] = a[i] + b[i];
    }
    carry(&mut r);
    r
}

fn sub(a: &Fe, b: &Fe) -> Fe {
    let mut r = [0u64; 5];
    r[0] = a[0] + TWO_P0 - b[0];
    for i in 1..5 {
        r[i] = a[i] + TWO_P1 - b[i];
    }
    carry(&mut r);
    r
}

fn mul(a: &Fe, b: &Fe) -> Fe {
    // Multiples of b by 19, for the terms that wrap around the top limb.
    let b1 = b[1] * 19;
    let b2 = b[2] * 19;
    let b3 = b[3] * 19;
    let b4 = b[4] * 19;

    let m = |x: u64, y: u64| -> u128 { (x as u128) * (y as u128) };

    let t0 = m(a[0], b[0]) + m(a[1], b4) + m(a[2], b3) + m(a[3], b2) + m(a[4], b1);
    let t1 = m(a[0], b[1]) + m(a[1], b[0]) + m(a[2], b4) + m(a[3], b3) + m(a[4], b2);
    let t2 = m(a[0], b[2]) + m(a[1], b[1]) + m(a[2], b[0]) + m(a[3], b4) + m(a[4], b3);
    let t3 = m(a[0], b[3]) + m(a[1], b[2]) + m(a[2], b[1]) + m(a[3], b[0]) + m(a[4], b4);
    let t4 = m(a[0], b[4]) + m(a[1], b[3]) + m(a[2], b[2]) + m(a[3], b[1]) + m(a[4], b[0]);

    let mut r = [0u64; 5];
    let mut c: u128;
    c = t0 >> 51;
    r[0] = (t0 as u64) & MASK;
    let t1 = t1 + c;
    c = t1 >> 51;
    r[1] = (t1 as u64) & MASK;
    let t2 = t2 + c;
    c = t2 >> 51;
    r[2] = (t2 as u64) & MASK;
    let t3 = t3 + c;
    c = t3 >> 51;
    r[3] = (t3 as u64) & MASK;
    let t4 = t4 + c;
    c = t4 >> 51;
    r[4] = (t4 as u64) & MASK;
    r[0] += (c as u64) * 19;

    carry(&mut r);
    r
}

fn sq(a: &Fe) -> Fe {
    mul(a, a)
}

/// Multiply by a small constant (the ladder's a24, and the final squeeze).
fn mul_small(a: &Fe, k: u64) -> Fe {
    let mut r = [0u64; 5];
    let mut c: u128 = 0;
    for i in 0..5 {
        let t = (a[i] as u128) * (k as u128) + c;
        r[i] = (t as u64) & MASK;
        c = t >> 51;
    }
    r[0] += (c as u64) * 19;
    carry(&mut r);
    r
}

/// z^(2^255 - 21), which is z^-1 because p - 2 = 2^255 - 21.
///
/// The addition chain is the one from RFC 7748; written as a loop it would be
/// 255 squarings and 254 multiplications instead of 254 and 11.
fn invert(z: &Fe) -> Fe {
    let z2 = sq(z);
    let z8 = sq(&sq(&z2));
    let z9 = mul(&z8, z);
    let z11 = mul(&z9, &z2);
    let z22 = sq(&z11);
    let z_5_0 = mul(&z22, &z9);

    let mut t = sq(&z_5_0);
    for _ in 1..5 {
        t = sq(&t);
    }
    let z_10_0 = mul(&t, &z_5_0);

    let mut t = sq(&z_10_0);
    for _ in 1..10 {
        t = sq(&t);
    }
    let z_20_0 = mul(&t, &z_10_0);

    let mut t = sq(&z_20_0);
    for _ in 1..20 {
        t = sq(&t);
    }
    let z_40_0 = mul(&t, &z_20_0);

    let mut t = sq(&z_40_0);
    for _ in 1..10 {
        t = sq(&t);
    }
    let z_50_0 = mul(&t, &z_10_0);

    let mut t = sq(&z_50_0);
    for _ in 1..50 {
        t = sq(&t);
    }
    let z_100_0 = mul(&t, &z_50_0);

    let mut t = sq(&z_100_0);
    for _ in 1..100 {
        t = sq(&t);
    }
    let z_200_0 = mul(&t, &z_100_0);

    let mut t = sq(&z_200_0);
    for _ in 1..50 {
        t = sq(&t);
    }
    let z_250_0 = mul(&t, &z_50_0);

    let mut t = sq(&z_250_0);
    for _ in 1..5 {
        t = sq(&t);
    }
    mul(&t, &z11)
}

fn from_bytes(b: &[u8; 32]) -> Fe {
    let load = |i: usize| -> u64 {
        let mut v = [0u8; 8];
        v.copy_from_slice(&b[i..i + 8]);
        u64::from_le_bytes(v)
    };
    let mut h = [
        load(0) & MASK,
        (load(6) >> 3) & MASK,
        (load(12) >> 6) & MASK,
        (load(19) >> 1) & MASK,
        (load(24) >> 12) & MASK,
    ];
    // The top bit of the last byte is not part of the coordinate.
    h[4] &= MASK;
    h
}

fn to_bytes(h: &Fe) -> [u8; 32] {
    let mut t = *h;
    carry(&mut t);
    carry(&mut t);

    // Now reduce mod p.  Adding 19 and looking at the carry out of bit 255
    // tells us whether the value was at least p; if it was, 19*q subtracts it.
    let mut q = (t[0] + 19) >> 51;
    q = (t[1] + q) >> 51;
    q = (t[2] + q) >> 51;
    q = (t[3] + q) >> 51;
    q = (t[4] + q) >> 51;
    t[0] += 19 * q;

    // Carry once more, but this time let the top limb's overflow fall off:
    // it is exactly the p that was just subtracted.
    t[1] += t[0] >> 51;
    t[0] &= MASK;
    t[2] += t[1] >> 51;
    t[1] &= MASK;
    t[3] += t[2] >> 51;
    t[2] &= MASK;
    t[4] += t[3] >> 51;
    t[3] &= MASK;
    t[4] &= MASK;

    // Pack the five 51-bit limbs little-endian.  Written byte by byte rather
    // than by shifting limbs into u64s, because 51 is not a multiple of 8 and
    // the shifts overflow.
    let (h0, h1, h2, h3, h4) = (t[0], t[1], t[2], t[3], t[4]);
    let mut s = [0u8; 32];
    s[0] = h0 as u8;
    s[1] = (h0 >> 8) as u8;
    s[2] = (h0 >> 16) as u8;
    s[3] = (h0 >> 24) as u8;
    s[4] = (h0 >> 32) as u8;
    s[5] = (h0 >> 40) as u8;
    s[6] = ((h0 >> 48) | (h1 << 3)) as u8;
    s[7] = (h1 >> 5) as u8;
    s[8] = (h1 >> 13) as u8;
    s[9] = (h1 >> 21) as u8;
    s[10] = (h1 >> 29) as u8;
    s[11] = (h1 >> 37) as u8;
    s[12] = ((h1 >> 45) | (h2 << 6)) as u8;
    s[13] = (h2 >> 2) as u8;
    s[14] = (h2 >> 10) as u8;
    s[15] = (h2 >> 18) as u8;
    s[16] = (h2 >> 26) as u8;
    s[17] = (h2 >> 34) as u8;
    s[18] = (h2 >> 42) as u8;
    s[19] = ((h2 >> 50) | (h3 << 1)) as u8;
    s[20] = (h3 >> 7) as u8;
    s[21] = (h3 >> 15) as u8;
    s[22] = (h3 >> 23) as u8;
    s[23] = (h3 >> 31) as u8;
    s[24] = (h3 >> 39) as u8;
    s[25] = ((h3 >> 47) | (h4 << 4)) as u8;
    s[26] = (h4 >> 4) as u8;
    s[27] = (h4 >> 12) as u8;
    s[28] = (h4 >> 20) as u8;
    s[29] = (h4 >> 28) as u8;
    s[30] = (h4 >> 36) as u8;
    s[31] = (h4 >> 44) as u8;
    s
}

/// Clamp a scalar the way RFC 7748 requires.
fn clamp(k: &mut [u8; 32]) {
    k[0] &= 248;
    k[31] &= 127;
    k[31] |= 64;
}

/// The raw X25519 function: `scalar` times the point with x-coordinate `u`.
pub fn scalarmult(scalar: &[u8; 32], u: &[u8; 32]) -> [u8; 32] {
    let mut k = *scalar;
    clamp(&mut k);

    let x1 = from_bytes(u);
    let mut x2: Fe = [1, 0, 0, 0, 0];
    let mut z2: Fe = [0; 5];
    let mut x3 = x1;
    let mut z3: Fe = [1, 0, 0, 0, 0];
    let mut swap = 0u64;

    for t in (0..255).rev() {
        let kt = ((k[t / 8] >> (t % 8)) & 1) as u64;
        swap ^= kt;
        // Conditional swap, branch-free so the timing does not depend on the
        // secret bits.
        cswap(&mut x2, &mut x3, swap);
        cswap(&mut z2, &mut z3, swap);
        swap = kt;

        let a = add(&x2, &z2);
        let aa = sq(&a);
        let b = sub(&x2, &z2);
        let bb = sq(&b);
        let e = sub(&aa, &bb);
        let c = add(&x3, &z3);
        let d = sub(&x3, &z3);
        let da = mul(&d, &a);
        let cb = mul(&c, &b);
        x3 = sq(&add(&da, &cb));
        z3 = mul(&x1, &sq(&sub(&da, &cb)));
        x2 = mul(&aa, &bb);
        z2 = mul(&e, &add(&aa, &mul_small(&e, A24)));
    }
    cswap(&mut x2, &mut x3, swap);
    cswap(&mut z2, &mut z3, swap);

    to_bytes(&mul(&x2, &invert(&z2)))
}

fn cswap(a: &mut Fe, b: &mut Fe, swap: u64) {
    let mask = 0u64.wrapping_sub(swap);
    for i in 0..5 {
        let t = mask & (a[i] ^ b[i]);
        a[i] ^= t;
        b[i] ^= t;
    }
}

/// The base point: u = 9.
pub const BASE: [u8; 32] = [
    9, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
];

/// A public key from a private one.
pub fn public_key(secret: &[u8; 32]) -> [u8; 32] {
    scalarmult(secret, &BASE)
}

pub fn selftest() -> usize {
    use crate::crypto::{check, unhex};
    let mut f = 0usize;

    // RFC 7748 §5.2, first vector: an arbitrary scalar and u-coordinate.
    {
        let mut k = [0u8; 32];
        unhex(b"a546e36bf0527c9d3b16154b82465edd62144c0ac1fc5a18506a2244ba449ac4", &mut k);
        let mut u = [0u8; 32];
        unhex(b"e6db6867583030db3594c1a424b15f7c726624ec26b3353b10a903a6d0ab1c4c", &mut u);
        let mut want = [0u8; 32];
        unhex(b"c3da55379de9c6908e94ea4df28d084f32eccf03491c71f754b4075577a28552", &mut want);
        f += check("x25519 rfc7748-1", &scalarmult(&k, &u), &want);
    }

    // RFC 7748 §5.2, second vector.  Two vectors rather than one because a
    // ladder with the swapped operands passes the first and fails this.
    {
        let mut k = [0u8; 32];
        unhex(b"4b66e9d4d1b4673c5ad22691957d6af5c11b6421e0ea01d42ca4169e7918ba0d", &mut k);
        let mut u = [0u8; 32];
        unhex(b"e5210f12786811d3f4b7959d0538ae2c31dbe7106fc03c3efc4cd549c715a493", &mut u);
        let mut want = [0u8; 32];
        unhex(b"95cbde9476e8907d7aade45cb4b873f88b595a68799fa152e6f8f7647aac7957", &mut want);
        f += check("x25519 rfc7748-2", &scalarmult(&k, &u), &want);
    }

    // RFC 7748 §6.1: the two sides of a real exchange agree.
    {
        let mut a = [0u8; 32];
        unhex(b"77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a", &mut a);
        let mut b = [0u8; 32];
        unhex(b"5dab087e624a8a4b79e17f8b83800ee66f3bb1292618b6fd1c2f8b27ff88e0eb", &mut b);

        let mut want_apub = [0u8; 32];
        unhex(b"8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a", &mut want_apub);
        let mut want_bpub = [0u8; 32];
        unhex(b"de9edb7d7b7dc1b4d35b61c2ece435373f8343c85b78674dadfc7e146f882b4f", &mut want_bpub);
        let apub = public_key(&a);
        let bpub = public_key(&b);
        f += check("x25519 rfc7748 a-pub", &apub, &want_apub);
        f += check("x25519 rfc7748 b-pub", &bpub, &want_bpub);

        let mut want_s = [0u8; 32];
        unhex(b"4a5d9d5ba4ce2de1728e3bf480350f25e07e21c947d19e3376f09b3c1e161742", &mut want_s);
        let s1 = scalarmult(&a, &bpub);
        let s2 = scalarmult(&b, &apub);
        f += check("x25519 rfc7748 shared", &s1, &want_s);
        f += check("x25519 both sides agree", &s2, &want_s);
    }

    f
}
