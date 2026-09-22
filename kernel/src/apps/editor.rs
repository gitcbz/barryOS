//! Text editor — opens a file from the filesystem, edits it, writes it back.
//!
//! The buffer is one flat byte array with a byte-index cursor.  Lines are
//! derived on the fly by scanning for newlines, which is fine at this size and
//! avoids a second index that could drift out of sync with the text.
//!
//! Keyboard: characters insert, Enter breaks the line, Backspace and Delete
//! remove, arrows move.  Save is a toolbar button — there is no Ctrl handling
//! in the scancode map yet.

use crate::dev::framebuffer;
use crate::fs::{ramfs, vfs};
use crate::serial;
use crate::wm::font;
use crate::wm::window;
use core::sync::atomic::{AtomicU64, Ordering};

const ED_POS_X: u32 = 70;
const ED_POS_Y: u32 = 60;
const ED_W: u32 = 480;
const ED_H: u32 = 340;

const TOOL_Y: u32 = 24;
const TOOL_H: u32 = 24;
const TEXT_Y: u32 = 52;
const LINE_H: u32 = 16;
const PAD_X: u32 = 8;

/// Editor buffer, one byte per character including newlines.
const CAP: usize = vfs::MAX_FILE_SIZE;
/// Longest single rendered line (a window is ~58 columns wide).
const LINE_MAX: usize = 200;
static mut TEXT: [u8; CAP] = [0; CAP];
static mut LEN: usize = 0;
static mut CURSOR: usize = 0;
/// First line shown, so the cursor can be scrolled into view.
static mut TOP_LINE: usize = 0;
/// Byte index where the editor started, to keep the view steady while a
/// multi-byte character is being typed.  (Unused for now; kept for clarity.)
static mut DIRTY: bool = false;

static WIN_ID: AtomicU64 = AtomicU64::new(0);
static FILE_ID: AtomicU64 = AtomicU64::new(0);

/// Name of the open file, for the title bar.
static mut FILE_NAME: [u8; vfs::MAX_NAME + 1] = [0; vfs::MAX_NAME + 1];
static mut FILE_NAME_LEN: usize = 0;
static mut STATUS: [u8; 40] = [0; 40];
static mut STATUS_LEN: usize = 0;

pub fn window_id() -> u64 {
    WIN_ID.load(Ordering::Relaxed)
}

/// Open the editor, creating its window on first use.
pub fn open() {
    if window::exists(window_id()) {
        window::focus(window_id());
        return;
    }
    let id = window::create(ED_POS_X, ED_POS_Y, ED_W, ED_H, "Editor");
    WIN_ID.store(id, Ordering::SeqCst);
    serial::print_str("[apps] editor: window created id=");
    serial::print_hex(id);
    serial::print_str("\n");
}

/// Load a file into the editor and raise it.
pub fn open_file(vnode_id: u64) {
    open();
    if !vfs::exists(vnode_id) || vfs::get_type(vnode_id) == Some(vfs::VnodeType::Dir) {
        set_status("not a file");
        return;
    }
    FILE_ID.store(vnode_id, Ordering::SeqCst);

    let mut name = [0u8; vfs::MAX_NAME + 1];
    let n = vfs::get_name(vnode_id, &mut name);
    unsafe {
        FILE_NAME[..n].copy_from_slice(&name[..n]);
        FILE_NAME_LEN = n;
        // Buffer contents before the length are the whole file.
        let got = ramfs::read_file(vnode_id, &mut TEXT[..]);
        LEN = got;
        CURSOR = 0;
        TOP_LINE = 0;
        DIRTY = false;
    }

    // Show the name in the title bar.
    if let Ok(s) = core::str::from_utf8(&name[..n]) {
        let mut title = [0u8; 24];
        let mut k = 0usize;
        for &b in b"Edit: " {
            title[k] = b;
            k += 1;
        }
        for &b in s.as_bytes() {
            if k >= 23 {
                break;
            }
            title[k] = b;
            k += 1;
        }
        if let Ok(t) = core::str::from_utf8(&title[..k]) {
            window::set_title(window_id(), t);
        }
    }

    set_status("loaded");
    serial::print_str("[editor] opened id=");
    serial::print_hex(vnode_id);
    serial::print_str(" (");
    serial::print_hex(unsafe { LEN } as u64);
    serial::print_str(" bytes)\n");
}

fn set_status(msg: &str) {
    unsafe {
        let n = msg.len().min(STATUS.len());
        STATUS[..n].copy_from_slice(&msg.as_bytes()[..n]);
        STATUS_LEN = n;
    }
}

// ---------------------------------------------------------------------------
//  Text model
// ---------------------------------------------------------------------------

/// Index of the first character of the line containing `pos`.
fn line_start(pos: usize) -> usize {
    let len = unsafe { LEN };
    let p = unsafe { core::ptr::addr_of!(TEXT) as *const u8 };
    let mut i = pos.min(len);
    while i > 0 {
        if unsafe { core::ptr::read_volatile(p.add(i - 1)) } == b'\n' {
            break;
        }
        i -= 1;
    }
    i
}

/// Index of the newline ending the line containing `pos` (or `LEN`).
fn line_end(pos: usize) -> usize {
    let len = unsafe { LEN };
    let p = unsafe { core::ptr::addr_of!(TEXT) as *const u8 };
    let mut i = pos.min(len);
    while i < len {
        if unsafe { core::ptr::read_volatile(p.add(i)) } == b'\n' {
            break;
        }
        i += 1;
    }
    i
}

/// Which line the cursor is on.
fn cursor_line() -> usize {
    let cur = unsafe { CURSOR };
    let p = unsafe { core::ptr::addr_of!(TEXT) as *const u8 };
    let mut line = 0usize;
    for i in 0..cur.min(unsafe { LEN }) {
        if unsafe { core::ptr::read_volatile(p.add(i)) } == b'\n' {
            line += 1;
        }
    }
    line
}

fn insert(ch: u8) {
    unsafe {
        if LEN >= CAP {
            set_status("buffer full");
            return;
        }
        let p = core::ptr::addr_of_mut!(TEXT) as *mut u8;
        let cur = CURSOR;
        // Shift the tail right, from the back so nothing is overwritten.
        let mut i = LEN;
        while i > cur {
            let v = core::ptr::read_volatile(p.add(i - 1));
            core::ptr::write_volatile(p.add(i), v);
            i -= 1;
        }
        core::ptr::write_volatile(p.add(cur), ch);
        LEN += 1;
        CURSOR += 1;
        DIRTY = true;
    }
}

fn delete_back() {
    unsafe {
        if CURSOR == 0 {
            return;
        }
        let p = core::ptr::addr_of_mut!(TEXT) as *mut u8;
        let cur = CURSOR - 1;
        for i in cur..LEN - 1 {
            let v = core::ptr::read_volatile(p.add(i + 1));
            core::ptr::write_volatile(p.add(i), v);
        }
        LEN -= 1;
        CURSOR = cur;
        DIRTY = true;
    }
}

fn delete_forward() {
    unsafe {
        if CURSOR >= LEN {
            return;
        }
        let p = core::ptr::addr_of_mut!(TEXT) as *mut u8;
        for i in CURSOR..LEN - 1 {
            let v = core::ptr::read_volatile(p.add(i + 1));
            core::ptr::write_volatile(p.add(i), v);
        }
        LEN -= 1;
        DIRTY = true;
    }
}

fn move_up() {
    unsafe {
        let cur = CURSOR;
        let col = cur - line_start(cur);
        let ls = line_start(cur);
        if ls == 0 {
            CURSOR = 0;
            return;
        }
        let prev_end = ls - 1;              // the newline itself
        let prev_start = line_start(prev_end);
        CURSOR = (prev_start + col).min(prev_end);
    }
}

fn move_down() {
    unsafe {
        let cur = CURSOR;
        let col = cur - line_start(cur);
        let le = line_end(cur);
        if le >= LEN {
            CURSOR = LEN;
            return;
        }
        let next_start = le + 1;
        let next_end = line_end(next_start);
        CURSOR = (next_start + col).min(next_end);
    }
}

fn save() {
    let id = FILE_ID.load(Ordering::Relaxed);
    if id == 0 || !vfs::exists(id) {
        set_status("no file open");
        return;
    }
    // Writing a file needs write permission on the file itself.
    if !crate::fs::perm::allowed(id, crate::fs::perm::W) {
        set_status("permission denied: file is read-only");
        serial::print_str("[editor] save refused: no write permission\n");
        return;
    }
    let n = unsafe { ramfs::write_file(id, &TEXT[..LEN]) };
    unsafe { DIRTY = false };
    set_status("saved");
    serial::print_str("[editor] saved ");
    serial::print_hex(n as u64);
    serial::print_str(" bytes\n");
}

// ---------------------------------------------------------------------------
//  Input
// ---------------------------------------------------------------------------

fn geometry() -> (u32, u32, u32, u32) {
    window::geometry(window_id()).unwrap_or((ED_POS_X, ED_POS_Y, ED_W, ED_H))
}

/// X offset and width of toolbar button `i`.
fn button_rect_in(w: u32, i: usize) -> (u32, u32) {
    let margin = 6u32;
    let bw = 90u32;
    let _ = w;
    (margin + (i as u32) * (bw + 6), bw)
}

pub fn on_click(px: u32, py: u32) -> bool {
    let (wx, wy, w, h) = geometry();
    if px < wx || py < wy || px >= wx + w || py >= wy + h {
        return false;
    }
    let lx = px - wx;
    let ly = py - wy;

    if ly >= TOOL_Y && ly < TOOL_Y + TOOL_H {
        for i in 0..2 {
            let (bx, bw) = button_rect_in(w, i);
            if lx >= bx && lx < bx + bw {
                match i {
                    0 => save(),
                    1 => {
                        // Revert: reload from disk.
                        let id = FILE_ID.load(Ordering::Relaxed);
                        if id != 0 && vfs::exists(id) {
                            open_file(id);
                        }
                    }
                    _ => {}
                }
                return true;
            }
        }
        return true;
    }

    // Click in the text: put the cursor on the clicked line.
    if ly >= TEXT_Y {
        let target_line = ((ly - TEXT_Y) / LINE_H) as usize + unsafe { TOP_LINE };
        unsafe {
            let p = core::ptr::addr_of!(TEXT) as *const u8;
            let mut line = 0usize;
            let mut i = 0usize;
            while i < LEN && line < target_line {
                if core::ptr::read_volatile(p.add(i)) == b'\n' {
                    line += 1;
                }
                i += 1;
            }
            CURSOR = i.min(LEN);
        }
        return true;
    }
    true
}

/// A key was pressed while the editor had focus.
pub fn handle_key(key: u8) -> bool {
    use crate::dev::keyboard::{
        KEY_BACKSPACE, KEY_DELETE, KEY_DOWN, KEY_ENTER, KEY_LEFT, KEY_RIGHT, KEY_UP,
    };

    match key {
        KEY_ENTER => insert(b'\n'),
        KEY_BACKSPACE => delete_back(),
        KEY_DELETE => delete_forward(),
        KEY_LEFT => unsafe {
            if CURSOR > 0 {
                CURSOR -= 1;
            }
        },
        KEY_RIGHT => unsafe {
            if CURSOR < LEN {
                CURSOR += 1;
            }
        },
        KEY_UP => move_up(),
        KEY_DOWN => move_down(),
        ch if ch >= 0x20 && ch < 0x7F => insert(ch),
        _ => {}
    }
    true
}

// ---------------------------------------------------------------------------
//  Rendering
// ---------------------------------------------------------------------------

fn draw_clipped(text: &str, x: u32, y: u32, max_x: u32, r: u8, g: u8, b: u8) {
    let mut cx = x;
    for &ch in text.as_bytes() {
        if cx + font::GLYPH_WIDTH > max_x {
            break;
        }
        font::draw_char(ch, cx, y, r, g, b);
        cx += font::GLYPH_WIDTH;
    }
}

pub fn render() {
    let (wx, wy, w, h) = geometry();

    // Toolbar.
    let labels = ["Save", "Revert"];
    for (i, label) in labels.iter().enumerate() {
        let (bx, bw) = button_rect_in(w, i);
        framebuffer::fill_rect(wx + bx, wy + TOOL_Y, bw, TOOL_H - 2, 0x24, 0x2C, 0x42);
        framebuffer::fill_rect(wx + bx, wy + TOOL_Y, bw, 1, 0x10, 0xB9, 0x81);
        draw_clipped(label, wx + bx + 8, wy + TOOL_Y + 4, wx + bx + bw - 4, 0xFF, 0xFF, 0xFF);
    }

    // Filename + dirty marker on the right of the toolbar.
    let (dirty, flen) = unsafe { (DIRTY, FILE_NAME_LEN) };
    let mut name_buf = [0u8; vfs::MAX_NAME + 2];
    unsafe {
        name_buf[..flen].copy_from_slice(&FILE_NAME[..flen]);
    }
    if dirty && flen + 1 <= name_buf.len() {
        name_buf[flen] = b'*';
    }
    let name_len = if dirty { flen + 1 } else { flen };
    if let Ok(s) = core::str::from_utf8(&name_buf[..name_len]) {
        let tw = (name_len as u32) * font::GLYPH_WIDTH;
        let x = wx + w - 8 - tw;
        draw_clipped(s, x, wy + TOOL_Y + 4, wx + w - 8, 0xAA, 0xCC, 0xFF);
    }

    // Text area.
    let text_h = h.saturating_sub(TEXT_Y + 20);
    let max_lines = (text_h / LINE_H) as usize;
    framebuffer::fill_rect(wx + 4, wy + TEXT_Y - 4, w - 8, text_h + 4, 0x12, 0x16, 0x24);

    let cur_line = cursor_line();
    // Scroll so the cursor is visible.
    unsafe {
        if cur_line < TOP_LINE {
            TOP_LINE = cur_line;
        } else if max_lines > 0 && cur_line >= TOP_LINE + max_lines {
            TOP_LINE = cur_line + 1 - max_lines;
        }
    }
    let top = unsafe { TOP_LINE };

    let len = unsafe { LEN };
    let p = unsafe { core::ptr::addr_of!(TEXT) as *const u8 };
    let mut i = 0usize;
    let mut line = 0usize;
    let mut shown = 0usize;
    while i <= len && shown < max_lines {
        let start = i;
        while i < len && unsafe { core::ptr::read_volatile(p.add(i)) } != b'\n' {
            i += 1;
        }
        if line >= top {
            // Copy the line out rather than borrowing it: handing out a &str
            // into a `static mut` buffer is the aliasing the optimiser likes to
            // exploit, and this is a handful of bytes.
            let n = (i - start).min(LINE_MAX);
            let mut lbuf = [0u8; LINE_MAX];
            for (k, slot) in lbuf.iter_mut().enumerate().take(n) {
                *slot = unsafe { core::ptr::read_volatile(p.add(start + k)) };
            }
            if let Ok(s) = core::str::from_utf8(&lbuf[..n]) {
                draw_clipped(s, wx + PAD_X, wy + TEXT_Y + (shown as u32) * LINE_H, wx + w - PAD_X, 0xE0, 0xE0, 0xE0);
            }
            shown += 1;
        }
        line += 1;
        if i >= len {
            break;
        }
        i += 1;      // skip the newline
    }

    // Caret.
    let cur = unsafe { CURSOR };
    let cur_line_no = cursor_line();
    if cur_line_no >= top && cur_line_no - top < max_lines {
        let col = (cur - line_start(cur)) as u32;
        let cx = wx + PAD_X + col * font::GLYPH_WIDTH;
        let cy = wy + TEXT_Y + ((cur_line_no - top) as u32) * LINE_H;
        framebuffer::fill_rect(cx, cy, 2, LINE_H, 0x10, 0xB9, 0x81);
    }

    // Status.
    let sy = wy + h - 18;
    unsafe {
        if STATUS_LEN > 0 {
            if let Ok(s) = core::str::from_utf8(&STATUS[..STATUS_LEN]) {
                draw_clipped(s, wx + 8, sy, wx + w - 8, 0xAA, 0xCC, 0xFF);
            }
        }
        let mut tmp = [0u8; 24];
        let n = fmt_dec(LEN as u64, &mut tmp);
        if let Ok(s) = core::str::from_utf8(&tmp[..n]) {
            let tw = (n as u32) * font::GLYPH_WIDTH;
            draw_clipped(s, wx + w - 8 - tw, sy, wx + w - 8, 0x88, 0x88, 0x88);
        }
        let mut b2 = [0u8; 32];
        let mut k = 0;
        for &b in b"bytes + EOF" {
            if k < b2.len() {
                b2[k] = b;
                k += 1;
            }
        }
        if let Ok(s) = core::str::from_utf8(&b2[..k]) {
            let tw = ((n as u32) + 0) * font::GLYPH_WIDTH;
            draw_clipped(s, wx + w - 8 - tw - 56, sy, wx + w - 8, 0x66, 0x66, 0x66);
        }
    }

    serial::print_str("[apps] editor: rendered\n");
}

fn fmt_dec(mut v: u64, out: &mut [u8]) -> usize {
    let mut tmp = [0u8; 20];
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
