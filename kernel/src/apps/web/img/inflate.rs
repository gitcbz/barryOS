//! DEFLATE (RFC 1951), and the zlib wrapper around it (RFC 1950).
//!
//! PNG's pixel data is zlib, and zlib's payload is DEFLATE, so a picture in
//! that format cannot be read without both.  Only the decompressor is here:
//! nothing in this kernel compresses anything.
//!
//! This is deliberately the simple decoder — a canonical Huffman table walked
//! one bit at a time — rather than the table-driven one that reads several
//! bits per lookup.  The fast version is another two hundred lines of table
//! construction for a constant factor, and this one is correct by inspection.
//! That trade is worth making here: the largest picture the browser will
//! accept decodes in a few tens of milliseconds either way, and a decoder
//! whose failure mode is "reads slightly wrong" is a decoder nobody can debug
//! from a screenshot.

use alloc::vec::Vec;

/// Length codes 257..=285, and the extra bits each carries.
const LEN_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59,
    67, 83, 99, 115, 131, 163, 195, 227, 258,
];
const LEN_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];

/// Distance codes 0..=29.
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769,
    1025, 1537, 2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13,
];

/// The order the code-length code's own lengths arrive in.  Not 0..18: the
/// three lengths that describe long runs of zeros come first, because that is
/// what a header mostly consists of.
const CLEN_ORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

/// A bit reader, least significant bit first, which is the order DEFLATE
/// writes everything except the Huffman codes themselves.
struct Bits<'a> {
    src: &'a [u8],
    at: usize,
    buf: u32,
    n: u32,
}

impl<'a> Bits<'a> {
    fn new(src: &'a [u8]) -> Bits<'a> {
        Bits { src, at: 0, buf: 0, n: 0 }
    }

    fn take(&mut self, want: u32) -> Result<u32, &'static str> {
        while self.n < want {
            if self.at >= self.src.len() {
                return Err("the compressed data stops in the middle of a symbol");
            }
            self.buf |= (self.src[self.at] as u32) << self.n;
            self.at += 1;
            self.n += 8;
        }
        let v = if want >= 32 { self.buf } else { self.buf & ((1u32 << want) - 1) };
        self.buf >>= want;
        self.n -= want;
        Ok(v)
    }

    fn bit(&mut self) -> Result<u32, &'static str> {
        self.take(1)
    }

    /// Rewind to the byte boundary.  Reading whole bytes at a time means the
    /// buffer can hold up to three bytes that have not been consumed yet, so
    /// the byte position is `at` minus whatever is still buffered — not `at`.
    fn align(&mut self) {
        let back = (self.n / 8) as usize;
        self.at -= back;
        self.buf = 0;
        self.n = 0;
    }
}

/// A canonical Huffman table: how many codes there are of each length, and
/// the symbols in order.
struct Huff {
    count: [u16; 16],
    symbol: [u16; 288],
}

impl Huff {
    /// Build from a list of code lengths, one per symbol.  Length 0 means the
    /// symbol has no code.
    fn build(lengths: &[u8]) -> Result<Huff, &'static str> {
        let mut count = [0u16; 16];
        for &l in lengths {
            if l as usize > 15 {
                return Err("a Huffman code is longer than fifteen bits");
            }
            count[l as usize] += 1;
        }
        if count[0] as usize == lengths.len() {
            return Err("a Huffman table with no codes in it");
        }
        // Over-subscribed means two codes share a bit pattern, and there is no
        // way to tell which was meant.  Under-subscribed is legal and only
        // means some bit patterns are not codes.
        let mut left = 1i32;
        for len in 1..16 {
            left <<= 1;
            left -= count[len] as i32;
            if left < 0 {
                return Err("a Huffman table that assigns one code twice");
            }
        }
        let mut offs = [0u16; 16];
        for len in 1..15 {
            offs[len + 1] = offs[len] + count[len];
        }
        let mut symbol = [0u16; 288];
        for (s, &l) in lengths.iter().enumerate() {
            if l != 0 {
                symbol[offs[l as usize] as usize] = s as u16;
                offs[l as usize] += 1;
            }
        }
        Ok(Huff { count, symbol })
    }

    /// One code, walked a bit at a time.
    fn decode(&self, bits: &mut Bits) -> Result<u16, &'static str> {
        let mut code = 0i32;
        let mut first = 0i32;
        let mut index = 0i32;
        for len in 1..16 {
            code |= bits.bit()? as i32;
            let count = self.count[len] as i32;
            // The first `count` codes of this length start at `first`; if what
            // has been read falls inside that window, it is one of them.
            if code - first < count {
                return Ok(self.symbol[(index + code - first) as usize]);
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        Err("no Huffman code matches the bits that were read")
    }
}

/// Decompress a zlib stream (RFC 1950), refusing to produce more than `cap`
/// bytes.
///
/// The cap is not a convenience.  A decompressor that trusts its input is the
/// classic way for a file of a few kilobytes to ask for gigabytes, and this
/// one runs in a kernel whose heap is a bump allocator with no way back.
pub fn zlib(src: &[u8], cap: usize) -> Result<Vec<u8>, &'static str> {
    if src.len() < 2 {
        return Err("the compressed data is too short to have a header");
    }
    let (cmf, flg) = (src[0], src[1]);
    if cmf & 0x0F != 8 {
        return Err("not a DEFLATE stream (the compression method is not 8)");
    }
    // The header's own check: it is the only thing standing between a wrong
    // guess about the format and a decoder that runs on noise.
    if (cmf as u16 * 256 + flg as u16) % 31 != 0 {
        return Err("the zlib header's check value does not match");
    }
    if flg & 0x20 != 0 {
        return Err("a preset zlib dictionary, which is not used for pictures");
    }
    inflate(&src[2..], cap)
}

/// Decompress a raw DEFLATE stream (RFC 1951).
pub fn inflate(src: &[u8], cap: usize) -> Result<Vec<u8>, &'static str> {
    let mut bits = Bits::new(src);
    let mut out: Vec<u8> = Vec::new();
    loop {
        let last = bits.take(1)?;
        match bits.take(2)? {
            0 => stored(&mut bits, &mut out, cap)?,
            1 => {
                let lit = fixed_literal();
                let dist = fixed_distance();
                codes(&mut bits, &mut out, &lit, &dist, cap)?;
            }
            2 => {
                let (lit, dist) = dynamic(&mut bits)?;
                codes(&mut bits, &mut out, &lit, &dist, cap)?;
            }
            _ => return Err("compression type 3, which DEFLATE leaves undefined"),
        }
        if last == 1 {
            return Ok(out);
        }
    }
}

/// A block stored with no compression at all.
fn stored(bits: &mut Bits, out: &mut Vec<u8>, cap: usize) -> Result<(), &'static str> {
    bits.align();
    let at = bits.at;
    if at + 4 > bits.src.len() {
        return Err("a stored block with no length");
    }
    let len = (bits.src[at] as usize) | ((bits.src[at + 1] as usize) << 8);
    let nlen = (bits.src[at + 2] as usize) | ((bits.src[at + 3] as usize) << 8);
    // The one's complement is there so a truncated stream is caught here
    // rather than turning into a wrong picture.
    if len ^ 0xFFFF != nlen {
        return Err("a stored block whose length is not its own complement");
    }
    if at + 4 + len > bits.src.len() {
        return Err("a stored block that runs past the end of the data");
    }
    if out.len() + len > cap {
        return Err("the picture decompresses to more than it is allowed");
    }
    out.extend_from_slice(&bits.src[at + 4..at + 4 + len]);
    bits.at = at + 4 + len;
    Ok(())
}

/// The literal and length tables every DEFLATE stream is allowed to use
/// without declaring them.  Fixed tables cost nothing and make a small block
/// cheaper than describing a table would.
fn fixed_literal() -> Huff {
    let mut lengths = [0u8; 288];
    for (i, l) in lengths.iter_mut().enumerate() {
        *l = match i {
            0..=143 => 8,
            144..=255 => 9,
            256..=279 => 7,
            _ => 8,
        };
    }
    // Cannot fail: these are the lengths the specification fixes.
    Huff::build(&lengths).unwrap_or(Huff { count: [0; 16], symbol: [0; 288] })
}

fn fixed_distance() -> Huff {
    let lengths = [5u8; 30];
    Huff::build(&lengths).unwrap_or(Huff { count: [0; 16], symbol: [0; 288] })
}

/// A block that describes its own Huffman tables first.
fn dynamic(bits: &mut Bits) -> Result<(Huff, Huff), &'static str> {
    let hlit = bits.take(5)? as usize + 257;
    let hdist = bits.take(5)? as usize + 1;
    let hclen = bits.take(4)? as usize + 4;

    // The code-length code is itself sent as lengths, in a fixed order chosen
    // so that the ones a header usually needs come early.
    let mut clen = [0u8; 19];
    for i in 0..hclen {
        clen[CLEN_ORDER[i]] = bits.take(3)? as u8;
    }
    let clen = Huff::build(&clen)?;

    // Then the two real tables, run-length encoded with that code.
    let mut lengths = [0u8; 288 + 30];
    let total = hlit + hdist;
    let mut i = 0usize;
    while i < total {
        let sym = clen.decode(bits)?;
        match sym {
            0..=15 => {
                lengths[i] = sym as u8;
                i += 1;
            }
            16 => {
                if i == 0 {
                    return Err("a run of repeated code lengths with nothing to repeat");
                }
                let prev = lengths[i - 1];
                let n = 3 + bits.take(2)? as usize;
                for _ in 0..n {
                    if i >= total {
                        return Err("a run of repeated code lengths that runs off the end");
                    }
                    lengths[i] = prev;
                    i += 1;
                }
            }
            17 | 18 => {
                let n = if sym == 17 { 3 + bits.take(3)? as usize } else { 11 + bits.take(7)? as usize };
                for _ in 0..n {
                    if i >= total {
                        return Err("a run of zeros that runs off the end");
                    }
                    lengths[i] = 0;
                    i += 1;
                }
            }
            _ => return Err("a code-length symbol that is not one"),
        }
    }

    // A distance table with a single code is legal and means "always the same
    // distance".  DEFLATE allows it to arrive with no lengths at all, which
    // `build` would reject as empty.
    let lit = Huff::build(&lengths[..hlit])?;
    let dist_lengths = &lengths[hlit..total];
    let dist = if dist_lengths.iter().all(|&l| l == 0) {
        let mut l = [0u8; 30];
        l[0] = 1;
        Huff::build(&l)?
    } else {
        Huff::build(dist_lengths)?
    };
    Ok((lit, dist))
}

/// The main loop: literals copied out, and (length, distance) pairs copied
/// back from what has already been written.
fn codes(
    bits: &mut Bits,
    out: &mut Vec<u8>,
    lit: &Huff,
    dist: &Huff,
    cap: usize,
) -> Result<(), &'static str> {
    loop {
        let sym = lit.decode(bits)?;
        if sym < 256 {
            if out.len() >= cap {
                return Err("the picture decompresses to more than it is allowed");
            }
            out.push(sym as u8);
            continue;
        }
        if sym == 256 {
            return Ok(());
        }
        let li = sym as usize - 257;
        if li >= LEN_BASE.len() {
            return Err("a length symbol that is not one");
        }
        let len = LEN_BASE[li] as usize + bits.take(LEN_EXTRA[li] as u32)? as usize;

        let dsym = dist.decode(bits)? as usize;
        if dsym >= DIST_BASE.len() {
            return Err("a distance symbol that is not one");
        }
        let back = DIST_BASE[dsym] as usize + bits.take(DIST_EXTRA[dsym] as u32)? as usize;
        if back > out.len() {
            return Err("a copy from before the start of the data");
        }
        if out.len() + len > cap {
            return Err("the picture decompresses to more than it is allowed");
        }
        // Byte at a time, not a slice copy: the source and the destination can
        // overlap, and that overlap is how DEFLATE encodes a run.
        let from = out.len() - back;
        for k in 0..len {
            let b = out[from + k];
            out.push(b);
        }
    }
}
