//! PNG (RFC 2083), which is what most of a page's icons and logos are.
//!
//! Every colour type and bit depth the specification defines is here, and so
//! is Adam7 interlacing — the seven-pass scheme that lets a picture appear
//! half-finished rather than not at all.  Interlacing is rare on the web,
//! because every optimiser strips it, and it is the part most decoders get
//! wrong; the saving grace is that a pass is just the same row loop over a
//! different set of pixels, so supporting it is a loop around the code that
//! would have to exist anyway rather than a second decoder.
//!
//! The chunk CRCs are not checked.  They exist to catch a file damaged in
//! transit, and a picture that is slightly wrong is a better outcome here than
//! a page that loses its images because a byte flipped — the reader can see
//! that something is off, which is more than they can do with an error
//! message.

use super::inflate;
use super::{over, plane, Image};
use alloc::vec::Vec;

/// The seven Adam7 passes: the first pixel of the pass, and the step between
/// pixels and between rows.
const PASSES: [(usize, usize, usize, usize); 7] = [
    (0, 0, 8, 8),
    (4, 0, 8, 8),
    (0, 4, 4, 8),
    (2, 0, 4, 4),
    (0, 2, 2, 4),
    (1, 0, 2, 2),
    (0, 1, 1, 2),
];

#[derive(Default, Clone, Copy)]
struct Header {
    w: usize,
    h: usize,
    depth: u8,
    color: u8,
    interlace: u8,
}

impl Header {
    fn channels(&self) -> usize {
        match self.color {
            0 | 3 => 1,
            2 => 3,
            4 => 2,
            _ => 4,
        }
    }

    fn bits_per_pixel(&self) -> usize {
        self.channels() * self.depth as usize
    }

    /// The filter's idea of a pixel: the smallest whole number of bytes a
    /// pixel occupies.  For a one-bit picture that is one byte, because a
    /// filter works on bytes and cannot shift by a fraction of one.
    fn filter_step(&self) -> usize {
        (self.bits_per_pixel() + 7) / 8
    }
}

/// Decode a PNG into RGB.
pub fn decode(src: &[u8], budget: usize, bg: (u8, u8, u8)) -> Result<Image, &'static str> {
    let mut head: Option<Header> = None;
    let mut palette: Vec<(u8, u8, u8)> = Vec::new();
    let mut trns: Vec<u8> = Vec::new();
    let mut idat: Vec<u8> = Vec::new();

    let mut at = 8usize;
    loop {
        if at + 8 > src.len() {
            return Err("a PNG that ends before its last chunk");
        }
        let len = be32(&src[at..at + 4]);
        let kind = &src[at + 4..at + 8];
        let body = at + 8;
        if body + len + 4 > src.len() {
            return Err("a PNG chunk that runs past the end of the file");
        }
        let chunk = &src[body..body + len];
        match kind {
            b"IHDR" => {
                if len < 13 {
                    return Err("a PNG header that is too short to be one");
                }
                head = Some(Header {
                    w: be32(&chunk[0..4]),
                    h: be32(&chunk[4..8]),
                    depth: chunk[8],
                    color: chunk[9],
                    interlace: chunk[12],
                });
            }
            b"PLTE" => {
                for c in chunk.chunks_exact(3) {
                    palette.push((c[0], c[1], c[2]));
                }
            }
            b"tRNS" => trns.extend_from_slice(chunk),
            b"IDAT" => idat.extend_from_slice(chunk),
            b"IEND" => break,
            _ => {}
        }
        at = body + len + 4;
    }

    let head = head.ok_or("a PNG with no header chunk")?;
    if head.w == 0 || head.h == 0 {
        return Err("a PNG with no pixels in it");
    }
    if head.interlace > 1 {
        return Err("a PNG with an interlace method that is not 0 or 1");
    }
    if !depth_ok(head.color, head.depth) {
        return Err("a PNG colour type and bit depth that do not go together");
    }
    // Checked before anything is allocated: the header is four bytes per side
    // and can describe a picture far larger than the machine.
    let want = head
        .w
        .checked_mul(head.h)
        .and_then(|n| n.checked_mul(3))
        .ok_or("a PNG whose dimensions overflow")?;
    if want > budget {
        return Err("a PNG that decodes to more bytes than there is room for");
    }

    // The raw stream is the filtered rows, which is a little more than the
    // pixels because every row carries a filter byte.
    let raw_cap = raw_size(&head)?;
    let raw = inflate::zlib(&idat, raw_cap)?;

    let mut out = plane(head.w, head.h)?;
    if head.interlace == 1 {
        let mut at = 0usize;
        for &(x0, y0, dx, dy) in PASSES.iter() {
            if x0 >= head.w || y0 >= head.h {
                continue;                       // a pass this picture has none of
            }
            let pw = (head.w - x0 + dx - 1) / dx;
            let ph = (head.h - y0 + dy - 1) / dy;
            unfilter_pass(&head, &raw, &mut at, &mut Pass {
                pw, ph, x0, y0, dx, dy,
                palette: &palette, trns: &trns, out: &mut out, bg,
            })?;
        }
    } else {
        let mut at = 0usize;
        unfilter_pass(&head, &raw, &mut at, &mut Pass {
            pw: head.w, ph: head.h, x0: 0, y0: 0, dx: 1, dy: 1,
            palette: &palette, trns: &trns, out: &mut out, bg,
        })?;
    }
    Ok(Image { w: head.w, h: head.h, px: out })
}

/// Is this a colour type and bit depth the specification allows together?
fn depth_ok(color: u8, depth: u8) -> bool {
    match color {
        0 => matches!(depth, 1 | 2 | 4 | 8 | 16),
        3 => matches!(depth, 1 | 2 | 4 | 8),
        2 | 4 | 6 => matches!(depth, 8 | 16),
        _ => false,
    }
}

/// Upper bound on the filtered stream: rows, each padded to a byte boundary,
/// plus one filter byte each.
fn raw_size(head: &Header) -> Result<usize, &'static str> {
    let row = |w: usize| (w * head.bits_per_pixel() + 7) / 8 + 1;
    if head.interlace == 0 {
        return head
            .h
            .checked_mul(row(head.w))
            .ok_or("a PNG whose rows overflow");
    }
    let mut total = 0usize;
    for &(x0, y0, dx, dy) in PASSES.iter() {
        if x0 >= head.w || y0 >= head.h {
            continue;
        }
        let pw = (head.w - x0 + dx - 1) / dx;
        let ph = (head.h - y0 + dy - 1) / dy;
        total += ph * row(pw);
    }
    Ok(total)
}

/// One pass's worth of work, which for a non-interlaced picture is all of it.
struct Pass<'a> {
    pw: usize,
    ph: usize,
    x0: usize,
    y0: usize,
    dx: usize,
    dy: usize,
    palette: &'a [(u8, u8, u8)],
    trns: &'a [u8],
    out: &'a mut [u8],
    bg: (u8, u8, u8),
}

/// Undo the row filters and turn the samples into RGB.
///
/// The filter state is per pass and not per picture: each pass has its own
/// first row, and carrying the previous pass's last row into it is the mistake
/// that makes an interlaced picture come out as coloured noise.
fn unfilter_pass(
    head: &Header,
    raw: &[u8],
    at: &mut usize,
    p: &mut Pass,
) -> Result<(), &'static str> {
    let stride = (p.pw * head.bits_per_pixel() + 7) / 8;
    let step = head.filter_step();
    let mut prev = alloc::vec![0u8; stride];
    let mut cur = alloc::vec![0u8; stride];

    for y in 0..p.ph {
        if *at + 1 + stride > raw.len() {
            return Err("a PNG whose pixel data stops early");
        }
        let filter = raw[*at];
        *at += 1;
        cur.copy_from_slice(&raw[*at..*at + stride]);
        *at += stride;
        unfilter_row(filter, &mut cur, &prev, step)?;

        let ty = p.y0 + y * p.dy;
        for x in 0..p.pw {
            let tx = p.x0 + x * p.dx;
            let to = (ty * head.w + tx) * 3;
            emit(head, &cur, x, &mut p.out[to..to + 3], p.palette, p.trns, p.bg);
        }
        core::mem::swap(&mut prev, &mut cur);
    }
    Ok(())
}

/// The five filters, one of which is "none".
fn unfilter_row(filter: u8, cur: &mut [u8], prev: &[u8], step: usize) -> Result<(), &'static str> {
    match filter {
        0 => {}
        1 => {
            for i in step..cur.len() {
                cur[i] = cur[i].wrapping_add(cur[i - step]);
            }
        }
        2 => {
            for i in 0..cur.len() {
                cur[i] = cur[i].wrapping_add(prev[i]);
            }
        }
        3 => {
            for i in 0..cur.len() {
                let left = if i >= step { cur[i - step] as u16 } else { 0 };
                let up = prev[i] as u16;
                cur[i] = cur[i].wrapping_add(((left + up) / 2) as u8);
            }
        }
        4 => {
            for i in 0..cur.len() {
                let left = if i >= step { cur[i - step] as i16 } else { 0 };
                let up = prev[i] as i16;
                let upleft = if i >= step { prev[i - step] as i16 } else { 0 };
                // Paeth's predictor: whichever of the three neighbours the
                // gradient says is closest to where the pixel is going.
                let p = left + up - upleft;
                let pa = (p - left).abs();
                let pb = (p - up).abs();
                let pc = (p - upleft).abs();
                let pred = if pa <= pb && pa <= pc {
                    left
                } else if pb <= pc {
                    up
                } else {
                    upleft
                };
                cur[i] = cur[i].wrapping_add(pred as u8);
            }
        }
        _ => return Err("a PNG row filter that is not one of the five"),
    }
    Ok(())
}

/// One pixel out of an unfiltered row, as RGB.
fn emit(
    head: &Header,
    row: &[u8],
    x: usize,
    out: &mut [u8],
    palette: &[(u8, u8, u8)],
    trns: &[u8],
    bg: (u8, u8, u8),
) {
    let depth = head.depth as usize;
    let ch = head.channels();
    // The sample as the file stores it: eight bits, sixteen, or a packed
    // fraction of a byte.  Transparency is declared in these terms, so it is
    // the value to compare; the value to draw is the reduced one.
    let raw = |channel: usize| -> u16 {
        match depth {
            16 => be16(&row[(x * ch + channel) * 2..]),
            8 => row[x * ch + channel] as u16,
            _ => sub_byte(row, x, depth) as u16,
        }
    };
    let show = |v: u16| -> u8 {
        match depth {
            16 => reduce16(v),
            8 => v as u8,
            _ => scale(v as u8, depth),
        }
    };

    let (rgb, alpha) = match head.color {
        0 => {
            let g = raw(0);
            let clear = trns.len() >= 2 && be16(&trns[0..2]) == g;
            let v = show(g);
            ((v, v, v), if clear { 0 } else { 255 })
        }
        2 => {
            let rgb16 = (raw(0), raw(1), raw(2));
            let clear = trns.len() >= 6
                && (be16(&trns[0..2]), be16(&trns[2..4]), be16(&trns[4..6])) == rgb16;
            (
                (show(rgb16.0), show(rgb16.1), show(rgb16.2)),
                if clear { 0 } else { 255 },
            )
        }
        3 => {
            let i = raw(0) as usize;
            let rgb = palette.get(i).copied().unwrap_or((0, 0, 0));
            (rgb, trns.get(i).copied().unwrap_or(255))
        }
        4 => {
            let v = show(raw(0));
            ((v, v, v), show(raw(1)))
        }
        _ => (
            (show(raw(0)), show(raw(1)), show(raw(2))),
            show(raw(3)),
        ),
    };
    let c = over(rgb, alpha, bg);
    out[0] = c.0;
    out[1] = c.1;
    out[2] = c.2;
}

/// Sample `i` of a row whose samples are narrower than a byte.
fn sub_byte(row: &[u8], i: usize, depth: usize) -> u8 {
    let per = 8 / depth;
    let byte = match row.get(i / per) {
        Some(&b) => b,
        None => return 0,
    };
    let shift = 8 - depth * (i % per + 1);
    (byte >> shift) & ((1u16 << depth) - 1) as u8
}

/// Widen a sample to the full range, so a one-bit white is 255 and not 1.
fn scale(v: u8, depth: usize) -> u8 {
    let max = (1u16 << depth) - 1;
    ((v as u16 * 255) / max) as u8
}

/// Narrow a sixteen-bit sample to eight.  Dividing by 257 is the same as
/// scaling by 255 over 65535, and does it without an intermediate that could
/// overflow; the half is there so the result rounds rather than truncates.
/// Taking the high byte instead — which is the usual shortcut — makes every
/// value slightly too dark.
fn reduce16(v: u16) -> u8 {
    ((v as u32 + 128) / 257) as u8
}

fn be16(b: &[u8]) -> u16 {
    ((b[0] as u16) << 8) | b[1] as u16
}

fn be32(b: &[u8]) -> usize {
    ((b[0] as usize) << 24) | ((b[1] as usize) << 16) | ((b[2] as usize) << 8) | b[3] as usize
}
