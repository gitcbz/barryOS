//! File manager — browses the RAM filesystem for real.
//!
//! The previous version drew a hardcoded list of four names and looked up
//! nothing.  This one walks the actual VFS: it lists directories, navigates
//! in and out, selects, and creates / renames / deletes entries.  Naming
//! happens in an inline input line at the bottom of the window, so keystrokes
//! go here while it is active (see `main`'s focus-based routing).

use crate::dev::framebuffer;
use crate::fs::{perm, ramfs, vfs};
use crate::serial;
use crate::wm::font;
use crate::wm::window;
use core::sync::atomic::{AtomicU64, Ordering};

const FM_POS_X: u32 = 40;
const FM_POS_Y: u32 = 30;
const FM_W: u32 = 430;
const FM_H: u32 = 330;

/// Layout bands inside the window, measured from its top-left.
const PATH_Y: u32 = 24;
const PATH_H: u32 = 20;
const TOOL_Y: u32 = 46;
const TOOL_H: u32 = 24;
const LIST_Y: u32 = 74;
const ROW_H: u32 = 18;
const MAX_ROWS: usize = 16;

const BTN_LABELS: [&str; 5] = ["New File", "New Folder", "Rename", "Delete", "Open"];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Browse,
    NewFile,
    NewDir,
    Rename,
}

static WIN_ID: AtomicU64 = AtomicU64::new(0);

/// Directory being browsed, and the selected row (usize::MAX = none).
static CWD: AtomicU64 = AtomicU64::new(vfs::ROOT_ID);
static SELECTED: AtomicU64 = AtomicU64::new(u64::MAX);

static mut MODE: Mode = Mode::Browse;
static mut ROWS: [vfs::DirEntry; MAX_ROWS] = [vfs::DirEntry::empty(); MAX_ROWS];
static mut ROW_COUNT: usize = 0;
static mut HAS_PARENT: bool = false;
/// Name being typed in the inline input line.
static mut INPUT: [u8; vfs::MAX_NAME + 1] = [0; vfs::MAX_NAME + 1];
static mut INPUT_LEN: usize = 0;
/// One-line feedback ("created /x", "name is taken", ...).
static mut STATUS: [u8; 48] = [0; 48];
static mut STATUS_LEN: usize = 0;

pub fn window_id() -> u64 {
    WIN_ID.load(Ordering::Relaxed)
}

pub fn cwd() -> u64 {
    CWD.load(Ordering::Relaxed)
}

/// Create the window, if it is not already open.
pub fn init() {
    if window::exists(window_id()) {
        window::focus(window_id());
        return;
    }
    let id = window::create(FM_POS_X, FM_POS_Y, FM_W, FM_H, "Files");
    WIN_ID.store(id, Ordering::SeqCst);
    CWD.store(vfs::ROOT_ID, Ordering::SeqCst);
    SELECTED.store(u64::MAX, Ordering::SeqCst);
    unsafe {
        MODE = Mode::Browse;
        STATUS_LEN = 0;
    }
    refresh();
    serial::print_str("[apps] file manager: window created id=");
    serial::print_hex(id);
    serial::print_str("\n");
}

pub fn open() {
    if window::exists(window_id()) {
        window::focus(window_id());
        return;
    }
    init();
}

// ---------------------------------------------------------------------------
//  Directory state
// ---------------------------------------------------------------------------

fn refresh() {
    let cwd = cwd();
    unsafe {
        ROW_COUNT = vfs::read_dir(cwd, &mut ROWS);
        HAS_PARENT = cwd != vfs::ROOT_ID;
    }
}

fn display_rows() -> usize {
    unsafe { ROW_COUNT + if HAS_PARENT { 1 } else { 0 } }
}

/// Entry index for a displayed row, or None for the ".." row.
fn entry_index(disp: usize) -> Option<usize> {
    unsafe {
        if HAS_PARENT {
            if disp == 0 {
                None
            } else {
                Some(disp - 1)
            }
        } else {
            Some(disp)
        }
    }
}

fn entry(disp: usize) -> Option<vfs::DirEntry> {
    let i = entry_index(disp)?;
    unsafe {
        if i < ROW_COUNT {
            Some(ROWS[i])
        } else {
            None
        }
    }
}

fn selected_entry() -> Option<vfs::DirEntry> {
    let sel = SELECTED.load(Ordering::Relaxed);
    if sel == u64::MAX {
        return None;
    }
    entry(sel as usize)
}

fn set_status(msg: &str) {
    unsafe {
        let n = msg.len().min(STATUS.len());
        for (i, &b) in msg.as_bytes().iter().enumerate().take(n) {
            STATUS[i] = b;
        }
        STATUS_LEN = n;
    }
}

fn cwd_path() -> ([u8; 96], usize) {
    let mut buf = [0u8; 96];
    let n = vfs::path_of(cwd(), &mut buf);
    (buf, n)
}

// ---------------------------------------------------------------------------
//  Actions
// ---------------------------------------------------------------------------

fn navigate_to(id: u64) {
    CWD.store(id, Ordering::SeqCst);
    SELECTED.store(u64::MAX, Ordering::SeqCst);
    refresh();
    serial::print_str("[fm] entered id=");
    serial::print_hex(id);
    serial::print_str("\n");
}

fn go_up() {
    let parent = vfs::get_parent(cwd());
    if parent != 0 {
        navigate_to(parent);
    }
}

fn open_selected() {
    let Some(e) = selected_entry() else {
        set_status("nothing selected");
        return;
    };
    match e.vtype {
        vfs::VnodeType::Dir => {
            // Entering a directory needs execute on it.
            if !perm::allowed(e.id, perm::X) {
                set_status("permission denied");
                return;
            }
            navigate_to(e.id)
        }
        _ => {
            if !perm::allowed(e.id, perm::R) {
                set_status("permission denied (read)");
                return;
            }
            // Hand the file to the editor and raise it.
            crate::apps::editor::open_file(e.id);
            set_status("opened in editor");
        }
    }
}

fn delete_selected() {
    // Removing an entry rewrites the directory, so that is where the
    // permission has to be.
    if !perm::may_modify_dir(cwd()) {
        set_status("permission denied");
        return;
    }
    let Some(e) = selected_entry() else {
        set_status("nothing selected");
        return;
    };
    // Copy the name out first: `unlink` frees the vnode the name lives in.
    let mut name = [0u8; vfs::MAX_NAME + 1];
    let n = vfs::get_name(e.id, &mut name);
    let mut owned = [0u8; vfs::MAX_NAME + 1];
    owned[..n].copy_from_slice(&name[..n]);
    let Ok(name_str) = core::str::from_utf8(&owned[..n]) else {
        return;
    };

    if ramfs::unlink(cwd(), name_str) {
        set_status("deleted");
        SELECTED.store(u64::MAX, Ordering::SeqCst);
    } else {
        set_status("delete failed (not empty?)");
    }
    refresh();
}

fn begin(mode: Mode, prompt: &str) {
    unsafe {
        MODE = mode;
        INPUT_LEN = 0;
    }
    set_status(prompt);
}

fn commit_input() {
    let mode = unsafe { MODE };
    if mode == Mode::Browse {
        return;
    }
    let (len, mut buf) = unsafe { (INPUT_LEN, INPUT) };
    buf[len] = 0;
    let Ok(name) = core::str::from_utf8(&buf[..len]) else {
        set_status("invalid name");
        return;
    };

    // All three of these rewrite the directory listing, so write+search on the
    // directory is what they need.
    if !perm::may_modify_dir(cwd()) {
        set_status("permission denied");
        unsafe {
            MODE = Mode::Browse;
            INPUT_LEN = 0;
        }
        return;
    }

    match mode {
        Mode::NewFile => {
            if ramfs::create_file_in(cwd(), name) != 0 {
                set_status("file created");
            } else {
                set_status("could not create file");
            }
        }
        Mode::NewDir => {
            if ramfs::create_dir_in(cwd(), name) != 0 {
                set_status("folder created");
            } else {
                set_status("could not create folder");
            }
        }
        Mode::Rename => {
            if let Some(e) = selected_entry() {
                let mut old_buf = [0u8; vfs::MAX_NAME + 1];
                let n = vfs::get_name(e.id, &mut old_buf);
                let mut old_owned = [0u8; vfs::MAX_NAME + 1];
                old_owned[..n].copy_from_slice(&old_buf[..n]);
                if let Ok(old) = core::str::from_utf8(&old_owned[..n]) {
                    if ramfs::rename(cwd(), old, name) {
                        set_status("renamed");
                    } else {
                        set_status("rename failed");
                    }
                }
            }
        }
        Mode::Browse => {}
    }

    unsafe {
        MODE = Mode::Browse;
        INPUT_LEN = 0;
    }
    refresh();
}

// ---------------------------------------------------------------------------
//  Input
// ---------------------------------------------------------------------------

/// A click landed somewhere in this window (absolute screen coordinates).
/// Returns true if it was consumed.
pub fn on_click(px: u32, py: u32) -> bool {
    let (wx, wy, w, h) = geometry();
    if px < wx || py < wy || px >= wx + w || py >= wy + h {
        return false;
    }
    let lx = px - wx;
    let ly = py - wy;

    // Toolbar.
    if ly >= TOOL_Y && ly < TOOL_Y + TOOL_H {
        for i in 0..BTN_LABELS.len() {
            let (bx, bw) = button_rect_in(w, i);
            if lx >= bx && lx < bx + bw {
                match i {
                    0 => begin(Mode::NewFile, "new file name:"),
                    1 => begin(Mode::NewDir, "new folder name:"),
                    2 => {
                        if selected_entry().is_some() {
                            begin(Mode::Rename, "new name:");
                        } else {
                            set_status("select something to rename");
                        }
                    }
                    3 => delete_selected(),
                    4 => open_selected(),
                    _ => {}
                }
                return true;
            }
        }
        return true;
    }

    // List.
    if ly >= LIST_Y && ly < LIST_Y + (MAX_ROWS as u32) * ROW_H {
        let row = ((ly - LIST_Y) / ROW_H) as usize;
        if row < display_rows() {
            // First click selects, a second click on the same row activates.
            // There is no double-click timing to rely on, and this is what a
            // single-click file manager does anyway.
            if SELECTED.load(Ordering::Relaxed) == row as u64 {
                if entry_index(row).is_none() {
                    go_up();
                } else {
                    open_selected();
                }
            } else {
                SELECTED.store(row as u64, Ordering::SeqCst);
                set_status("");
            }
            return true;
        }
    }
    true
}

/// A key was pressed while this window had focus.  Returns true if the window
/// needs repainting.
pub fn handle_key(key: u8) -> bool {
    use crate::dev::keyboard::{KEY_BACKSPACE, KEY_ENTER, KEY_ESC};

    if unsafe { MODE } == Mode::Browse {
        // Not typing a name: Enter opens, Delete is unavailable without a
        // Delete key in the scancode map, so Enter is the only shortcut.
        if key == KEY_ENTER {
            open_selected();
            return true;
        }
        return false;
    }

    match key {
        KEY_ESC => {
            unsafe { MODE = Mode::Browse };
            set_status("cancelled");
        }
        KEY_ENTER => commit_input(),
        KEY_BACKSPACE => unsafe {
            if INPUT_LEN > 0 {
                INPUT_LEN -= 1;
            }
        },
        ch if ch >= 0x20 && ch < 0x7F => unsafe {
            if INPUT_LEN < vfs::MAX_NAME {
                INPUT[INPUT_LEN] = ch;
                INPUT_LEN += 1;
            }
        },
        _ => {}
    }
    true
}

// ---------------------------------------------------------------------------
//  Geometry
// ---------------------------------------------------------------------------

fn geometry() -> (u32, u32, u32, u32) {
    window::geometry(window_id()).unwrap_or((FM_POS_X, FM_POS_Y, FM_W, FM_H))
}

/// X offset and width of toolbar button `i` for a window `w` wide.
fn button_rect_in(w: u32, i: usize) -> (u32, u32) {
    let margin = 6u32;
    let inner = w.saturating_sub(margin * 2);
    let n = BTN_LABELS.len() as u32;
    let bw = inner / n;
    (margin + (i as u32) * bw, bw.saturating_sub(4))
}

fn row_rect_in(w: u32, i: usize) -> (u32, u32, u32, u32) {
    (4, LIST_Y + (i as u32) * ROW_H, w.saturating_sub(8), ROW_H)
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
    let (_br, bg, bb) = window::background(window_id()).unwrap_or((0x1A, 0x1F, 0x35));

    // Path bar.
    framebuffer::fill_rect(wx + 4, wy + PATH_Y, w - 8, PATH_H, 0x10, 0x14, 0x24);
    let (path, n) = cwd_path();
    if let Ok(s) = core::str::from_utf8(&path[..n]) {
        draw_clipped(s, wx + 10, wy + PATH_Y + 2, wx + w - 8, 0x10, 0xB9, 0x81);
    }

    // Toolbar.
    for i in 0..BTN_LABELS.len() {
        let (bx, bw) = button_rect_in(w, i);
        framebuffer::fill_rect(wx + bx, wy + TOOL_Y, bw, TOOL_H - 2, 0x24, 0x2C, 0x42);
        framebuffer::fill_rect(wx + bx, wy + TOOL_Y, bw, 1, 0x10, 0xB9, 0x81);
        let label = BTN_LABELS[i];
        let tw = (label.len() as u32) * font::GLYPH_WIDTH;
        let tx = wx + bx + bw.saturating_sub(tw) / 2;
        draw_clipped(label, tx, wy + TOOL_Y + 4, wx + bx + bw - 2, 0xFF, 0xFF, 0xFF);
    }

    // Listing.
    let sel = SELECTED.load(Ordering::Relaxed);
    let rows = display_rows();
    for i in 0..rows.min(MAX_ROWS) {
        let (rx, ry, rw, rh) = row_rect_in(w, i);
        if sel == i as u64 {
            framebuffer::fill_rect(wx + rx, wy + ry, rw, rh, 0x2E, 0x6E, 0x58);
        }

        if entry_index(i).is_none() {
            // ".." row.
            framebuffer::fill_rect(wx + rx + 6, wy + ry + 4, 8, 10, 0xC8, 0xA0, 0x40);
            draw_clipped("..", wx + rx + 22, wy + ry + 1, wx + w, 0xAA, 0xCC, 0xFF);
            continue;
        }
        let Some(e) = entry(i) else { continue };

        let is_dir = e.vtype == vfs::VnodeType::Dir;
        // Folder is a filled square, file a smaller lighter one.
        if is_dir {
            framebuffer::fill_rect(wx + rx + 6, wy + ry + 4, 9, 11, 0xE0, 0xB0, 0x40);
        } else {
            framebuffer::fill_rect(wx + rx + 6, wy + ry + 4, 9, 11, 0x10, 0xB9, 0x81);
        }

        // Permission string: `chmod` needs something visible to change.
        let mut mode = [0u8; 10];
        let p = vfs::get_perm(e.id).unwrap_or(vfs::Perm { uid: 0, gid: 0, mode: 0 });
        perm::format_mode(e.vtype, p.mode, &mut mode);
        if let Ok(s) = core::str::from_utf8(&mode) {
            draw_clipped(s, wx + rx + 20, wy + ry + 1, wx + rx + rw - 90, 0x80, 0x90, 0xA8);
        }

        let (nr, ng, nb) = if is_dir { (0xFF, 0xD0, 0x80) } else { (0xFF, 0xFF, 0xFF) };
        draw_clipped(e.name_str(), wx + rx + 104, wy + ry + 1, wx + rx + rw - 90, nr, ng, nb);

        // Size column, right-aligned-ish.
        if !is_dir {
            let mut tmp = [0u8; 24];
            let sn = fmt_size(e.size, &mut tmp);
            if let Ok(s) = core::str::from_utf8(&tmp[..sn]) {
                draw_clipped(s, wx + w - 92, wy + ry + 1, wx + w - 8, 0xAA, 0xCC, 0xFF);
            }
        }
    }
    let _ = h;

    // Status / input line.
    let sy = wy + h - 20;
    framebuffer::fill_rect(wx + 4, sy, w - 8, 18, 0x10, 0x14, 0x24);
    unsafe {
        if MODE != Mode::Browse {
            let prompt = match MODE {
                Mode::NewFile => "new file: ",
                Mode::NewDir => "new folder: ",
                Mode::Rename => "rename to: ",
                Mode::Browse => "",
            };
            draw_clipped(prompt, wx + 10, sy + 1, wx + w - 8, 0x10, 0xB9, 0x81);
            let px = wx + 10 + (prompt.len() as u32) * font::GLYPH_WIDTH;
            if let Ok(s) = core::str::from_utf8(&INPUT[..INPUT_LEN]) {
                draw_clipped(s, px, sy + 1, wx + w - 8, 0xFF, 0xFF, 0xFF);
            }
            // Caret.
            let cx = px + (INPUT_LEN as u32) * font::GLYPH_WIDTH;
            framebuffer::fill_rect(cx, sy + 2, 2, 14, 0x10, 0xB9, 0x81);
        } else if STATUS_LEN > 0 {
            if let Ok(s) = core::str::from_utf8(&STATUS[..STATUS_LEN]) {
                draw_clipped(s, wx + 10, sy + 1, wx + w - 8, 0xAA, 0xCC, 0xFF);
            }
        } else {
            let n = display_rows();
            let mut tmp = [0u8; 24];
            let sn = fmt_size(n as u64, &mut tmp);
            if let Ok(s) = core::str::from_utf8(&tmp[..sn]) {
                let mut line = [0u8; 32];
                let mut k = 0;
                for &b in s.as_bytes() {
                    line[k] = b;
                    k += 1;
                }
                for &b in b" entries" {
                    line[k] = b;
                    k += 1;
                }
                if let Ok(t) = core::str::from_utf8(&line[..k]) {
                    draw_clipped(t, wx + 10, sy + 1, wx + w - 8, 0x88, 0x88, 0x88);
                }
            }
        }
    }

    serial::print_str("[apps] file manager: rendered (");
    serial::print_hex(display_rows() as u64);
    serial::print_str(" entries)\n");
}

/// Decimal into `out`; returns the length.
fn fmt_size(mut v: u64, out: &mut [u8]) -> usize {
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
