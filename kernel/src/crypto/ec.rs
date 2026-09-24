//! ECDSA verification over P-256 and P-384.
//!
//! Most of the certificate chain this browser will meet is ECDSA, not RSA —
//! the first chain captured for testing was ECDSA at every level — so this is
//! not optional if `https` is to work on the modern web.
//!
//! The field arithmetic is the Montgomery code already written for RSA,
//! reused with a different modulus.  A hand-written field implementation with
//! limbs sized to the curve would be faster; reusing the general one means
//! there is one modular arithmetic implementation in the kernel to get right
//! rather than three, and a page load does not care.
//!
//! Points are Jacobian — (X, Y, Z) with affine x = X/Z^2, y = Y/Z^3 — so the
//! scalar multiplication needs no inversion until the very end.  Both curves
//! have a = -3, which the doubling formula below relies on.

use crate::crypto::bignum::{from_be_bytes, zero, Big, Modulus};
use crate::crypto::sha512::{hash384, hash512};
use crate::sha256::hash as hash256;

/// A short Weierstrass curve y^2 = x^3 + ax + b over F_p, with a generator G
/// of prime order n.
pub struct Curve {
    pub name: &'static str,
    pub p: &'static [u8],
    pub b: &'static [u8],
    pub gx: &'static [u8],
    pub gy: &'static [u8],
    pub n: &'static [u8],
}

/// P-256, from FIPS 186-4 D.1.2.
pub const P256: Curve = Curve {
    name: "P-256",
    p: &[
        0xff, 0xff, 0xff, 0xff, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff, 0xff,
        0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
    ],
    b: &[
        0x5a, 0xc6, 0x35, 0xd8, 0xaa, 0x3a, 0x93, 0xe7, 0xb3, 0xeb, 0xbd, 0x55,
        0x76, 0x98, 0x86, 0xbc, 0x65, 0x1d, 0x06, 0xb0, 0xcc, 0x53, 0xb0, 0xf6,
        0x3b, 0xce, 0x3c, 0x3e, 0x27, 0xd2, 0x60, 0x4b,
    ],
    gx: &[
        0x6b, 0x17, 0xd1, 0xf2, 0xe1, 0x2c, 0x42, 0x47, 0xf8, 0xbc, 0xe6, 0xe5,
        0x63, 0xa4, 0x40, 0xf2, 0x77, 0x03, 0x7d, 0x81, 0x2d, 0xeb, 0x33, 0xa0,
        0xf4, 0xa1, 0x39, 0x45, 0xd8, 0x98, 0xc2, 0x96,
    ],
    gy: &[
        0x4f, 0xe3, 0x42, 0xe2, 0xfe, 0x1a, 0x7f, 0x9b, 0x8e, 0xe7, 0xeb, 0x4a,
        0x7c, 0x0f, 0x9e, 0x16, 0x2b, 0xce, 0x33, 0x57, 0x6b, 0x31, 0x5e, 0xce,
        0xcb, 0xb6, 0x40, 0x68, 0x37, 0xbf, 0x51, 0xf5,
    ],
    n: &[
        0xff, 0xff, 0xff, 0xff, 0x00, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff, 0xff,
        0xff, 0xff, 0xff, 0xff, 0xbc, 0xe6, 0xfa, 0xad, 0xa7, 0x17, 0x9e, 0x84,
        0xf3, 0xb9, 0xca, 0xc2, 0xfc, 0x63, 0x25, 0x51,
    ],
};

/// P-384, from FIPS 186-4 D.1.4.
pub const P384: Curve = Curve {
    name: "P-384",
    p: &[
        0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xfe, 0xff, 0xff, 0xff, 0xff,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff, 0xff,
    ],
    b: &[
        0xb3, 0x31, 0x2f, 0xa7, 0xe2, 0x3e, 0xe7, 0xe4, 0x98, 0x8e, 0x05, 0x6b,
        0xe3, 0xf8, 0x2d, 0x19, 0x18, 0x1d, 0x9c, 0x6e, 0xfe, 0x81, 0x41, 0x12,
        0x03, 0x14, 0x08, 0x8f, 0x50, 0x13, 0x87, 0x5a, 0xc6, 0x56, 0x39, 0x8d,
        0x8a, 0x2e, 0xd1, 0x9d, 0x2a, 0x85, 0xc8, 0xed, 0xd3, 0xec, 0x2a, 0xef,
    ],
    gx: &[
        0xaa, 0x87, 0xca, 0x22, 0xbe, 0x8b, 0x05, 0x37, 0x8e, 0xb1, 0xc7, 0x1e,
        0xf3, 0x20, 0xad, 0x74, 0x6e, 0x1d, 0x3b, 0x62, 0x8b, 0xa7, 0x9b, 0x98,
        0x59, 0xf7, 0x41, 0xe0, 0x82, 0x54, 0x2a, 0x38, 0x55, 0x02, 0xf2, 0x5d,
        0xbf, 0x55, 0x29, 0x6c, 0x3a, 0x54, 0x5e, 0x38, 0x72, 0x76, 0x0a, 0xb7,
    ],
    gy: &[
        0x36, 0x17, 0xde, 0x4a, 0x96, 0x26, 0x2c, 0x6f, 0x5d, 0x9e, 0x98, 0xbf,
        0x92, 0x92, 0xdc, 0x29, 0xf8, 0xf4, 0x1d, 0xbd, 0x28, 0x9a, 0x14, 0x7c,
        0xe9, 0xda, 0x31, 0x13, 0xb5, 0xf0, 0xb8, 0xc0, 0x0a, 0x60, 0xb1, 0xce,
        0x1d, 0x7e, 0x81, 0x9d, 0x7a, 0x43, 0x1d, 0x7c, 0x90, 0xea, 0x0e, 0x5f,
    ],
    n: &[
        0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        0xc7, 0x63, 0x4d, 0x81, 0xf4, 0x37, 0x2d, 0xdf, 0x58, 0x1a, 0x0d, 0xb2,
        0x48, 0xb0, 0xa7, 0x7a, 0xec, 0xec, 0x19, 0x6a, 0xcc, 0xc5, 0x29, 0x73,
    ],
};

/// A point.  `z` == 0 means the point at infinity.
#[derive(Clone, Copy)]
struct Jac {
    x: Big,
    y: Big,
    z: Big,
}

/// A curve with its two modular contexts ready.
pub struct Ecdsa {
    md: Modulus,
    order: Modulus,
    fn_limbs: usize,
    order_limbs: usize,
    gx: Big,
    gy: Big,
    b: Big,
    /// p - 2 and n - 2: the exponents that invert in the field and in the
    /// scalar ring, since a^-1 = a^(m-2) for prime m.
    p_minus_2: [u8; 64],
    p_minus_2_len: usize,
    n_minus_2: [u8; 64],
    n_minus_2_len: usize,
    coord_len: usize,
    order_len: usize,
}

impl Ecdsa {
    pub fn new(c: &Curve) -> Option<Self> {
        let (p, pn) = from_be_bytes(c.p);
        let (n, nn) = from_be_bytes(c.n);
        let md = Modulus::new(p, pn)?;
        let order = Modulus::new(n, nn)?;
        let (gx, _) = from_be_bytes(c.gx);
        let (gy, _) = from_be_bytes(c.gy);
        let (b, _) = from_be_bytes(c.b);

        let mut p_minus_2 = [0u8; 64];
        p_minus_2[..c.p.len()].copy_from_slice(c.p);
        sub_two(&mut p_minus_2[..c.p.len()]);
        let mut n_minus_2 = [0u8; 64];
        n_minus_2[..c.n.len()].copy_from_slice(c.n);
        sub_two(&mut n_minus_2[..c.n.len()]);

        Some(Self {
            md, order, fn_limbs: pn, order_limbs: nn, gx, gy, b,
            p_minus_2, p_minus_2_len: c.p.len(),
            n_minus_2, n_minus_2_len: c.n.len(),
            coord_len: c.p.len(), order_len: c.n.len(),
        })
    }

    /// The curve order in bytes.
    pub fn order_len(&self) -> usize {
        self.order_len
    }

    pub fn coord_len(&self) -> usize {
        self.coord_len
    }

    // --- field ---

    fn fmul(&self, a: &Big, b: &Big) -> Big { self.md.mul(a, b) }
    fn fsq(&self, a: &Big) -> Big { self.md.sq(a) }
    fn fadd(&self, a: &Big, b: &Big) -> Big { self.md.add(a, b) }
    fn fsub(&self, a: &Big, b: &Big) -> Big { self.md.sub(a, b) }
    fn finv(&self, a: &Big) -> Big { self.md.pow(a, &self.p_minus_2[..self.p_minus_2_len]) }
    fn fdouble(&self, a: &Big) -> Big { self.md.double(a) }
    fn fzero(&self, a: &Big) -> bool { a[..self.fn_limbs].iter().all(|&w| w == 0) }

    // --- scalars ---

    fn smul(&self, a: &Big, b: &Big) -> Big { self.order.mul(a, b) }
    fn sinv(&self, a: &Big) -> Big { self.order.pow(a, &self.n_minus_2[..self.n_minus_2_len]) }

    /// Is a in [1, n-1]?  A scalar outside that range is a malformed
    /// signature, and reducing it silently would accept one.
    fn scalar_ok(&self, a: &Big) -> bool {
        if a[..self.order_limbs].iter().all(|&w| w == 0) {
            return false;
        }
        !self.order.ge(a)
    }

    // --- points ---

    fn generator(&self) -> Jac {
        let mut z = zero();
        z[0] = 1;
        Jac { x: self.gx, y: self.gy, z }
    }

    fn infinity(&self) -> Jac {
        Jac { x: zero(), y: zero(), z: zero() }
    }

    fn is_infinity(&self, p: &Jac) -> bool {
        self.fzero(&p.z)
    }

    /// Point doubling, using the a = -3 shortcut: M = 3(X-Z^2)(X+Z^2).
    fn double(&self, p: &Jac) -> Jac {
        if self.is_infinity(p) || self.fzero(&p.y) {
            return self.infinity();
        }
        let zz = self.fsq(&p.z);
        let m = {
            let a = self.fsub(&p.x, &zz);
            let b = self.fadd(&p.x, &zz);
            let t = self.fmul(&a, &b);
            self.fadd(&t, &self.fdouble(&t))
        };
        let yy = self.fsq(&p.y);
        let s = self.fdouble(&self.fdouble(&self.fmul(&p.x, &yy)));   // 4XY^2
        let x3 = self.fsub(&self.fsq(&m), &self.fdouble(&s));
        let yyyy = self.fsq(&yy);
        let eight = self.fdouble(&self.fdouble(&self.fdouble(&yyyy)));
        let y3 = self.fsub(&self.fmul(&m, &self.fsub(&s, &x3)), &eight);
        let z3 = self.fdouble(&self.fmul(&p.y, &p.z));
        Jac { x: x3, y: y3, z: z3 }
    }

    /// Jacobian + Jacobian.
    fn add(&self, p: &Jac, q: &Jac) -> Jac {
        if self.is_infinity(p) {
            return *q;
        }
        if self.is_infinity(q) {
            return *p;
        }
        let z1z1 = self.fsq(&p.z);
        let z2z2 = self.fsq(&q.z);
        let u1 = self.fmul(&p.x, &z2z2);
        let u2 = self.fmul(&q.x, &z1z1);
        let s1 = self.fmul(&p.y, &self.fmul(&q.z, &z2z2));
        let s2 = self.fmul(&q.y, &self.fmul(&p.z, &z1z1));

        let h = self.fsub(&u2, &u1);
        let r = self.fsub(&s2, &s1);
        if self.fzero(&h) {
            return if self.fzero(&r) { self.double(p) } else { self.infinity() };
        }
        let hh = self.fsq(&h);
        let hhh = self.fmul(&h, &hh);
        let v = self.fmul(&u1, &hh);
        let x3 = self.fsub(&self.fsub(&self.fsq(&r), &hhh), &self.fdouble(&v));
        let y3 = self.fsub(&self.fmul(&r, &self.fsub(&v, &x3)), &self.fmul(&s1, &hhh));
        let z3 = self.fmul(&self.fmul(&p.z, &q.z), &h);
        Jac { x: x3, y: y3, z: z3 }
    }

    /// k * P, double-and-add over the bits of k from the top down.
    fn scalar_mul(&self, p: &Jac, k: &Big) -> Jac {
        let mut acc = self.infinity();
        let mut started = false;
        for i in (0..self.order_limbs).rev() {
            for bit in (0..64).rev() {
                if started {
                    acc = self.double(&acc);
                }
                if k[i] & (1u64 << bit) != 0 {
                    acc = if started { self.add(&acc, p) } else { *p };
                    started = true;
                }
            }
        }
        acc
    }

    /// Convert to affine, or None for the point at infinity.
    fn affine(&self, p: &Jac) -> Option<(Big, Big)> {
        if self.is_infinity(p) {
            return None;
        }
        let zi = self.finv(&p.z);
        let zi2 = self.fsq(&zi);
        let zi3 = self.fmul(&zi2, &zi);
        Some((self.fmul(&p.x, &zi2), self.fmul(&p.y, &zi3)))
    }

    fn on_curve(&self, p: &Jac) -> bool {
        if self.is_infinity(p) {
            return false;
        }
        let y2 = self.fsq(&p.y);
        let x3 = self.fmul(&self.fsq(&p.x), &p.x);
        // a = -3, so a*x = -3x = 0 - 3x.
        let t3 = self.fadd(&self.fdouble(&p.x), &p.x);
        let ax = self.fsub(&zero(), &t3);
        let rhs = self.fadd(&self.fadd(&x3, &ax), &self.b);
        y2 == rhs
    }

    /// Read a point from the uncompressed SEC1 form, 0x04 || X || Y.
    ///
    /// Compressed points are not accepted: that would need a modular square
    /// root, which is more code for a form no certificate in the wild uses.
    /// The point is checked to be on the curve, which is what stops an
    /// invalid-curve attack.
    pub fn point_from_bytes(&self, data: &[u8]) -> Option<Jac> {
        if data.len() != 1 + 2 * self.coord_len || data[0] != 0x04 {
            return None;
        }
        let (x, _) = from_be_bytes(&data[1..1 + self.coord_len]);
        let (y, _) = from_be_bytes(&data[1 + self.coord_len..]);
        let mut z = zero();
        z[0] = 1;
        let p = Jac { x, y, z };
        if !self.on_curve(&p) {
            return None;
        }
        Some(p)
    }

    /// ECDSA verification.
    ///
    /// `pubkey` is the uncompressed point, `r` and `s` the signature, and
    /// `digest` the hash of the signed bytes — already hashed, because the
    /// hash is chosen by the signer's algorithm identifier, not by the curve.
    pub fn verify(&self, pubkey: &[u8], r: &[u8], s: &[u8], digest: &[u8]) -> bool {
        let Some(q) = self.point_from_bytes(pubkey) else { return false };
        let (rb, _) = from_be_bytes(r);
        let (sb, _) = from_be_bytes(s);
        if !self.scalar_ok(&rb) || !self.scalar_ok(&sb) {
            return false;
        }

        // z is the leftmost nbits of the digest, where nbits is the bit length
        // of the order — so a SHA-512 digest with P-256 uses its first 32
        // bytes, not all 64.
        let mut z_bytes = [0u8; 64];
        if digest.len() >= self.order_len {
            z_bytes[..self.order_len].copy_from_slice(&digest[..self.order_len]);
        } else {
            z_bytes[self.order_len - digest.len()..self.order_len].copy_from_slice(digest);
        }
        let (mut z, _) = from_be_bytes(&z_bytes[..self.order_len]);
        // The leftmost bytes can still exceed the order; reduce if so.
        if self.order.ge(&z) {
            z = self.order.sub(&z, self.order.modulus());
        }

        let w = self.sinv(&sb);
        let u1 = self.smul(&z, &w);
        let u2 = self.smul(&rb, &w);

        let p1 = self.scalar_mul(&self.generator(), &u1);
        let p2 = self.scalar_mul(&q, &u2);
        let sum = self.add(&p1, &p2);
        let Some((x, _)) = self.affine(&sum) else { return false };

        // R.x is reduced mod n before the comparison, which is what the
        // standard says.
        let xr = if self.order.ge(&x) {
            self.order.sub(&x, self.order.modulus())
        } else {
            x
        };
        xr == rb
    }
}

/// Subtract two from a big-endian byte string, in place.
///
/// Both p and n end in an odd byte, so there is always something to borrow
/// from and the result never goes negative.
fn sub_two(v: &mut [u8]) {
    let mut i = v.len();
    while i > 0 {
        i -= 1;
        if v[i] >= 2 {
            v[i] -= 2;
            return;
        }
        v[i] = v[i].wrapping_add(254);   // 256 - 2, carrying the borrow
    }
}

/// Hash a message with the algorithm a signature names.
pub fn hash_for(name: &[u8], data: &[u8], out: &mut [u8; 64]) -> Option<usize> {
    match name {
        b"sha256" => { out[..32].copy_from_slice(&hash256(data)); Some(32) }
        b"sha384" => { out[..48].copy_from_slice(&hash384(data)); Some(48) }
        b"sha512" => { out[..64].copy_from_slice(&hash512(data)); Some(64) }
        _ => None,
    }
}

/// Hash two chunks, for a certificate's TBS.
pub fn hash_for2(name: &[u8], a: &[u8], b: &[u8], out: &mut [u8; 64]) -> Option<usize> {
    match name {
        b"sha256" => {
            let mut h = crate::sha256::Sha256::new();
            h.update(a); h.update(b);
            out[..32].copy_from_slice(&h.finish());
            Some(32)
        }
        b"sha384" => {
            let mut h = crate::crypto::sha512::Sha512::new384();
            h.update(a); h.update(b);
            out[..48].copy_from_slice(&h.finish384());
            Some(48)
        }
        b"sha512" => {
            let mut h = crate::crypto::sha512::Sha512::new512();
            h.update(a); h.update(b);
            out[..64].copy_from_slice(&h.finish512());
            Some(64)
        }
        _ => None,
    }
}

pub fn selftest() -> usize {
    use crate::crypto::check;
    use crate::crypto::ec_vectors::EC_VECTORS;
    let mut f = 0usize;

    let p256 = Ecdsa::new(&P256);
    let p384 = Ecdsa::new(&P384);
    let (Some(p256), Some(p384)) = (p256, p384) else {
        return check("ecdsa curve setup", &[], &[1]);
    };

    // Real signatures from a real chain: each one is a certificate's
    // TBSCertificate, checked against the public key of the certificate above
    // it.  A curve implementation that is subtly wrong — a bad doubling
    // formula, the wrong z truncation — passes an invented test and fails
    // these.
    //
    // The vectors are hex strings rather than byte literals because a byte
    // literal long enough to hold a certificate has to wrap, and a wrapped
    // byte literal silently gains the newline and the indentation as bytes.
    // That is not hypothetical: it added ten bytes to every value here and
    // turned a working verifier into a panic.
    for v in EC_VECTORS {
        let curve = match v.curve {
            "P-256" => &p256,
            "P-384" => &p384,
            _ => continue,
        };

        let mut pub_buf = [0u8; 192];
        let mut tbs = [0u8; 1200];
        let mut sig = [0u8; 192];
        let pn = crate::crypto::unhex(v.pubkey, &mut pub_buf);
        let tn = crate::crypto::unhex(v.signed, &mut tbs);
        let sn = crate::crypto::unhex(v.sig, &mut sig);
        if pn == 0 || tn == 0 || sn == 0 {
            f += check(v.name, &[], &[1]);
            continue;
        }

        let mut digest = [0u8; 64];
        let Some(dn) = hash_for(v.hash.as_bytes(), &tbs[..tn], &mut digest) else {
            f += check(v.name, &[], &[1]);
            continue;
        };
        let half = sn / 2;
        let ok = curve.verify(&pub_buf[..pn], &sig[..half], &sig[half..sn], &digest[..dn]);
        f += check(v.name, &[ok as u8], &[1]);

        // And the same signature with one bit changed must fail.  A verifier
        // that accepts everything passes the line above.
        let mut bad = [0u8; 192];
        bad[..sn].copy_from_slice(&sig[..sn]);
        bad[half / 2] ^= 0x01;
        let ok = curve.verify(&pub_buf[..pn], &bad[..half], &bad[half..sn], &digest[..dn]);
        f += check("rejects a tampered signature", &[ok as u8], &[0]);
    }

    f
}
