//! BMP, and the ICO container that wraps one.
//!
//! Neither is a web format.  Both are here because of a browser's oldest
//! ornament: the address bar's favicon is an `.ico`, and an ICO is either a
//! PNG — already covered — or a BMP with its file header filed off.  A BMP is
//! about sixty lines, most of which is a palette, so the favicon is nearly
//! free once the pixels are understood.
//!
//! Sixteen-bit BMPs are read as the 5-5-5 layout that BI_RGB implies, and
//! sixteen-bit ones with explicit bit masks are read as though the masks were
//! the usual ones.  Both are guesses, both are rare, and both are wrong in a
//! way that is visible rather than silent.

use super::{over, plane, Image};
use alloc::vec::Vec;

/// Decode a BMP file or an ICO.
pub fn decode(src: &[u8], budget: usize, bg: (u8, u8, u8)) -> Result<Image, &'static str> {
    if src.starts_with(b"BM") {
        return dib(src, 14, budget, bg, false);
    }
    ico(src, budget, bg)
}

/// The largest image in an icon that still fits, which is the one a browser
/// would draw at a size worth looking at.
fn ico(src: &[u8], budget: usize, bg: (u8, u8, u8)) -> Result<Image, &'static str> {
    if src.len() < 6 || le16(src, 0) != 0 || le16(src, 2) != 1 {
        return Err("not an icon file");
    }
    let count = le16(src, 4);
    if count == 0 {
        return Err("an icon with no images in it");
    }
    let mut best: Option<(usize, usize, usize)> = None;   // area, offset, length
    for i in 0..count {
        let e = 6 + i * 16;
        if e + 16 > src.len() {
            break;
        }
        let len = le32(src, e + 8);
        let at = le32(src, e + 12);
        if at + len > src.len() {
            continue;
        }
        // A zero in either dimension means 256, which is the one value the
        // single byte could not otherwise hold.
        let w = if src[e] == 0 { 256 } else { src[e] as usize };
        let h = if src[e + 1] == 0 { 256 } else { src[e + 1] as usize };
        if best.map(|(a, _, _)| w * h > a).unwrap_or(true) {
            best = Some((w * h, at, len));
        }
    }
    let (_, at, len) = best.ok_or("an icon whose entries all point outside the file")?;
    let body = &src[at..at + len];
    if body.starts_with(&[0x89, b'P', b'N', b'G']) {
        return super::png::decode(body, budget, bg);
    }
    // A DIB, with the height doubled: the bottom half is a one-bit mask that
    // predates alpha channels and is now ignored by everything.
    dib(body, 0, budget, bg, true)
}

/// One DIB — the part of a BMP after the file header, which is also the whole
/// of what an ICO holds.
fn dib(
    src: &[u8],
    at: usize,
    budget: usize,
    bg: (u8, u8, u8),
    half_height: bool,
) -> Result<Image, &'static str> {
    if at + 4 > src.len() {
        return Err("a bitmap header that runs past the end");
    }
    let header = le32(src, at);
    let (w, mut h, bpp, bottom_up, mut palette_at, entry);
    let mut used = 0usize;
    if header == 12 {
        // The OS/2 header, the only one with sixteen-bit dimensions and the
        // only one with no way to say "top down".
        if at + 12 > src.len() {
            return Err("a bitmap header that runs past the end");
        }
        w = le16(src, at + 4);
        h = le16(src, at + 6);
        bpp = le16(src, at + 10);
        bottom_up = true;
        palette_at = at + 12;
        entry = 3;
    } else if header >= 40 {
        if at + 40 > src.len() {
            return Err("a bitmap header that runs past the end");
        }
        let ww = le32(src, at + 4) as i32;
        let hh = le32(src, at + 8) as i32;
        // A negative height means the rows are stored top down, which is a
        // late addition and the opposite of the default.
        if ww <= 0 || hh == 0 {
            return Err("a bitmap with no pixels in it");
        }
        w = ww as usize;
        h = hh.unsigned_abs() as usize;
        bpp = le16(src, at + 14);
        bottom_up = hh > 0;
        used = le32(src, at + 32);
        let compression = le32(src, at + 16);
        if compression != 0 && compression != 3 {
            return Err("a compressed bitmap, which is never used on the web");
        }
        palette_at = at + header;
        entry = 4;
        if compression == 3 && header == 40 {
            // Room for the three colour masks, which this reader does not use
            // but which the palette still sits behind.
            palette_at += 12;
        }
    } else {
        return Err("a bitmap header of a size this reader does not know");
    }

    if half_height {
        h /= 2;
    }
    if w == 0 || h == 0 {
        return Err("a bitmap with no pixels in it");
    }
    let want = w
        .checked_mul(h)
        .and_then(|n| n.checked_mul(3))
        .ok_or("a bitmap whose dimensions overflow")?;
    if want > budget {
        return Err("a bitmap that decodes to more bytes than there is room for");
    }

    // The palette, for the depths that have one.  `colors_used` is allowed to
    // be smaller than the depth implies, and encoders do write it that way.
    let mut palette: Vec<(u8, u8, u8)> = Vec::new();
    if bpp <= 8 {
        let n = if used != 0 && used < (1 << bpp) { used } else { 1 << bpp };
        if palette_at + n * entry > src.len() {
            return Err("a bitmap palette that runs past the end");
        }
        for i in 0..n {
            let b = src[palette_at + i * entry];
            let g = src[palette_at + i * entry + 1];
            let r = src[palette_at + i * entry + 2];
            palette.push((r, g, b));
        }
    }

    let pixels = palette_at + if bpp <= 8 { palette.len() * entry } else { 0 };
    // Rows are padded out to four bytes, which is where the format's age shows.
    let stride = ((bpp as usize * w + 31) / 32) * 4;
    if pixels + stride * h > src.len() {
        return Err("a bitmap whose pixel data runs past the end");
    }

    let mut out = plane(w, h)?;
    for y in 0..h {
        let row = if bottom_up { h - 1 - y } else { y };
        let base = pixels + row * stride;
        for x in 0..w {
            let c = match bpp {
                1 | 4 | 8 => {
                    let per = 8 / bpp as usize;
                    let byte = src[base + x / per];
                    let shift = 8 - bpp as usize * (x % per + 1);
                    let i = (byte >> shift) & ((1u16 << bpp) - 1) as u8;
                    palette.get(i as usize).copied().unwrap_or((0, 0, 0))
                }
                16 => {
                    let v = le16(src, base + x * 2);
                    let r = ((v >> 10) & 0x1F) as u8;
                    let g = ((v >> 5) & 0x1F) as u8;
                    let b = (v & 0x1F) as u8;
                    ((r << 3) | (r >> 2), (g << 3) | (g >> 2), (b << 3) | (b >> 2))
                }
                24 => {
                    let p = base + x * 3;
                    (src[p + 2], src[p + 1], src[p])
                }
                32 => {
                    let p = base + x * 4;
                    let a = src[p + 3];
                    // A zero alpha in a 32-bit BMP almost always means "no
                    // alpha channel was written", not "invisible".
                    let a = if a == 0 { 255 } else { a };
                    over((src[p + 2], src[p + 1], src[p]), a, bg)
                }
                _ => return Err("a bitmap at a colour depth this reader does not know"),
            };
            let o = (y * w + x) * 3;
            out[o] = c.0;
            out[o + 1] = c.1;
            out[o + 2] = c.2;
        }
    }
    Ok(Image { w, h, px: out })
}

fn le16(b: &[u8], at: usize) -> usize {
    (b[at] as usize) | ((b[at + 1] as usize) << 8)
}

fn le32(b: &[u8], at: usize) -> usize {
    (b[at] as usize)
        | ((b[at + 1] as usize) << 8)
        | ((b[at + 2] as usize) << 16)
        | ((b[at + 3] as usize) << 24)
}
