//! GIF87a and GIF89a: LZW over a palette.
//!
//! The oldest format here, and still the one that turns up for small animated
//! things.  Only the first frame is decoded — this browser shows a still
//! picture, and animating one would mean a timer, a redraw, and a decision
//! about what to do when a page has forty of them.
//!
//! The canvas is the logical screen and the frame is drawn onto it at its own
//! offset, which is what the format actually says.  For the usual case — one
//! frame the size of the screen — that is the same as decoding the frame.

use super::{plane, Image};
use alloc::vec::Vec;

/// The most entries a GIF dictionary can have: twelve-bit codes.
const MAX_CODES: usize = 4096;

/// Decode the first frame of a GIF into RGB.
pub fn decode(src: &[u8], budget: usize, bg: (u8, u8, u8)) -> Result<Image, &'static str> {
    if src.len() < 13 {
        return Err("a GIF that ends inside its own header");
    }
    let cw = le16(&src[6..8]);
    let ch = le16(&src[8..10]);
    let packed = src[10];
    let bg_index = src[11];
    if cw == 0 || ch == 0 {
        return Err("a GIF with no pixels in it");
    }
    let want = cw
        .checked_mul(ch)
        .and_then(|n| n.checked_mul(3))
        .ok_or("a GIF whose dimensions overflow")?;
    if want > budget {
        return Err("a GIF that decodes to more bytes than there is room for");
    }

    let mut at = 13usize;
    let global = if packed & 0x80 != 0 {
        Some(read_table(src, &mut at, packed)?)
    } else {
        None
    };

    // What the parts of the canvas the frame does not cover are made of.
    // Without this they would be whatever the allocator left behind.
    let mut clear = bg;
    if let Some(t) = &global {
        if let Some(&c) = t.get(bg_index as usize) {
            clear = c;
        }
    }

    let mut transparent: Option<u8> = None;
    loop {
        let Some(&tag) = src.get(at) else {
            return Err("a GIF that ends before its last block");
        };
        at += 1;
        match tag {
            0x3B => return Err("a GIF with no picture in it"),
            // An extension.  The graphic control one is where transparency is
            // declared, and it applies to the frame that follows it.
            0x21 => {
                let Some(&label) = src.get(at) else {
                    return Err("a GIF extension with no label");
                };
                at += 1;
                let body = sub_blocks(src, &mut at)?;
                if label == 0xF9 && body.len() >= 4 && body[0] & 0x01 != 0 {
                    transparent = Some(body[3]);
                }
            }
            0x2C => {
                let (f, fw, left, top) = frame(src, &mut at, global.as_deref(), transparent, bg)?;
                return Ok(Image { w: cw, h: ch, px: compose(cw, ch, &f, fw, left, top, clear) });
            }
            _ => return Err("a GIF block this decoder does not know"),
        }
    }
}

/// The whole sub-block chain after a block's introducer, joined.  A GIF has no
/// length field anywhere: every block is a chain of counted chunks ending in a
/// zero.
fn sub_blocks(src: &[u8], at: &mut usize) -> Result<Vec<u8>, &'static str> {
    let mut out = Vec::new();
    loop {
        let Some(&len) = src.get(*at) else {
            return Err("a GIF that ends inside a block");
        };
        *at += 1;
        if len == 0 {
            return Ok(out);
        }
        let len = len as usize;
        if *at + len > src.len() {
            return Err("a GIF sub-block that runs past the end");
        }
        out.extend_from_slice(&src[*at..*at + len]);
        *at += len;
    }
}

fn read_table(src: &[u8], at: &mut usize, packed: u8) -> Result<Vec<(u8, u8, u8)>, &'static str> {
    let n = 1usize << ((packed & 0x07) + 1);
    if *at + n * 3 > src.len() {
        return Err("a GIF colour table that runs past the end");
    }
    let mut t = Vec::with_capacity(n);
    for c in src[*at..*at + n * 3].chunks_exact(3) {
        t.push((c[0], c[1], c[2]));
    }
    *at += n * 3;
    Ok(t)
}

/// One frame, as its own patch of pixels, plus its width and where on the
/// canvas it goes.
fn frame(
    src: &[u8],
    at: &mut usize,
    global: Option<&[(u8, u8, u8)]>,
    transparent: Option<u8>,
    bg: (u8, u8, u8),
) -> Result<(Vec<u8>, usize, usize, usize), &'static str> {
    if *at + 9 > src.len() {
        return Err("a GIF image descriptor that runs past the end");
    }
    let left = le16(&src[*at..*at + 2]);
    let top = le16(&src[*at + 2..*at + 4]);
    let w = le16(&src[*at + 4..*at + 6]);
    let h = le16(&src[*at + 6..*at + 8]);
    let packed = src[*at + 8];
    *at += 9;

    let local = if packed & 0x80 != 0 {
        Some(read_table(src, at, packed)?)
    } else {
        None
    };
    let table = local
        .as_deref()
        .or(global)
        .ok_or("a GIF frame with no colour table to draw it from")?;

    let Some(&min_code_size) = src.get(*at) else {
        return Err("a GIF frame with no LZW code size");
    };
    *at += 1;
    if !(2..=8).contains(&min_code_size) {
        return Err("a GIF whose LZW code size is out of range");
    }
    if w == 0 || h == 0 {
        return Err("a GIF frame with no pixels in it");
    }
    let data = sub_blocks(src, at)?;
    let indices = lzw(&data, min_code_size, w * h)?;

    // Start from the paper the caller is drawing on, so a transparent pixel
    // is simply one that was never written and the frame needs no separate
    // mask.  The logical screen's own background colour is not used here: it
    // describes what shows where the frame does not reach, and that is the
    // canvas's business, not the frame's.
    let mut out = plane(w, h)?;
    for p in out.chunks_exact_mut(3) {
        p[0] = bg.0;
        p[1] = bg.1;
        p[2] = bg.2;
    }

    // Interlaced frames arrive in four passes over the rows, which is what
    // made a GIF appear top-to-bottom in strips on a slow connection.
    let (rows, steps): ([usize; 4], [usize; 4]) = ([0, 4, 2, 1], [8, 8, 4, 2]);
    let mut order: Vec<usize> = Vec::with_capacity(h);
    if packed & 0x40 != 0 {
        for p in 0..4 {
            let mut y = rows[p];
            while y < h {
                order.push(y);
                y += steps[p];
            }
        }
    } else {
        order.extend(0..h);
    }

    for (i, &y) in order.iter().enumerate() {
        for x in 0..w {
            let idx = indices[i * w + x];
            if Some(idx) == transparent {
                continue;
            }
            let c = table.get(idx as usize).copied().unwrap_or((0, 0, 0));
            let o = (y * w + x) * 3;
            out[o] = c.0;
            out[o + 1] = c.1;
            out[o + 2] = c.2;
        }
    }
    Ok((out, w, left, top))
}

/// Put a frame onto a canvas of `clear`, clipped to it.  A frame is allowed to
/// hang off the edge, and one that did would otherwise write past the end.
fn compose(cw: usize, ch: usize, f: &[u8], fw: usize, left: usize, top: usize,
           clear: (u8, u8, u8)) -> Vec<u8> {
    let mut out = alloc::vec![0u8; cw * ch * 3];
    for p in out.chunks_exact_mut(3) {
        p[0] = clear.0;
        p[1] = clear.1;
        p[2] = clear.2;
    }
    let fh = if fw == 0 { 0 } else { f.len() / 3 / fw };
    for y in 0..fh {
        let ty = top + y;
        if ty >= ch {
            break;
        }
        let xmax = fw.min(cw.saturating_sub(left));
        let from = y * fw * 3;
        let to = (ty * cw + left) * 3;
        out[to..to + xmax * 3].copy_from_slice(&f[from..from + xmax * 3]);
    }
    out
}

/// The LZW variant GIF uses: codes packed least significant bit first, a table
/// that grows out of the codes themselves, and a clear code that resets it.
///
/// The one case that is not obvious is a code equal to the next free table
/// entry.  The encoder may send it before the decoder has built it, and it
/// means "the previous string, plus that string's own first character" — the
/// only code whose meaning depends on what came before rather than on the
/// table.
fn lzw(data: &[u8], min_code_size: u8, expect: usize) -> Result<Vec<u8>, &'static str> {
    let clear = 1u16 << min_code_size;
    let end = clear + 1;

    let mut prefix = [0u16; MAX_CODES];
    let mut suffix = [0u8; MAX_CODES];
    for i in 0..clear as usize {
        suffix[i] = i as u8;
    }
    let mut next = end as usize + 1;
    let mut size = min_code_size as u32 + 1;
    let mut prev: i32 = -1;
    let mut first = 0u8;

    let mut out = Vec::with_capacity(expect.min(1 << 20));
    let mut stack = [0u8; MAX_CODES];
    let mut bits = LsbBits::new(data);

    while out.len() < expect {
        let Some(code) = bits.take(size) else {
            return Err("a GIF whose LZW data stops early");
        };
        if code == clear {
            next = end as usize + 1;
            size = min_code_size as u32 + 1;
            prev = -1;
            continue;
        }
        if code == end {
            break;
        }
        if code as usize > next || (code as usize == next && prev < 0) {
            return Err("a GIF referring to a table entry it has not built");
        }

        let mut sp = 0usize;
        let mut cur = code as i32;
        if code as usize == next {
            stack[sp] = first;
            sp += 1;
            cur = prev;
        }
        while cur >= clear as i32 {
            if sp >= MAX_CODES || cur as usize >= MAX_CODES {
                return Err("a GIF dictionary that has run away");
            }
            stack[sp] = suffix[cur as usize];
            sp += 1;
            cur = prefix[cur as usize] as i32;
        }
        first = suffix[cur as usize];
        stack[sp] = first;
        sp += 1;

        // The stack was filled back to front, so it comes out in reverse.
        for k in (0..sp).rev() {
            if out.len() >= expect {
                break;
            }
            out.push(stack[k]);
        }

        if prev >= 0 && next < MAX_CODES {
            prefix[next] = prev as u16;
            suffix[next] = first;
            next += 1;
            // The code width grows as soon as the table needs one more bit,
            // and the encoder grows it at exactly the same moment.
            if next == (1usize << size) && size < 12 {
                size += 1;
            }
        }
        prev = code as i32;
    }

    if out.len() < expect {
        return Err("a GIF frame with fewer pixels than its descriptor says");
    }
    out.truncate(expect);
    Ok(out)
}

/// Codes read least significant bit first, which is the opposite of JPEG's
/// entropy data and a standing trap for anyone who writes both.
struct LsbBits<'a> {
    src: &'a [u8],
    at: usize,
    buf: u32,
    n: u32,
}

impl<'a> LsbBits<'a> {
    fn new(src: &'a [u8]) -> LsbBits<'a> {
        LsbBits { src, at: 0, buf: 0, n: 0 }
    }

    fn take(&mut self, want: u32) -> Option<u16> {
        while self.n < want {
            let b = *self.src.get(self.at)?;
            self.at += 1;
            self.buf |= (b as u32) << self.n;
            self.n += 8;
        }
        let v = self.buf & ((1u32 << want) - 1);
        self.buf >>= want;
        self.n -= want;
        Some(v as u16)
    }
}

fn le16(b: &[u8]) -> usize {
    (b[0] as usize) | ((b[1] as usize) << 8)
}
