//! A small ASN.1 DER reader.
//!
//! DER only, and only the parts a certificate is made of.  DER is the
//! definite-length, one-representation subset of BER, which means every
//! length is explicit and nothing is optional — the two things that make a
//! parser of this size safe to write.
//!
//! Every read is bounds-checked and returns None rather than a truncated
//! slice.  A certificate is attacker-supplied data; a parser that trusts a
//! length field is how a chain verifier gets its memory overwritten.

/// Tag numbers, for the ones certificates use.
pub const TAG_BOOLEAN: u8 = 0x01;
pub const TAG_INTEGER: u8 = 0x02;
pub const TAG_BIT_STRING: u8 = 0x03;
pub const TAG_OCTET_STRING: u8 = 0x04;
pub const TAG_NULL: u8 = 0x05;
pub const TAG_OID: u8 = 0x06;
pub const TAG_UTF8_STRING: u8 = 0x0C;
pub const TAG_SEQUENCE: u8 = 0x30;
pub const TAG_SET: u8 = 0x31;
pub const TAG_PRINTABLE_STRING: u8 = 0x13;
pub const TAG_IA5_STRING: u8 = 0x16;
pub const TAG_UTC_TIME: u8 = 0x17;
pub const TAG_GENERALIZED_TIME: u8 = 0x18;

/// A parsed tag-length-value.
#[derive(Clone, Copy)]
pub struct Tlv<'a> {
    pub tag: u8,
    /// The contents, without the tag or the length.
    pub body: &'a [u8],
    /// Tag, length and contents — what a signature covers.
    pub full: &'a [u8],
}

/// Read one TLV from the front of `d`, returning it and the rest.
pub fn read<'a>(d: &'a [u8]) -> Option<(Tlv<'a>, &'a [u8])> {
    if d.len() < 2 {
        return None;
    }
    let tag = d[0];
    let first = d[1];
    let (len, hdr) = if first & 0x80 == 0 {
        (first as usize, 2usize)
    } else {
        let n = (first & 0x7F) as usize;
        // 0x80 means "indefinite", which DER forbids, and any more than four
        // length bytes is a number no real structure has.
        if n == 0 || n > 4 || d.len() < 2 + n {
            return None;
        }
        let mut v = 0usize;
        for i in 0..n {
            v = (v << 8) | d[2 + i] as usize;
        }
        (v, 2 + n)
    };
    if d.len() < hdr + len {
        return None;
    }
    let tlv = Tlv { tag, body: &d[hdr..hdr + len], full: &d[..hdr + len] };
    Some((tlv, &d[hdr + len..]))
}

/// Walk the TLVs inside a constructed value.
pub struct Reader<'a> {
    d: &'a [u8],
}

impl<'a> Reader<'a> {
    pub fn new(d: &'a [u8]) -> Self {
        Self { d }
    }

    pub fn is_empty(&self) -> bool {
        self.d.is_empty()
    }

    pub fn next(&mut self) -> Option<Tlv<'a>> {
        let (t, rest) = read(self.d)?;
        self.d = rest;
        Some(t)
    }

    /// The next value, if it has this tag.
    pub fn expect(&mut self, tag: u8) -> Option<Tlv<'a>> {
        let save = self.d;
        match self.next() {
            Some(t) if t.tag == tag => Some(t),
            _ => {
                self.d = save;
                None
            }
        }
    }

    pub fn peek_tag(&self) -> Option<u8> {
        self.d.first().copied()
    }
}

/// The single top-level value of `d`, which must be the whole of it.
pub fn parse_one(d: &[u8]) -> Option<Tlv<'_>> {
    let (t, rest) = read(d)?;
    if !rest.is_empty() {
        return None;
    }
    Some(t)
}

/// Does an OID's contents equal `want`?  Compared as encoded bytes, which is
/// what the standard means by an OID being equal.
pub fn oid_is(oid: &[u8], want: &[u8]) -> bool {
    oid == want
}

/// Read an INTEGER as a big-endian magnitude, ignoring a leading zero that
/// DER adds to keep a positive value positive.
pub fn int_bytes<'a>(t: &Tlv<'a>) -> Option<&'a [u8]> {
    if t.tag != TAG_INTEGER || t.body.is_empty() {
        return None;
    }
    if t.body[0] == 0 { Some(&t.body[1..]) } else { Some(t.body) }
}

/// Read a BIT STRING's contents, minus the leading "unused bits" byte.
pub fn bit_string<'a>(t: &Tlv<'a>) -> Option<&'a [u8]> {
    if t.tag != TAG_BIT_STRING || t.body.is_empty() {
        return None;
    }
    if t.body[0] != 0 {
        // A partial final byte is legal DER but no certificate key or
        // signature uses one.
        return None;
    }
    Some(&t.body[1..])
}

/// Encode an OID's contents as dotted decimal, for the log.  Returns the
/// number of characters written.
pub fn oid_to_string(oid: &[u8], out: &mut [u8]) -> usize {
    if oid.is_empty() {
        return 0;
    }
    let mut n = 0usize;
    let mut push = |s: &str, n: &mut usize| {
        for &b in s.as_bytes() {
            if *n < out.len() {
                out[*n] = b;
                *n += 1;
            }
        }
    };
    // The first byte packs two components: x*40 + y.
    let first = oid[0] as u32;
    let mut buf = [0u8; 12];
    let a = dec(first / 40, &mut buf);
    push(core::str::from_utf8(a).unwrap_or("?"), &mut n);
    push(".", &mut n);
    let b = dec(first % 40, &mut buf);
    push(core::str::from_utf8(b).unwrap_or("?"), &mut n);

    let mut i = 1usize;
    while i < oid.len() {
        let mut v: u64 = 0;
        while i < oid.len() {
            v = (v << 7) | (oid[i] & 0x7F) as u64;
            let last = oid[i] & 0x80 == 0;
            i += 1;
            if last {
                break;
            }
        }
        push(".", &mut n);
        let d = dec(v as u32, &mut buf);
        push(core::str::from_utf8(d).unwrap_or("?"), &mut n);
    }
    n
}

fn dec(mut v: u32, buf: &mut [u8; 12]) -> &[u8] {
    let mut i = buf.len();
    if v == 0 {
        i -= 1;
        buf[i] = b'0';
    }
    while v > 0 {
        i -= 1;
        buf[i] = b'0' + (v % 10) as u8;
        v /= 10;
    }
    &buf[i..]
}

pub fn selftest() -> usize {
    use crate::crypto::{check, unhex};

    let mut f = 0usize;
    // A SEQUENCE { INTEGER 5, OCTET STRING "ab" }.
    {
        let mut buf = [0u8; 32];
        let n = unhex(b"300702010504026162", &mut buf);
        let Some(seq) = parse_one(&buf[..n]) else {
            return check("der parse", &[], &[1]);
        };
        f += check("der tag", &[seq.tag], &[TAG_SEQUENCE]);
        let mut r = Reader::new(seq.body);
        let i = r.expect(TAG_INTEGER).unwrap();
        f += check("der integer", int_bytes(&i).unwrap_or(&[]), &[5]);
        let o = r.expect(TAG_OCTET_STRING).unwrap();
        f += check("der octet string", o.body, b"ab");
        f += check("der ends", &[r.is_empty() as u8], &[1]);
    }

    // A long-form length, which the short form above would never reach.
    {
        let mut body = [0u8; 200];
        for (i, b) in body.iter_mut().enumerate() {
            *b = i as u8;
        }
        let mut buf = [0u8; 210];
        buf[0] = TAG_OCTET_STRING;
        buf[1] = 0x81;
        buf[2] = 200;
        buf[3..203].copy_from_slice(&body);
        let t = parse_one(&buf[..203]).unwrap();
        f += check("der long form length", &[t.body.len() as u8], &[200]);
        f += check("der long form last byte", &[t.body[199]], &[199]);
    }

    // A truncated length must be refused, not read past the end.  This is the
    // one that matters: a certificate is attacker-supplied.
    {
        let mut buf = [0u8; 8];
        let n = unhex(b"3007020105", &mut buf);
        f += check("der refuses a truncated value", &[parse_one(&buf[..n]).is_some() as u8], &[0]);
        // A length field claiming five bytes of contents when one follows.
        let n = unhex(b"3082000502", &mut buf);
        f += check("der refuses a lying length", &[parse_one(&buf[..n]).is_some() as u8], &[0]);
        // A length with more than four bytes of its own, which no structure
        // has and which is how a length field is used to wrap a parser.
        let n = unhex(b"3085000000000102", &mut buf);
        f += check("der refuses an oversized length", &[parse_one(&buf[..n]).is_some() as u8], &[0]);
    }

    f
}
