//! JPEG: baseline and progressive, Huffman-coded, eight bits per sample.
//!
//! Progressive is not an optional extra here.  Every JPEG this browser was
//! tested against — from a CDN, from a resizing service, from a content
//! manager — came back progressive, because a progressive file *looks* like it
//! loads faster and every encoder worth using defaults to it.  A baseline-only
//! decoder would have worked on the specification's examples and on almost
//! nothing on the web, which is the kind of "working" that is worse than an
//! honest refusal.
//!
//! The two share everything but the band loop.  Coefficients are decoded into
//! per-component planes and turned into pixels at the end, so a baseline file
//! is simply a file with one scan that happens to cover everything: the
//! Huffman tables, the bit reader, the inverse transform and the colour
//! conversion are one implementation exercised by both paths rather than two
//! implementations that have to be kept in step.
//!
//! What is not here: arithmetic coding, lossless and hierarchical modes, and
//! twelve-bit samples.  All three exist in the specification and none of them
//! exists on the web.

use super::{plane, Image};
use alloc::vec::Vec;

/// The order coefficients arrive in, as indices into a block held in natural
/// row-major order.  JPEG walks a block from low frequency to high, which is a
/// diagonal path; everything here works in natural order and converts at the
/// edges, because the inverse transform wants a plain 8 by 8 grid.
const ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5,
    12, 19, 26, 33, 40, 48, 41, 34, 27, 20, 13, 6, 7, 14, 21, 28,
    35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51,
    58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

/// `C(u) cos((2x+1) u pi / 16)` in twelve-bit fixed point, with `C(0)`'s
/// one-over-root-two folded in.  Written out rather than computed because
/// there is no maths library in the kernel to compute it with.
#[rustfmt::skip]
const T: [[i32; 8]; 8] = [
    [2896,  2896,  2896,  2896,  2896,  2896,  2896,  2896],
    [4017,  3406,  2276,   799,  -799, -2276, -3406, -4017],
    [3784,  1567, -1567, -3784, -3784, -1567,  1567,  3784],
    [3406,  -799, -4017, -2276,  2276,  4017,   799, -3406],
    [2896, -2896, -2896,  2896,  2896, -2896, -2896,  2896],
    [2276, -4017,   799,  3406, -3406,  -799,  4017, -2276],
    [1567, -3784,  3784, -1567, -1567,  3784, -3784,  1567],
    [799,  -2276,  3406, -4017,  4017, -3406,  2276,  -799],
];

#[derive(Clone, Copy)]
struct Quant {
    v: [u16; 64],
    set: bool,
}

impl Default for Quant {
    fn default() -> Quant {
        Quant { v: [0; 64], set: false }
    }
}

/// One Huffman table: how many codes there are of each length, and the symbols
/// in order.
#[derive(Clone, Copy)]
struct Huff {
    counts: [u8; 17],
    symbols: [u8; 256],
    set: bool,
}

impl Default for Huff {
    fn default() -> Huff {
        Huff { counts: [0; 17], symbols: [0; 256], set: false }
    }
}

impl Huff {
    fn build(&mut self, counts: &[u8; 16], symbols: &[u8]) {
        self.counts = [0; 17];
        self.counts[1..17].copy_from_slice(counts);
        self.symbols = [0; 256];
        let n = symbols.len().min(256);
        self.symbols[..n].copy_from_slice(&symbols[..n]);
        self.set = true;
    }

    /// One code, most significant bit first.
    fn decode(&self, e: &mut Entropy) -> Result<u8, &'static str> {
        if !self.set {
            return Err("a JPEG scan that uses a Huffman table it never declared");
        }
        let mut code = 0i32;
        let mut first = 0i32;
        let mut index = 0i32;
        for len in 1..17 {
            code |= e.bit()? as i32;
            let count = self.counts[len] as i32;
            if code - first < count {
                return Ok(self.symbols[(index + code - first) as usize]);
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        Err("no JPEG Huffman code matches those bits")
    }
}

/// The entropy-coded bytes of one scan, with the stuffing undone.
///
/// Doing that up front rather than inside the bit reader is what makes restart
/// markers tractable: they are the only thing in a scan's data that is not a
/// byte of that data, and a reader that has to watch for 0xFF on every byte is
/// both slower and harder to follow than one handed a clean stream and a list
/// of places to jump to.
struct Entropy {
    data: Vec<u8>,
    rst: Vec<usize>,
    at: usize,
    buf: u32,
    n: u32,
    next_rst: usize,
}

impl Entropy {
    fn new(raw: &[u8]) -> Entropy {
        let mut data = Vec::with_capacity(raw.len());
        let mut rst = Vec::new();
        let mut i = 0usize;
        while i < raw.len() {
            if raw[i] != 0xFF {
                data.push(raw[i]);
                i += 1;
                continue;
            }
            match raw.get(i + 1) {
                Some(0x00) => {
                    data.push(0xFF);
                    i += 2;
                }
                Some(0xD0..=0xD7) => {
                    // A restart marker is a byte boundary by definition, so
                    // the length of the clean stream up to here is where the
                    // reader resumes.
                    rst.push(data.len());
                    i += 2;
                }
                Some(0xFF) => i += 1,               // a fill byte, not a marker
                _ => break,                         // any other marker ends the scan
            }
        }
        Entropy { data, rst, at: 0, buf: 0, n: 0, next_rst: 0 }
    }

    /// Keep the top of the word stocked.  Past the end of the data this feeds
    /// zeroes: a scan's last byte is padded by the encoder and the padding is
    /// not information, and the number of blocks is known independently, so
    /// running out is a termination rather than a failure.
    fn fill(&mut self) {
        while self.n <= 24 {
            let b = if self.at < self.data.len() { self.data[self.at] } else { 0 };
            self.at += 1;
            self.buf |= (b as u32) << (24 - self.n);
            self.n += 8;
        }
    }

    fn bit(&mut self) -> Result<u32, &'static str> {
        if self.n == 0 {
            self.fill();
        }
        let v = (self.buf >> 31) & 1;
        self.buf <<= 1;
        self.n -= 1;
        Ok(v)
    }

    fn take(&mut self, want: u32) -> Result<u32, &'static str> {
        let mut v = 0u32;
        for _ in 0..want {
            v = (v << 1) | self.bit()?;
        }
        Ok(v)
    }

    /// Jump to the next restart marker, discarding whatever is left in the
    /// current stretch of entropy data.
    fn restart(&mut self) -> Result<(), &'static str> {
        if self.next_rst >= self.rst.len() {
            return Err("a JPEG that runs out of restart markers");
        }
        self.at = self.rst[self.next_rst];
        self.next_rst += 1;
        self.buf = 0;
        self.n = 0;
        Ok(())
    }
}

/// One component of the picture.
struct Comp {
    /// The identifier the frame gave it, which is not its position: real files
    /// number their components 1, 2, 3 or 'R', 'G', 'B'.
    id: u8,
    hs: usize,
    vs: usize,
    tq: usize,
    /// Blocks across and down at this component's own resolution — the ones
    /// that are on the picture.
    bw: usize,
    bh: usize,
    /// The same, rounded up to whole MCUs, which is the grid the encoder laid
    /// the coefficients out on and therefore the stride of the plane below.
    /// It is never smaller than `bw` and `bh`.
    bw_pad: usize,
    bh_pad: usize,
    cw: usize,
    ch: usize,
    pred: i32,
    coeffs: Vec<i16>,
}

impl Comp {
    fn block(&mut self, bx: usize, by: usize) -> &mut [i16] {
        let at = (by * self.bw_pad + bx) * 64;
        &mut self.coeffs[at..at + 64]
    }
}

/// One component's place in the scan being decoded.
struct ScanComp {
    ci: usize,
    dc: Huff,
    ac: Huff,
    hs: usize,
    vs: usize,
    bw: usize,
    bh: usize,
}

struct Decoder {
    qt: [Quant; 4],
    dc: [Huff; 4],
    ac: [Huff; 4],
    comps: Vec<Comp>,
    w: usize,
    h: usize,
    hmax: usize,
    vmax: usize,
    progressive: bool,
    dri: usize,
    /// Adobe's colour transform from the APP14 segment: 0 none, 1 YCbCr,
    /// 2 YCCK.  Absent means the file is JFIF, and a three-component JFIF file
    /// is always YCbCr.
    adobe: Option<u8>,
}

/// Decode a JPEG into RGB.
pub fn decode(src: &[u8], budget: usize, _bg: (u8, u8, u8)) -> Result<Image, &'static str> {
    let mut d = Decoder {
        qt: [Quant::default(); 4],
        dc: [Huff::default(); 4],
        ac: [Huff::default(); 4],
        comps: Vec::new(),
        w: 0,
        h: 0,
        hmax: 1,
        vmax: 1,
        progressive: false,
        dri: 0,
        adobe: None,
    };

    let mut at = 2usize;                            // past SOI
    loop {
        // Find the next marker.  A byte that is not 0xFF here is either
        // padding or a file that has lost its place.
        while at < src.len() && src[at] != 0xFF {
            at += 1;
        }
        while at < src.len() && src[at] == 0xFF {
            at += 1;
        }
        let Some(&marker) = src.get(at) else {
            return Err("a JPEG that ends before its last scan");
        };
        at += 1;
        // The end of the image is the end of the loop, and the only place
        // the loop has one: everything before this point was a segment with a
        // length or a scan that ran to the next marker.
        if marker == 0xD9 {
            break;
        }
        // Stuffing and restart markers carry no length.
        if marker == 0x00 || (0xD0..=0xD8).contains(&marker) || marker == 0x01 {
            continue;
        }
        let len = match (src.get(at), src.get(at + 1)) {
            (Some(&hi), Some(&lo)) => ((hi as usize) << 8) | lo as usize,
            _ => return Err("a JPEG segment with no length"),
        };
        if len < 2 || at + len > src.len() {
            return Err("a JPEG segment that runs past the end");
        }
        let body = &src[at + 2..at + len];
        match marker {
            0xC0 | 0xC1 => frame(&mut d, body, false, budget)?,
            0xC2 => frame(&mut d, body, true, budget)?,
            // Extended sequential, lossless, arithmetic and hierarchical all
            // exist, and none of them is worth a decoder here.
            0xC3 | 0xC5..=0xCB | 0xCD..=0xCF => {
                return Err("a JPEG mode this decoder does not implement");
            }
            0xC4 => dht(&mut d, body)?,
            0xDB => dqt(&mut d, body)?,
            0xDD => {
                if body.len() >= 2 {
                    d.dri = ((body[0] as usize) << 8) | body[1] as usize;
                }
            }
            0xEE => {
                if body.len() >= 12 && body.starts_with(b"Adobe") {
                    d.adobe = Some(body[11]);
                }
            }
            0xDA => {
                if d.comps.is_empty() {
                    return Err("a JPEG scan before its frame header");
                }
                let e = if at + len < src.len() { Entropy::new(&src[at + len..]) } else { Entropy::new(&[]) };
                decode_scan(&mut d, body, e)?;
                // Resume after the entropy data.  The reader stopped at the
                // first marker that was not stuffing or a restart, and finding
                // that marker again from the raw bytes is the only honest way
                // to know where the scan actually ended.
                at = scan_data_end(src, at + len);
                continue;
            }
            _ => {}
        }
        at += len;
    }

    finish(d, budget)
}

/// Where the entropy data of a scan ends, by walking it the way `Entropy::new`
/// does.  The outer loop needs the same answer, and asking the same question
/// twice beats threading a position back out of the reader.
fn scan_data_end(src: &[u8], from: usize) -> usize {
    let mut i = from;
    while i < src.len() {
        if src[i] != 0xFF {
            i += 1;
            continue;
        }
        match src.get(i + 1) {
            Some(0x00) | Some(0xD0..=0xD7) => i += 2,
            Some(0xFF) => i += 1,
            _ => return i,
        }
    }
    src.len()
}

fn frame(d: &mut Decoder, body: &[u8], progressive: bool, budget: usize) -> Result<(), &'static str> {
    if body.len() < 6 {
        return Err("a JPEG frame header that is too short");
    }
    if body[0] != 8 {
        return Err("a JPEG with samples wider than eight bits");
    }
    d.h = ((body[1] as usize) << 8) | body[2] as usize;
    d.w = ((body[3] as usize) << 8) | body[4] as usize;
    let n = body[5] as usize;
    if d.w == 0 || d.h == 0 {
        return Err("a JPEG with no pixels in it");
    }
    // Two components is not a colour space anybody uses; four is Adobe's
    // separation, which turns up in print exports.
    if n != 1 && n != 3 && n != 4 {
        return Err("a JPEG with a number of components this decoder cannot interpret");
    }
    if body.len() < 6 + n * 3 {
        return Err("a JPEG frame header that stops before its components");
    }
    let want = d
        .w
        .checked_mul(d.h)
        .and_then(|v| v.checked_mul(3))
        .ok_or("a JPEG whose dimensions overflow")?;
    if want > budget {
        return Err("a JPEG that decodes to more bytes than there is room for");
    }

    d.progressive = progressive;
    d.comps.clear();
    d.hmax = 1;
    d.vmax = 1;
    for i in 0..n {
        let id = body[6 + i * 3];
        let hs = (body[6 + i * 3 + 1] >> 4) as usize;
        let vs = (body[6 + i * 3 + 1] & 15) as usize;
        if hs == 0 || vs == 0 || hs > 4 || vs > 4 {
            return Err("a JPEG sampling factor that is not between one and four");
        }
        let tq = body[6 + i * 3 + 2] as usize;
        if tq >= 4 {
            return Err("a JPEG frame that names a quantisation table that cannot exist");
        }
        d.hmax = d.hmax.max(hs);
        d.vmax = d.vmax.max(vs);
        d.comps.push(Comp {
            id,
            hs,
            vs,
            tq,
            bw: 0,
            bh: 0,
            bw_pad: 0,
            bh_pad: 0,
            cw: 0,
            ch: 0,
            pred: 0,
            coeffs: Vec::new(),
        });
    }
    // How many MCUs the picture is, which is what an interleaved scan walks.
    let mcus_x = (d.w + 8 * d.hmax - 1) / (8 * d.hmax);
    let mcus_y = (d.h + 8 * d.vmax - 1) / (8 * d.vmax);

    // A component's own resolution is the picture's, scaled by how finely it
    // is sampled relative to the finest one.
    for i in 0..d.comps.len() {
        let (hs, vs) = (d.comps[i].hs, d.comps[i].vs);
        let cw = (d.w * hs + d.hmax - 1) / d.hmax;
        let ch = (d.h * vs + d.vmax - 1) / d.vmax;
        let c = &mut d.comps[i];
        c.cw = cw;
        c.ch = ch;
        c.bw = (cw + 7) / 8;
        c.bh = (ch + 7) / 8;
        // An MCU holds the same number of blocks wherever it falls, so the
        // last column and the last row of MCUs are full of blocks that are
        // partly or wholly off the picture.  They are still encoded and still
        // have to be read: skipping them — which is the obvious thing to do,
        // since their pixels are never shown — leaves the Huffman decoder
        // reading an AC value where a run length should be, and everything
        // after it is noise.  So the plane is the padded grid, and the blocks
        // outside the picture are decoded into it and then ignored.
        c.bw_pad = c.bw.max(mcus_x * hs);
        c.bh_pad = c.bh.max(mcus_y * vs);
        // The coefficients for the whole picture are held at once, because a
        // progressive file sends them in bands and there is nowhere else to
        // keep the ones that arrived in an earlier scan.
        c.coeffs = alloc::vec![0i16; c.bw_pad * c.bh_pad * 64];
    }
    Ok(())
}

fn dqt(d: &mut Decoder, body: &[u8]) -> Result<(), &'static str> {
    let mut i = 0usize;
    while i < body.len() {
        let pq = body[i] >> 4;
        let tq = (body[i] & 15) as usize;
        i += 1;
        if tq >= 4 || pq > 1 {
            return Err("a JPEG quantisation table this decoder does not know");
        }
        let need = if pq == 0 { 64 } else { 128 };
        if i + need > body.len() {
            return Err("a JPEG quantisation table that runs past the end");
        }
        for k in 0..64 {
            let v = if pq == 0 {
                body[i + k] as u16
            } else {
                ((body[i + k * 2] as u16) << 8) | body[i + k * 2 + 1] as u16
            };
            d.qt[tq].v[ZIGZAG[k]] = v;
        }
        d.qt[tq].set = true;
        i += need;
    }
    Ok(())
}

fn dht(d: &mut Decoder, body: &[u8]) -> Result<(), &'static str> {
    let mut i = 0usize;
    while i < body.len() {
        let tc = body[i] >> 4;
        let th = (body[i] & 15) as usize;
        i += 1;
        if th >= 4 || tc > 1 || i + 16 > body.len() {
            return Err("a JPEG Huffman table this decoder does not know");
        }
        let mut counts = [0u8; 16];
        counts.copy_from_slice(&body[i..i + 16]);
        i += 16;
        let total: usize = counts.iter().map(|&c| c as usize).sum();
        if i + total > body.len() {
            return Err("a JPEG Huffman table that runs past the end");
        }
        if tc == 0 {
            d.dc[th].build(&counts, &body[i..i + total]);
        } else {
            d.ac[th].build(&counts, &body[i..i + total]);
        }
        i += total;
    }
    Ok(())
}

/// Read `n` bits and sign-extend them, which is how JPEG sends a value that
/// may be negative without spending a bit on the sign.
fn receive(e: &mut Entropy, n: u32) -> Result<i32, &'static str> {
    if n == 0 {
        return Ok(0);
    }
    if n > 16 {
        return Err("a JPEG coefficient wider than its own format allows");
    }
    let v = e.take(n)? as i32;
    if v < (1 << (n - 1)) {
        Ok(v - (1 << n) + 1)
    } else {
        Ok(v)
    }
}

/// One scan, from its header to the end of its entropy data.
fn decode_scan(d: &mut Decoder, header: &[u8], mut e: Entropy) -> Result<(), &'static str> {
    if header.is_empty() {
        return Err("a JPEG scan header with no components in it");
    }
    let ns = header[0] as usize;
    if ns == 0 || ns > d.comps.len() || header.len() < 1 + ns * 2 + 3 {
        return Err("a JPEG scan that names components the frame does not have");
    }
    let mut list: Vec<ScanComp> = Vec::with_capacity(ns);
    for i in 0..ns {
        let id = header[1 + i * 2];
        let td = (header[1 + i * 2 + 1] >> 4) as usize;
        let ta = (header[1 + i * 2 + 1] & 15) as usize;
        if td >= 4 || ta >= 4 {
            return Err("a JPEG scan that names a Huffman table that cannot exist");
        }
        let ci = d
            .comps
            .iter()
            .position(|c| c.id == id)
            .ok_or("a JPEG scan that names a component the frame does not have")?;
        let c = &d.comps[ci];
        list.push(ScanComp {
            ci,
            dc: d.dc[td],
            ac: d.ac[ta],
            hs: c.hs,
            vs: c.vs,
            bw: c.bw,
            bh: c.bh,
        });
    }
    let p = &header[1 + ns * 2..];
    let ss = p[0] as usize;
    let se = p[1] as usize;
    let ah = (p[2] >> 4) as u32;
    let al = (p[2] & 15) as u32;
    if se > 63 || ss > se {
        return Err("a JPEG scan with a spectral range that is not a range");
    }

    // The end-of-band run is shared by every block in the scan, which is how a
    // progressive file spends almost nothing on the flat parts of a picture.
    let mut eobrun = 0u32;
    let mut done = 0usize;

    if ns > 1 {
        // Interleaved: the picture is walked in MCUs, the largest block the
        // sampling factors describe, and every component contributes its share
        // of blocks to each one.
        let mx = (d.w + 8 * d.hmax - 1) / (8 * d.hmax);
        let my = (d.h + 8 * d.vmax - 1) / (8 * d.vmax);
        for my_i in 0..my {
            for mx_i in 0..mx {
                if d.dri != 0 && done != 0 && done % d.dri == 0 {
                    e.restart()?;
                    for c in d.comps.iter_mut() {
                        c.pred = 0;
                    }
                }
                done += 1;
                for sc in &list {
                    for v in 0..sc.vs {
                        for h in 0..sc.hs {
                            // Every block of the MCU, including the ones off
                            // the edge of the picture.
                            decode_block(
                                &mut d.comps[sc.ci], sc, mx_i * sc.hs + h, my_i * sc.vs + v,
                                ss, se, ah, al, d.progressive, &mut eobrun, &mut e,
                            )?;
                        }
                    }
                }
            }
        }
    } else {
        // One component, walked as its own blocks: a scan wraps at the
        // component's width, not the picture's, and for a subsampled
        // component those are not the same number.
        let sc = &list[0];
        for by in 0..sc.bh {
            for bx in 0..sc.bw {
                if d.dri != 0 && done != 0 && done % d.dri == 0 {
                    e.restart()?;
                    d.comps[sc.ci].pred = 0;
                }
                done += 1;
                decode_block(
                    &mut d.comps[sc.ci], sc, bx, by, ss, se, ah, al,
                    d.progressive, &mut eobrun, &mut e,
                )?;
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn decode_block(
    c: &mut Comp,
    sc: &ScanComp,
    bx: usize,
    by: usize,
    ss: usize,
    se: usize,
    ah: u32,
    al: u32,
    progressive: bool,
    eobrun: &mut u32,
    e: &mut Entropy,
) -> Result<(), &'static str> {
    // The DC coefficient comes first in every block, and belongs to the scan
    // whenever the scan's band starts at it — which is every sequential file
    // and every progressive file's first scan.  A progressive AC scan names a
    // band that starts higher and leaves the DC where it is.
    if ss == 0 {
        if ah == 0 {
            let t = sc.dc.decode(e)? as u32;
            if t > 16 {
                return Err("a JPEG DC difference wider than its own format allows");
            }
            let diff = receive(e, t)?;
            c.pred += diff;
            let v = (c.pred << al) as i16;
            let block = c.block(bx, by);
            block[0] = v;
        } else if e.bit()? != 0 {
            // A later pass adds one bit to the low end of the DC value.  The
            // bit is combined into the two's-complement word, which raises the
            // magnitude of a negative value as much as it does a positive one.
            let block = c.block(bx, by);
            block[0] |= (1i16) << al;
        }
    }
    // A DC-only scan ends here; anything else has a band of AC coefficients to
    // read as well.
    if se == 0 {
        return Ok(());
    }

    if !progressive {
        return sequential_band(c, sc, bx, by, ss, se, al, e);
    }
    if ah == 0 {
        ac_first(c, sc, bx, by, ss, se, al, eobrun, e)
    } else {
        ac_refine(c, sc, bx, by, ss, se, al, eobrun, e)
    }
}

/// A run-length coded band of AC coefficients: how many zeros to skip, then
/// how many bits the value occupies.  This is what a sequential file uses for
/// its single band, and it is the shape every other band coding is a variation
/// on.
fn sequential_band(
    c: &mut Comp,
    sc: &ScanComp,
    bx: usize,
    by: usize,
    ss: usize,
    se: usize,
    al: u32,
    e: &mut Entropy,
) -> Result<(), &'static str> {
    let block = c.block(bx, by);
    // A sequential scan reports its band as starting at zero, but zero is the
    // DC coefficient and the DC has already been read.  Starting the walk
    // there overwrites it with the first AC value and shifts everything after
    // it by one position.
    let mut k = if ss == 0 { 1 } else { ss };
    while k <= se {
        let rs = sc.ac.decode(e)?;
        let r = (rs >> 4) as usize;
        let s = (rs & 15) as u32;
        if s == 0 {
            if r != 15 {
                // End of band.  A sequential file has only one band, so this
                // is the end of the block.
                break;
            }
            k += 16;
            continue;
        }
        k += r;
        if k > se {
            return Err("a JPEG run of zeros that runs past the end of a band");
        }
        let v = receive(e, s)?;
        block[ZIGZAG[k]] = (v << al) as i16;
        k += 1;
    }
    Ok(())
}

/// A progressive scan that introduces the AC coefficients of a band, one
/// end-of-band run covering a stretch of blocks at a time.
fn ac_first(
    c: &mut Comp,
    sc: &ScanComp,
    bx: usize,
    by: usize,
    ss: usize,
    se: usize,
    al: u32,
    eobrun: &mut u32,
    e: &mut Entropy,
) -> Result<(), &'static str> {
    if ss == 0 {
        return Err("a progressive AC scan that starts at the DC coefficient");
    }
    if *eobrun > 0 {
        // This block is inside a run of ones the encoder said were all zero.
        *eobrun -= 1;
        return Ok(());
    }
    let block = c.block(bx, by);
    let mut k = ss;
    while k <= se {
        let rs = sc.ac.decode(e)?;
        let r = (rs >> 4) as usize;
        let s = (rs & 15) as u32;
        if s == 0 {
            if r != 15 {
                // An end-of-band run: two to the r, plus the bits that follow.
                // The block that emits it counts as the first of the run,
                // which is why it is decremented here.
                *eobrun = (1u32 << r) + if r > 0 { e.take(r as u32)? } else { 0 };
                *eobrun -= 1;
                break;
            }
            k += 16;
            continue;
        }
        k += r;
        if k > se {
            return Err("a JPEG run of zeros that runs past the end of a band");
        }
        let v = receive(e, s)?;
        block[ZIGZAG[k]] = (v << al) as i16;
        k += 1;
    }
    Ok(())
}

/// A progressive scan that sharpens what an earlier pass put down.
///
/// This is the part of progressive JPEG that is genuinely unlike anything
/// else.  The bits arriving now are mostly not values but corrections — one
/// per coefficient that is already non-zero, saying whether its magnitude
/// should grow — interleaved with occasional brand new coefficients of exactly
/// one quantum.  Reading them in the wrong order gives a picture that is
/// recognisably the right picture and wrong everywhere, so the loop below
/// follows libjpeg's rather than being rebuilt from the text of the
/// specification.
fn ac_refine(
    c: &mut Comp,
    sc: &ScanComp,
    bx: usize,
    by: usize,
    ss: usize,
    se: usize,
    al: u32,
    eobrun: &mut u32,
    e: &mut Entropy,
) -> Result<(), &'static str> {
    let p1 = 1i16 << al;
    let m1 = -1i16 << al;
    let block = c.block(bx, by);
    let mut k = ss;

    if *eobrun == 0 {
        while k <= se {
            let rs = sc.ac.decode(e)?;
            let mut r = (rs >> 4) as i32;
            let s = (rs & 15) as u32;
            let mut newval = 0i16;
            if s == 0 {
                if r != 15 {
                    *eobrun = (1u32 << r) + if r > 0 { e.take(r as u32)? } else { 0 };
                    break;
                }
            } else {
                newval = if e.bit()? != 0 { p1 } else { m1 };
            }
            // Step over the coefficients that are already set, correcting
            // them, and over `r` that are still zero.
            loop {
                if k > se {
                    break;
                }
                let idx = ZIGZAG[k];
                if block[idx] != 0 {
                    if e.bit()? != 0 && (block[idx] & p1) == 0 {
                        block[idx] += if block[idx] >= 0 { p1 } else { m1 };
                    }
                } else {
                    r -= 1;
                    if r < 0 {
                        break;
                    }
                }
                k += 1;
            }
            if newval != 0 && k <= se {
                block[ZIGZAG[k]] = newval;
            }
            k += 1;
        }
    }

    if *eobrun > 0 {
        // The rest of this band carries nothing but corrections.
        while k <= se {
            let idx = ZIGZAG[k];
            if block[idx] != 0 && e.bit()? != 0 && (block[idx] & p1) == 0 {
                block[idx] += if block[idx] >= 0 { p1 } else { m1 };
            }
            k += 1;
        }
        *eobrun -= 1;
    }
    Ok(())
}

/// Turn coefficients into pixels: an inverse transform per block, then the
/// colour conversion, then the sampling factors undone by copying samples up.
fn finish(d: Decoder, budget: usize) -> Result<Image, &'static str> {
    let want = d
        .w
        .checked_mul(d.h)
        .and_then(|n| n.checked_mul(3))
        .ok_or("a JPEG whose dimensions overflow")?;
    if want > budget {
        return Err("a JPEG that decodes to more bytes than there is room for");
    }
    let qt = d.qt;
    let mut planes: Vec<Vec<u8>> = Vec::with_capacity(d.comps.len());
    for ci in 0..d.comps.len() {
        let c = &d.comps[ci];
        if !qt[c.tq].set {
            return Err("a JPEG that uses a quantisation table it never declared");
        }
        let mut p = plane(c.cw, c.ch)?;
        let mut block = [0i32; 64];
        let mut out = [0u8; 64];
        for by in 0..c.bh {
            for bx in 0..c.bw {
                let src = &c.coeffs[(by * c.bw_pad + bx) * 64..(by * c.bw_pad + bx) * 64 + 64];
                for k in 0..64 {
                    block[k] = src[k] as i32 * qt[c.tq].v[k] as i32;
                }
                idct(&block, &mut out);
                for yy in 0..8 {
                    let py = by * 8 + yy;
                    if py >= c.ch {
                        break;
                    }
                    for xx in 0..8 {
                        let px = bx * 8 + xx;
                        if px >= c.cw {
                            break;
                        }
                        p[py * c.cw + px] = out[yy * 8 + xx];
                    }
                }
            }
        }
        // A whole-number ratio, or nothing: a component whose sampling factor
        // does not divide into the finest one has no correct filter, only a
        // defensible one.
        let (rx, ry) = if c.hs != 0 && c.vs != 0 && d.hmax % c.hs == 0 && d.vmax % c.vs == 0 {
            (d.hmax / c.hs, d.vmax / c.vs)
        } else {
            (0, 0)
        };
        if rx * ry <= 1 {
            planes.push(nearest(&p, c.cw, c.ch, d.w, d.h));
        } else {
            planes.push(to_full(&p, c.cw, c.ch, d.w, d.h, rx, ry));
        }
    }

    let n = d.comps.len();
    let mut img = plane(d.w, d.h)?;
    for y in 0..d.h {
        for x in 0..d.w {
            let at = y * d.w + x;
            let s = |ci: usize| -> u8 { planes[ci].get(at).copied().unwrap_or(0) };
            let rgb = if n == 1 {
                let g = s(0);
                (g, g, g)
            } else if n == 4 {
                cmyk(s(0), s(1), s(2), s(3), d.adobe)
            } else {
                ycbcr(s(0), s(1), s(2), d.adobe == Some(0))
            };
            let o = at * 3;
            img[o] = rgb.0;
            img[o + 1] = rgb.1;
            img[o + 2] = rgb.2;
        }
    }
    Ok(Image { w: d.w, h: d.h, px: img })
}

/// Bring a component to the picture's own size.
///
/// A component sampled at half resolution is not simply doubled.  JPEG puts
/// each chroma sample on the *first* pixel of the pair it covers rather than
/// between the two, so repeating it shifts every colour edge half a pixel to
/// the left, and doubling each sample leaves visible squares of flat colour.
/// libjpeg calls the correction "fancy upsampling"; it is a triangle filter
/// with weights of three quarters and a quarter, which is the value a pixel
/// halfway between two samples should have when the samples are not halfway
/// between the pixels.
fn to_full(p: &[u8], sw: usize, sh: usize, w: usize, h: usize, rx: usize, ry: usize) -> Vec<u8> {
    // Across first, into a plane of the right width and the component's own
    // height.
    let mid_w = sw * rx;
    let mut mid = alloc::vec![0u8; mid_w * sh];
    for y in 0..sh {
        for x in 0..sw {
            let cur = p[y * sw + x];
            if rx == 2 {
                let next = if x + 1 < sw { p[y * sw + x + 1] } else { cur };
                mid[y * mid_w + 2 * x] = cur;
                mid[y * mid_w + 2 * x + 1] = ((cur as u32 * 3 + next as u32 + 2) >> 2) as u8;
            } else {
                for k in 0..rx {
                    let t = x * rx + k;
                    if t < mid_w {
                        mid[y * mid_w + t] = cur;
                    }
                }
            }
        }
    }
    // Then down.
    let mut out = alloc::vec![0u8; w * h];
    for x in 0..mid_w.min(w) {
        for y in 0..sh {
            let cur = mid[y * mid_w + x];
            if ry == 2 {
                let next = if y + 1 < sh { mid[(y + 1) * mid_w + x] } else { cur };
                if 2 * y < h {
                    out[(2 * y) * w + x] = cur;
                }
                if 2 * y + 1 < h {
                    out[(2 * y + 1) * w + x] = ((cur as u32 * 3 + next as u32 + 2) >> 2) as u8;
                }
            } else {
                for k in 0..ry {
                    let ty = y * ry + k;
                    if ty < h {
                        out[ty * w + x] = cur;
                    }
                }
            }
        }
    }
    out
}

/// A plain resize, for sampling factors that are not whole multiples of one
/// another.
fn nearest(p: &[u8], sw: usize, sh: usize, w: usize, h: usize) -> Vec<u8> {
    let mut out = alloc::vec![0u8; w * h];
    if sw == w && sh == h {
        let n = (w * h).min(p.len());
        out[..n].copy_from_slice(&p[..n]);
        return out;
    }
    for y in 0..h {
        let sy = (y * sh / h).min(sh.saturating_sub(1));
        for x in 0..w {
            let sx = (x * sw / w).min(sw.saturating_sub(1));
            out[y * w + x] = p.get(sy * sw + sx).copied().unwrap_or(0);
        }
    }
    out
}

/// YCbCr to RGB, in fixed point.  The constants are the ones the specification
/// recommends, scaled by two to the sixteenth.
fn ycbcr(y: u8, cb: u8, cr: u8, as_rgb: bool) -> (u8, u8, u8) {
    if as_rgb {
        return (y, cb, cr);
    }
    let y = y as i32;
    let cb = cb as i32 - 128;
    let cr = cr as i32 - 128;
    let r = y + ((91881 * cr) >> 16);
    let g = y - ((22554 * cb + 46802 * cr) >> 16);
    let b = y + ((116130 * cb) >> 16);
    (clamp(r), clamp(g), clamp(b))
}

/// Four components are Adobe's separation.  With a transform of 2 the first
/// three are YCbCr of the inverse of the ink, and with no transform they are
/// the ink itself; either way the black plate multiplies the result down.
fn cmyk(c: u8, m: u8, y: u8, k: u8, adobe: Option<u8>) -> (u8, u8, u8) {
    let (c, m, y) = if adobe == Some(2) {
        let a = ycbcr(c, m, y, false);
        (255 - a.0, 255 - a.1, 255 - a.2)
    } else {
        (c, m, y)
    };
    let k = k as u32;
    let ink = |v: u8| -> u8 { ((v as u32 * k) / 255) as u8 };
    (ink(c), ink(m), ink(y))
}

fn clamp(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

/// The inverse discrete cosine transform, in fixed point.
///
/// Two passes of the table above give the specification's double sum scaled by
/// two to the twenty-fourth, and the transform carries a leading factor of a
/// quarter on top of that — so the value wanted is the accumulation shifted by
/// twenty-six, and there is no floating point anywhere near a kernel that has
/// no maths library.
fn idct(coef: &[i32; 64], out: &mut [u8; 64]) {
    // A block whose alternating coefficients are all zero is flat, which is
    // most of a photograph's sky and all of a large rectangle of colour.
    if coef[1..].iter().all(|&v| v == 0) {
        out.fill(clamp(((coef[0] + 4) >> 3) + 128));
        return;
    }
    let mut tmp = [0i64; 64];
    for v in 0..8 {
        for x in 0..8 {
            let mut s = 0i64;
            for u in 0..8 {
                s += coef[v * 8 + u] as i64 * T[u][x] as i64;
            }
            tmp[v * 8 + x] = s;
        }
    }
    for y in 0..8 {
        for x in 0..8 {
            let mut s = 0i64;
            for v in 0..8 {
                s += tmp[v * 8 + x] * T[v][y] as i64;
            }
            out[y * 8 + x] = clamp(((s + (1 << 25)) >> 26) as i32 + 128);
        }
    }
}
