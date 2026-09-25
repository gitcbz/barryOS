//! Decoding the pictures a page refers to.
//!
//! One entry point, `decode`, which looks at the first few bytes and hands the
//! work to whichever format that is.  Everything below returns the same thing:
//! a flat buffer of eight-bit red, green and blue, top row first — the only
//! shape the framebuffer can draw without a conversion step, and the only one
//! worth having when the alternative is four decoders each with their own
//! idea of what a pixel is.
//!
//! Transparency is resolved here rather than carried.  A transparent pixel has
//! to become *some* colour before it is drawn, and the only thing that knows
//! what is behind it is the caller — so the caller passes the colour of the
//! paper and the decoder composites onto it.  Carrying an alpha channel
//! instead would cost a quarter more memory to defer a decision that has
//! exactly one answer.
//!
//! Formats: PNG, GIF, JPEG (baseline and progressive), BMP and ICO.  WebP and
//! AVIF are not here and are not going to be: both are built on video codecs,
//! which is a different project from this one.  A page that uses them shows
//! the `alt` text, which is what the alt text is for.

pub mod bmp;
pub mod gif;
pub mod inflate;
pub mod jpeg;
pub mod png;

use alloc::vec::Vec;

/// A decoded picture, eight bits per channel, three channels, no padding.
#[derive(Clone)]
pub struct Image {
    pub w: usize,
    pub h: usize,
    pub px: Vec<u8>,
}

impl Image {
    pub fn pixel(&self, x: usize, y: usize) -> (u8, u8, u8) {
        let i = (y * self.w + x) * 3;
        if i + 2 >= self.px.len() {
            return (0, 0, 0);
        }
        (self.px[i], self.px[i + 1], self.px[i + 2])
    }

    pub fn bytes(&self) -> usize {
        self.px.len()
    }

    /// The average of the picture, for a self-test that has to say something
    /// about a decode without comparing every pixel.
    pub fn mean(&self) -> (u8, u8, u8) {
        if self.px.is_empty() {
            return (0, 0, 0);
        }
        let n = self.px.len() / 3;
        let (mut r, mut g, mut b) = (0u64, 0u64, 0u64);
        for i in 0..n {
            r += self.px[i * 3] as u64;
            g += self.px[i * 3 + 1] as u64;
            b += self.px[i * 3 + 2] as u64;
        }
        ((r / n as u64) as u8, (g / n as u64) as u8, (b / n as u64) as u8)
    }
}

/// Decode whatever this is, as long as the result fits in `budget` bytes of
/// RGB.  `bg` is what shows through anything transparent.
///
/// The budget is checked against the *decoded* size and not the file size, and
/// each decoder checks it before allocating — a 40 KiB PNG can describe a
/// 20000 by 20000 picture, and finding that out after the allocation is not
/// finding out in time.
pub fn decode(bytes: &[u8], budget: usize, bg: (u8, u8, u8)) -> Result<Image, &'static str> {
    if bytes.len() < 16 {
        return Err("too short to be a picture");
    }
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        return png::decode(bytes, budget, bg);
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return jpeg::decode(bytes, budget, bg);
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return gif::decode(bytes, budget, bg);
    }
    if bytes.starts_with(b"BM") || bytes.starts_with(&[0x00, 0x00, 0x01, 0x00]) {
        return bmp::decode(bytes, budget, bg);
    }
    Err("not a picture format this browser reads")
}

/// The size a picture is allowed to decode to, and the total across a page.
///
/// Both are guesswork made explicit.  A picture wider than the window is
/// scaled down to fit when it is drawn, so the pixels past that point are work
/// done for nothing — but finding the scaled size means decoding the header,
/// and by then the decoder has to either commit or not.  Refusing is the
/// honest answer: the `alt` text still appears, and the log says why.
pub const IMAGE_BUDGET: usize = 1024 * 1024;

/// Keep an index inside the picture, or fail rather than panic.
pub(crate) fn checked(at: usize, len: usize) -> Result<usize, &'static str> {
    if at >= len {
        Err("a picture that ends before its own header says it does")
    } else {
        Ok(at)
    }
}

/// A row of `w` pixels, as three bytes each, zeroed and ready to write.
pub(crate) fn plane(w: usize, h: usize) -> Result<Vec<u8>, &'static str> {
    let n = w.checked_mul(h).and_then(|v| v.checked_mul(3));
    match n {
        Some(n) if n <= 64 << 20 => Ok(alloc::vec![0u8; n]),
        _ => Err("a picture whose own dimensions overflow"),
    }
}

/// Blend `fg` over `bg` at `alpha` of 255.
pub(crate) fn over(fg: (u8, u8, u8), alpha: u8, bg: (u8, u8, u8)) -> (u8, u8, u8) {
    if alpha == 255 {
        return fg;
    }
    if alpha == 0 {
        return bg;
    }
    let a = alpha as u32;
    let mix = |f: u8, b: u8| ((f as u32 * a + b as u32 * (255 - a)) / 255) as u8;
    (mix(fg.0, bg.0), mix(fg.1, bg.1), mix(fg.2, bg.2))
}
