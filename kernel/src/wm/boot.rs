//! Boot splash: what the machine shows while it is coming up.
//!
//! Drawn as each subsystem initialises, so the screen reports progress instead
//! of sitting black until the desktop appears.  Everything goes through the
//! backbuffer and is flipped as a whole, so a stage never appears half drawn.

use crate::dev::framebuffer;
use crate::wm::font;

const BG: (u8, u8, u8) = (0x08, 0x0A, 0x14);
const TITLE: (u8, u8, u8) = (0x10, 0xB9, 0x81);
const SUB: (u8, u8, u8) = (0x50, 0x70, 0x90);
const BAR_BG: (u8, u8, u8) = (0x1A, 0x22, 0x33);
const BAR_FG: (u8, u8, u8) = (0x10, 0xB9, 0x81);
const LABEL: (u8, u8, u8) = (0xAA, 0xCC, 0xFF);
const DONE: (u8, u8, u8) = (0xE0, 0xB0, 0x40);

const BAR_W: u32 = 420;
const BAR_H: u32 = 14;

/// How many stages `main` reports; used for the bar and the counter.
pub const TOTAL: u32 = 12;

fn bar_origin() -> (u32, u32) {
    let (w, h) = framebuffer::size();
    (w.saturating_sub(BAR_W) / 2, h / 2 + 30)
}

/// Paint the whole splash for `step`/`TOTAL`.
///
/// Repaints everything each time rather than only the bar: whatever was on
/// screen before — the framebuffer test pattern, a previous stage's label —
/// would otherwise show through around the parts we redraw.
fn paint(step: u32, label: &str, ready: bool) {
    let (w, h) = framebuffer::size();
    if w == 0 || h == 0 {
        return;
    }
    let bg = if ready { (0x0A, 0x12, 0x10) } else { BG };
    framebuffer::fill_rect(0, 0, w, h, bg.0, bg.1, bg.2);

    let title = "barryOS";
    let scale = 3;
    let tw = font::text_width(title, scale);
    font::draw_str_scaled(
        title,
        w.saturating_sub(tw) / 2,
        h / 2 - 96,
        scale,
        TITLE.0, TITLE.1, TITLE.2,
    );

    let sub = "a self-developed x86_64 operating system";
    let sw = font::text_width(sub, 1);
    font::draw_str(sub, w.saturating_sub(sw) / 2, h / 2 - 26, SUB.0, SUB.1, SUB.2);

    let (bx, by) = bar_origin();
    framebuffer::fill_rect(bx, by, BAR_W, BAR_H, BAR_BG.0, BAR_BG.1, BAR_BG.2);

    let step = step.min(TOTAL);
    let filled = BAR_W * step / TOTAL;
    if filled > 2 {
        framebuffer::fill_rect(
            bx + 1, by + 1,
            filled - 2, BAR_H - 2,
            BAR_FG.0, BAR_FG.1, BAR_FG.2,
        );
    }

    // Counter to the right of the bar, so the bar has a number attached.
    let mut buf = [0u8; 8];
    let mut k = 0usize;
    k += put_dec(step, &mut buf[k..]);
    buf[k] = b'/';
    k += 1;
    k += put_dec(TOTAL, &mut buf[k..]);
    if let Ok(s) = core::str::from_utf8(&buf[..k]) {
        font::draw_str(s, bx + BAR_W + 10, by, SUB.0, SUB.1, SUB.2);
    }

    // Current stage, centred below the bar.
    let ly = by + BAR_H + 16;
    let lw = font::text_width(label, 1);
    font::draw_str(label, w.saturating_sub(lw) / 2, ly, LABEL.0, LABEL.1, LABEL.2);

    if ready {
        let msg = "Ready";
        let mw = font::text_width(msg, 1);
        font::draw_str(msg, w.saturating_sub(mw) / 2, ly + 28, DONE.0, DONE.1, DONE.2);
    }

    framebuffer::flip();
}

/// Put the splash on screen before the first stage runs.
pub fn begin() {
    paint(0, "Starting up...", false);
}

/// Report progress: fill the bar to `step`/`TOTAL` and name the stage.
pub fn stage(step: u32, label: &str) {
    paint(step, label, false);
}

/// Fill the bar and say what happens next.
pub fn finish() {
    paint(TOTAL, "Ready", true);
}

fn put_dec(mut v: u32, out: &mut [u8]) -> usize {
    let mut tmp = [0u8; 10];
    let mut i = tmp.len();
    if v == 0 {
        i -= 1;
        tmp[i] = b'0';
    }
    while v > 0 {
        i -= 1;
        tmp[i] = b'0' + (v % 10) as u8;
        v /= 10;
    }
    let n = (tmp.len() - i).min(out.len());
    out[..n].copy_from_slice(&tmp[i..i + n]);
    n
}
