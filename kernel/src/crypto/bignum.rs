//! Big integers, for RSA.
//!
//! Certificate chain verification means checking one signature per certificate,
//! and every one of those is a modular exponentiation with the issuer's public
//! key.  That is the only thing this module does — there is no general-purpose
//! arithmetic here, no signed integers and no division beyond the Montgomery
//! form.
//!
//! Montgomery rather than plain square-and-multiply because it replaces the
//! reduction after every multiply with shifts and adds: no long division in
//! the inner loop, which is the difference between a certificate check taking
//! a moment and taking a minute.
//!
//! Limbs are 64 bits, little-endian, and every routine takes the actual limb
//! count so a 2048-bit key costs 32-limb products rather than the 64 the
//! arrays are sized for.

/// 4096 bits — the largest RSA modulus in the wild.
pub const MAX_LIMBS: usize = 64;
pub type Big = [u64; MAX_LIMBS];

pub fn zero() -> Big {
    [0u64; MAX_LIMBS]
}

/// Read a big-endian byte string into limbs.  Returns the limb count used.
pub fn from_be_bytes(bytes: &[u8]) -> (Big, usize) {
    let mut out = zero();
    let n = (bytes.len() + 7) / 8;
    let n = n.min(MAX_LIMBS);
    for i in 0..n {
        let end = bytes.len() - i * 8;
        let start = end.saturating_sub(8);
        let mut limb = 0u64;
        for &b in &bytes[start..end] {
            limb = (limb << 8) | b as u64;
        }
        out[i] = limb;
    }
    // Trim leading zero limbs so callers do not pay for them.
    let mut used = n;
    while used > 1 && out[used - 1] == 0 {
        used -= 1;
    }
    (out, used)
}

/// Write limbs back out big-endian, exactly `len` bytes.
pub fn to_be_bytes(a: &Big, n: usize, out: &mut [u8]) {
    let bytes = n * 8;
    for i in 0..out.len().min(bytes) {
        // Byte i counts from the most significant end.
        let from_low = bytes - 1 - i;
        let limb = a[from_low / 8];
        out[i] = (limb >> ((from_low % 8) * 8)) as u8;
    }
}

fn cmp(a: &Big, b: &Big, n: usize) -> core::cmp::Ordering {
    for i in (0..n).rev() {
        if a[i] != b[i] {
            return a[i].cmp(&b[i]);
        }
    }
    core::cmp::Ordering::Equal
}

fn is_zero(a: &Big, n: usize) -> bool {
    a[..n].iter().all(|&w| w == 0)
}

/// r = a - b, assuming a >= b.
fn sub_into(r: &mut Big, a: &Big, b: &Big, n: usize) {
    let mut borrow = 0u64;
    for i in 0..n {
        let (v, b1) = a[i].overflowing_sub(b[i]);
        let (v, b2) = v.overflowing_sub(borrow);
        r[i] = v;
        borrow = (b1 as u64) | (b2 as u64);
    }
}

/// r = (a + b) mod m, assuming a, b < m.
fn add_mod(r: &mut Big, a: &Big, b: &Big, m: &Big, n: usize) {
    let mut carry = 0u64;
    for i in 0..n {
        let (v, c1) = a[i].overflowing_add(b[i]);
        let (v, c2) = v.overflowing_add(carry);
        r[i] = v;
        carry = (c1 as u64) | (c2 as u64);
    }
    // A carry out means the sum exceeded 2^(64n) > m, and even without one
    // a + b can still be >= m; either way, subtract it back off.
    if carry != 0 || cmp(r, m, n) != core::cmp::Ordering::Less {
        let t = *r;
        sub_into(r, &t, m, n);
    }
}

/// r = a * R^-1 mod m, where R = 2^(64n).
///
/// `np0` is -m^-1 mod 2^64.  This is the whole point of the Montgomery form:
/// the reduction below is shifts and additions, not a division.
fn mont_mul(a: &Big, b: &Big, m: &Big, np0: u64, n: usize) -> Big {
    let mut t = [0u64; 2 * MAX_LIMBS + 2];

    // t = a * b
    for i in 0..n {
        let mut carry = 0u128;
        for j in 0..n {
            let x = t[i + j] as u128 + (a[i] as u128) * (b[j] as u128) + carry;
            t[i + j] = x as u64;
            carry = x >> 64;
        }
        // Propagate the final carry; it can ripple past one limb.
        let mut k = i + n;
        while carry != 0 {
            let x = t[k] as u128 + carry;
            t[k] = x as u64;
            carry = x >> 64;
            k += 1;
        }
    }

    // Reduce: n passes, each making one limb vanish.
    for i in 0..n {
        let u = t[i].wrapping_mul(np0);
        let mut carry = 0u128;
        for j in 0..n {
            let x = t[i + j] as u128 + (u as u128) * (m[j] as u128) + carry;
            t[i + j] = x as u64;
            carry = x >> 64;
        }
        let mut k = i + n;
        while carry != 0 {
            let x = t[k] as u128 + carry;
            t[k] = x as u64;
            carry = x >> 64;
            k += 1;
        }
    }

    // The answer is t[n .. 2n], possibly needing one subtraction of m.
    let mut r = zero();
    r[..n].copy_from_slice(&t[n..2 * n]);
    if t[2 * n] != 0 || cmp(&r, m, n) != core::cmp::Ordering::Less {
        let c = r;
        sub_into(&mut r, &c, m, n);
    }
    r
}

/// -m^-1 mod 2^64, by Newton's iteration on the low limb.
fn mont_np0(m: &Big) -> u64 {
    let m0 = m[0];
    let mut inv = 1u64;
    for _ in 0..6 {
        inv = inv.wrapping_mul(2u64.wrapping_sub(m0.wrapping_mul(inv)));
    }
    inv.wrapping_neg()
}

/// A modulus prepared for exponentiation, with its Montgomery constants.
pub struct Modulus {
    m: Big,
    n: usize,
    np0: u64,
    /// R^2 mod m, for converting into the Montgomery form.
    rr: Big,
}

impl Modulus {
    pub fn new(m: Big, n: usize) -> Option<Self> {
        if n == 0 || n > MAX_LIMBS || m[0] & 1 == 0 {
            return None;                    // must be odd for Montgomery
        }
        let np0 = mont_np0(&m);

        // R^2 mod m by doubling: start at 1 and double 2 * 64 * n times,
        // reducing each time.  Slow but it happens once per key.
        let mut rr = zero();
        rr[0] = 1;
        for _ in 0..(2 * 64 * n) {
            let mut doubled = zero();
            add_mod(&mut doubled, &rr, &rr, &m, n);
            rr = doubled;
        }
        Some(Self { m, n, np0, rr })
    }

    fn to_mont(&self, a: &Big) -> Big {
        mont_mul(a, &self.rr, &self.m, self.np0, self.n)
    }

    fn from_mont(&self, a: &Big) -> Big {
        let mut one = zero();
        one[0] = 1;
        mont_mul(a, &one, &self.m, self.np0, self.n)
    }

    /// base^exp mod m.  `exp` is big-endian bytes; the exponent is scanned
    /// from the top bit down, so a 65537 exponent costs 17 multiplies and a
    /// full set of squarings.
    pub fn pow(&self, base: &Big, exp: &[u8]) -> Big {
        let x = self.to_mont(base);
        let mut one = zero();
        one[0] = 1;
        let mut acc = self.to_mont(&one);
        let _ = acc;

        let mut started = false;
        for &byte in exp {
            for bit in (0..8).rev() {
                if started {
                    acc = mont_mul(&acc, &acc, &self.m, self.np0, self.n);
                }
                if byte & (1 << bit) != 0 {
                    acc = if started {
                        mont_mul(&acc, &x, &self.m, self.np0, self.n)
                    } else {
                        x
                    };
                    started = true;
                }
            }
        }
        if !started {
            // exp == 0: anything to the zero is one.
            let mut one = zero();
            one[0] = 1;
            return one;
        }
        self.from_mont(&acc)
    }

    pub fn limb_count(&self) -> usize {
        self.n
    }

    /// One limb of the modulus, for callers that need its true bit length.
    pub fn limb(&self, i: usize) -> u64 {
        if i < self.n { self.m[i] } else { 0 }
    }

    pub fn bytes(&self) -> usize {
        self.n * 8
    }

    pub fn zero(&self) -> Big {
        zero()
    }

    pub fn is_zero(&self, a: &Big) -> bool {
        is_zero(a, self.n)
    }

    /// Is a >= m?  Used to reject a signature that was not reduced.
    pub fn ge(&self, a: &Big) -> bool {
        cmp(a, &self.m, self.n) != core::cmp::Ordering::Less
    }

    /// (a + b) mod m.  Exposed for the elliptic-curve code, which needs field
    /// addition and subtraction as well as multiplication.
    pub fn add(&self, a: &Big, b: &Big) -> Big {
        let mut r = zero();
        add_mod(&mut r, a, b, &self.m, self.n);
        r
    }

    /// (a - b) mod m.
    pub fn sub(&self, a: &Big, b: &Big) -> Big {
        // a - b mod m = a + (m - b) mod m, which avoids a negative result
        // needing its own representation.
        let mut neg = zero();
        sub_into(&mut neg, &self.m, b, self.n);
        let mut r = zero();
        add_mod(&mut r, a, &neg, &self.m, self.n);
        r
    }

    /// a * 2 mod m, for the doubling in the curve ladder.
    pub fn double(&self, a: &Big) -> Big {
        self.add(a, a)
    }

    /// (a * b) mod m.
    ///
    /// This converts both operands into the Montgomery form and back out
    /// again, which is three times the work of keeping them there.  The
    /// elliptic-curve code multiplies a few hundred thousand times per
    /// signature at most, so the clarity is worth more than the factor; the
    /// RSA path, which does mind, stays in Montgomery form throughout.
    pub fn mul(&self, a: &Big, b: &Big) -> Big {
        let am = self.to_mont(a);
        let bm = self.to_mont(b);
        self.from_mont(&mont_mul(&am, &bm, &self.m, self.np0, self.n))
    }

    /// a^2 mod m.
    pub fn sq(&self, a: &Big) -> Big {
        self.mul(a, a)
    }

    /// a^exp mod m with a decimal exponent given as a small integer.
    pub fn pow_small(&self, a: &Big, exp: &[u8]) -> Big {
        self.pow(a, exp)
    }

    pub fn modulus(&self) -> &Big {
        &self.m
    }
}

/// Compare a big integer against a full-width hex string, big-endian.
///
/// Every expected value here is zero-padded to the modulus width.  It has to
/// be: `unhex` reads two characters at a time, so a hex string with an odd
/// digit count silently loses its last nibble and the whole comparison shifts
/// by four bits.  That is exactly how the first version of this test reported
/// a correct modexp as wrong.
fn check_big(name: &str, a: &Big, n: usize, hex: &[u8]) -> usize {
    let bn = n * 8;
    let mut want = [0u8; 512];
    let wn = crate::crypto::unhex(hex, &mut want);
    let mut got = [0u8; 512];
    to_be_bytes(a, n, &mut got);
    if wn != bn {
        return crate::crypto::check(name, &[0], &[1]);
    }
    crate::crypto::check(name, &got[..bn], &want[..bn])
}

pub fn selftest() -> usize {
    use crate::crypto::{check, unhex};

    // A fixed 2048-bit modulus and base.  The expected values are CPython's
    // pow(), not this code's output: a modexp compared against itself passes
    // with the limbs reversed or the Montgomery constant wrong.
    let mut n_bytes = [0u8; 256];
    unhex(
        b"ff915d7cfe166565f9142bee723e2de4ee643addf6921b7b6ec85a3b73e33172          e2ddc31c759c3ee4d980136b7e6c0731990d902380905be5384a37d676ba2074          414f21c5135499bc53154baec1b73227e4abf37b78658dc4c8598cfe5e9b7255          97cae8c7162531f4093e1db5d7171bde4407070234c1d5e34383ea420eed420f          b84caaa73e8540378a5eace47106302f585cfe94f07ed35855456035ee8bf4eb          89f03016c0602ef071660715a6b64016c344112d5e20408c4732c44d6d3b8e45          d8e734910ea02dacd0a45d3eba5ac473c9375d6d91937ccc8f07d990252c3c6f          b393586ca18d3c058be37df3e9e4c8e8a925e7a1f077b69fd1c5aaf003e8b84b",
        &mut n_bytes,
    );
    let mut b_bytes = [0u8; 256];
    unhex(
        b"87d06d97bffca55ba4b37470f2af70b90f67f69b5f9430983837f2cacdec56fd          7da13576d4958a3937fcda017c3870ad93e0e0b6e8d19aaa59891a07f42854ca          9c90af06a1c3f4078e139a9a60bf4967309242456870e57a4f757a88037a3844          063c9f73235e99da716ceb7220a3af501b07fd1b6a2d9c534aadacc0f06263bd          975bb85dc91c5c0e3900f62a7cc3dd6c05c10af9bc39c7cf1e30cbb2d857f90b          3d61108f81f034e36babf8619371e438842aaf2cce6ee9a174ba4dc72c26e850          d11bd6324600d9bd80ff78401e363d87698f94979aaf8833ba858629a7dbdcd1          647dfdb08c9aaa012ab2a6b78ee72696d96a74cfd1054ad609eb150f46eb3a10",
        &mut b_bytes,
    );

    let (m, nl) = from_be_bytes(&n_bytes);
    let (b, _) = from_be_bytes(&b_bytes);
    let Some(md) = Modulus::new(m, nl) else {
        return check("modexp modulus setup", &[], &[1]);
    };
    let mut f = 0usize;

    // The ordinary public exponent: 17 multiplies and a full set of squarings.
    f += check_big("modexp 2048-bit e=65537", &md.pow(&b, &[0x01, 0x00, 0x01]), nl,
        b"9cb7a2f5ecd471b8865ab6ba97ccd344a8ac885296df2ab638532e3891556da8          83c00243f92afa547b5d96bb3569ee4369124d8c62cb7649e79b68bf4f2af00          9e74937fae63f907eb79d8755f81bedc60567097476ca5701813de0ef8d119d3          fb4be7583186d925333778994e23b55a537274418bd939099205e69d00e56e46          f6c6edee7271ac3951f2816f39c6b348da5422a415aa9fe0770ee198a6b4a0ef          63c00ec6eb70b2878169b29fdcb739768ca8f73571521c0664b214d46d6a66fc          5a6edbda4e0674c12991443f0e3b6a8df0a1a1dc1cfc97ce051727726cad8a14          f6cd656b6321ff58099c5178cc808637a2f1f752a6b7e918a2184a4451d211aa0");

    // The smallest exponent, which reaches the multiply-only path: one square
    // and one multiply.  Its result is 255 bytes, so a comparison that is not
    // full width reports it as wrong.
    f += check_big("modexp 2048-bit e=3", &md.pow(&b, &[3]), nl,
        b"01cad0505221ccda6c8e7a9a40e87f4662443bae02cd70859c87c79b48348f2b          ebf9567025985a09b308ea5c9f6019566cb2aa9e7fd8576d5023dbd93655595          c00fd9051e17ba6d9e409b94bf71d632d624fc38b9a58c21d9fc9951f5aba3c          4e747e8973580a92579f0f950e4fbb5bfc5a914f5708032b0185edac79c144b4          b471248009589c276484a44bc5d7058d895c7bfe1d5a460cc004718f1674695a          290e284812c9df75dabc6651611178a53cd0d2b21a6a37935d20f215fa4300f6          6c9a6a889502709e02d5405cda3f4b305c05acccc3347c58836b21484d2c1cc5          6c60317477ec8572295181aa2cfe86ceb43c387509c54a7c81229487fa4e43b128");

    // A full-width exponent, so the scan over the exponent bits is exercised
    // with every bit set and not just a sparse 0x10001.
    let mut big_e = [0xFFu8; 256];
    big_e[255] = 0x61;
    f += check_big("modexp 2048-bit full-width exponent", &md.pow(&b, &big_e), nl,
        b"13abc7e81b427e0651ed56db1e604876c67b144ad62a0c38d3dc672883da10c4          cbce926762d332ad9dff37f92d52aca0897796eb99d15bcd8b9eed46045962cc          a7f2180c88f43ffa798030afbe2ed7b4409f8ae83137f685909db989e0a4797          fcc238167bc50789c59ca23cd491c975738213928d0121d61847da301d7cc232          7df55e5c487864205eac212f4c222a81d1831a71b0cce7a96798203011168ebc          018482050aad1420827442727966a211767df5f296bed8a5cb1590a070c804b5          0a28c2480046e1bf6a934cf086b09ae26a1625e56b85185c6e59f77f52c7effd          8548c30c8e31adbabc35434afd58aea973385478da7cb0e48cc6239c148478c7d");

    // Trivial cases, so a failure of the very first bit of the exponent scan
    // shows up on its own rather than inside a 2048-bit number.
    let mut two = zero();
    two[0] = 2;
    let mut want2 = [0u8; 512];
    want2[255] = 2;
    // 2^1, full width.
    let mut want_one = [0u8; 512];
    want_one[255] = 2;
    let mut two_hex = [b'0'; 512];
    two_hex[510] = b'0';
    two_hex[511] = b'2';
    f += check_big("modexp 2^1", &md.pow(&two, &[1]), nl, &two_hex);

    // Small exponent, larger result: 2^16, where the exponent is one byte.
    f += check_big("modexp 2^16", &md.pow(&two, &[0x10]), nl,
        b"0000000000000000000000000000000000000000000000000000000000000000          0000000000000000000000000000000000000000000000000000000000000000          0000000000000000000000000000000000000000000000000000000000000000          0000000000000000000000000000000000000000000000000000000000000000          0000000000000000000000000000000000000000000000000000000000000000          0000000000000000000000000000000000000000000000000000000000000000          0000000000000000000000000000000000000000000000000000000000000000          0000000000000000000000000000000000000000000000000000000000010000");

    f
}
