//! Browser — a page, as text and pictures.
//!
//! What this is: a window with an address bar, a scrollable view of a page
//! laid out by `apps::web`, links you can click, a back button, and a status
//! line that says what the network is doing.
//!
//! The rendering itself is not here.  `apps::web` turns markup, stylesheets
//! and scripts into lines of styled characters and decodes the pictures;
//! this puts them on screen and owns the parts that only exist in a browser —
//! the address bar, the history, the load order, and the flat byte buffers a
//! framebuffer can be drawn from.
//!
//! The fetch is not run here either.  `net::http` owns it and advances it from
//! `net::tick`, which the input loop calls; this module starts a request and
//! then paints whatever state that request is in.  That is why the window
//! keeps redrawing, and why the pointer keeps moving, while a page loads.
//!
//! Loading a page is four steps, and the order is the whole of it:
//!
//!   1. the document
//!   2. the stylesheets and scripts its markup named
//!   3. the scripts run, because a script can add a picture
//!   4. the pictures, because running a script can add one
//!
//! Laying out before step 2 would show the page twice, once wrong; fetching
//! pictures before step 3 would miss everything a page draws for itself.

use alloc::string::String;
use crate::apps::web;
use crate::dev::keyboard::{
    KEY_BACKSPACE, KEY_DELETE, KEY_DOWN, KEY_END, KEY_ENTER, KEY_HOME, KEY_LEFT,
    KEY_PAGE_DOWN, KEY_PAGE_UP, KEY_RIGHT, KEY_UP,
};
use crate::net::http;
use crate::serial;
use crate::wm::font;
use crate::wm::window::{self, TITLE_BAR_H};
use core::sync::atomic::Ordering;

static mut BR_WIN_ID: u64 = 0;

const WIN_X: u32 = 56;
const WIN_Y: u32 = 40;
const WIN_W: u32 = 720;
const WIN_H: u32 = 440;

/// Glyph cell.
const CW: u32 = 8;
const CH: u32 = 16;

/// The paper a page is drawn on.
///
/// Pages are designed for white paper and their stylesheets say so — a rule
/// that sets text to black means black on white, and drawing it on the
/// window's own dark background makes the commonest case in the world
/// unreadable.  It is also the colour a picture with transparency is blended
/// onto, because the only thing that knows what is behind a transparent pixel
/// is whatever is drawing the paper.
const PAPER: (u8, u8, u8) = (0xFA, 0xF9, 0xF5);

/// Above the page: the back and forward buttons, then the address bar.  All of
/// it measured from the bottom of the title bar so the drawing and the click
/// test cannot drift apart.  They did once: the hit test put the boundary at
/// `wy + 26` while the field was *drawn* from `wy+20` to `wy+40`, so clicking
/// the address bar almost always landed in the page.
const TOOLBAR_H: u32 = 28;
/// The toolbar's own left margin, the width of each of the two buttons, and
/// the gap between a button and the text in the field.  Named because the
/// drawing and the click test both use them and they must not drift: the same
/// mistake, made twice before, put the boundary of the address field eight
/// pixels from where the field was drawn, so a click on the first character
/// landed on the second.
const TOOLBAR_X: u32 = 4;
const BUTTON_W: u32 = 22;
const FIELD_PAD: u32 = 4;
/// Below it: where the fetch got to.
const STATUS_H: u32 = 22;

/// Text column limit, from the window width less the scroll margin.
const COLS: usize = ((WIN_W - 20) / CW) as usize;
/// Rows of page visible at once.
const ROWS: usize = ((WIN_H - TITLE_BAR_H - TOOLBAR_H - STATUS_H - 8) / CH) as usize;

/// The document buffer, matched to what the transports can actually deliver —
/// a page kept to less than the socket received is a page silently cut short
/// by this file rather than by the network.
const PAGE_CAP: usize = crate::net::tcp::RX_CAP;

/// Lines a page may be laid out into.  A quarter of a megabyte of text is
/// some three thousand lines at this width, so this is slack rather than a
/// limit anyone will meet.
const MAX_LINES: usize = 4096;
const MAX_URL: usize = 256;
/// A link's URL, and the total kept for all of them.  A page's navigation is
/// a few hundred bytes of URL; a page with more links than this is a page
/// whose links nobody will click.
const MAX_LINKS: usize = 254;
const LINK_BYTES: usize = 24 * 1024;

/// Pictures kept for one page, and the bytes they may use between them.  A
/// decoded picture is three bytes a pixel, so a three-megabyte arena is a
/// little over a million pixels — one large photograph, or a dozen small
/// ones, which is what a page that reads in a text browser has.
const IMG_MAX: usize = 12;
const IMG_BYTES: usize = 3 * 1024 * 1024;
const IMG_SRC_BYTES: usize = 8 * 1024;

/// Addresses kept for the back button.  Not a list of everywhere the user has
/// ever been: a ring this size is enough to get back to what they were
/// reading, and the entries are fixed-size so nothing here touches the heap.
const HIST_MAX: usize = 24;
const HIST_URL: usize = 256;

// ---------------------------------------------------------------------------
//  Buffers
// ---------------------------------------------------------------------------

/// The page as it arrived.
static mut PAGE: [u8; PAGE_CAP] = [0; PAGE_CAP];
static mut PAGE_LEN: usize = 0;

/// The same bytes with the markup resolved into lines, plus one byte per
/// character saying which style it has and which link it belongs to.
static mut TEXT: [u8; PAGE_CAP] = [0; PAGE_CAP];
static mut STYLE_AT: [u8; PAGE_CAP] = [0; PAGE_CAP];
static mut LINK_AT_BYTE: [u8; PAGE_CAP] = [0; PAGE_CAP];
static mut LINE_AT: [u32; MAX_LINES] = [0; MAX_LINES];
static mut LINE_LEN: [u32; MAX_LINES] = [0; MAX_LINES];
/// The picture on a line, if it is a picture line: slot plus one, and the
/// size to draw it at.  Held beside the lines rather than inside them because
/// the renderer walks a flat byte buffer and has no access to the `Line`s.
static mut LINE_IMG: [u16; MAX_LINES] = [0; MAX_LINES];
static mut LINE_COLS: [u16; MAX_LINES] = [0; MAX_LINES];
static mut LINE_ROWS: [u16; MAX_LINES] = [0; MAX_LINES];
static mut LINE_COUNT: usize = 0;
/// Which line the view is scrolled to.
static mut TOP: usize = 0;

/// Style records, and one index per byte of laid-out text.
///
/// The renderer draws from a flat byte buffer, so a style has to be reachable
/// from a byte offset.  A record is four bytes — three of colour and one of
/// attributes — and there are at most sixty-four of them on a page, because a
/// page's text has a handful of appearances rather than thousands.
const MAX_STYLES: usize = 64;
static mut STYLE_TAB: [[u8; 4]; MAX_STYLES] = [[0; 4]; MAX_STYLES];
static mut STYLE_N: usize = 0;

const ATTR_BOLD: u8 = 1;
const ATTR_ITALIC: u8 = 2;
const ATTR_UNDERLINE: u8 = 4;
const ATTR_STRIKE: u8 = 8;

/// Where links live while a page is on screen.  An arena rather than a `Vec`
/// of `String`s because everything else the page allocates is freed in one
/// step when the page is replaced, and a link table that outlived its page
/// would be a table of dangling pointers.
static mut LINK_TEXT: [u8; LINK_BYTES] = [0; LINK_BYTES];
static mut LINK_FROM: [u32; MAX_LINKS + 1] = [0; MAX_LINKS + 1];
static mut LINK_N: usize = 0;

/// Where pictures live.  One arena, packed from the front, so a page's
/// pictures are freed by resetting two counters.
static mut IMG_PIX: [u8; IMG_BYTES] = [0; IMG_BYTES];
static mut IMG_AT: [u32; IMG_MAX + 1] = [0; IMG_MAX + 1];
static mut IMG_W: [u32; IMG_MAX] = [0; IMG_MAX];
static mut IMG_H: [u32; IMG_MAX] = [0; IMG_MAX];
static mut IMG_N: usize = 0;
static mut IMG_SRC: [u8; IMG_SRC_BYTES] = [0; IMG_SRC_BYTES];
static mut IMG_SRC_AT: [u32; IMG_MAX + 1] = [0; IMG_MAX + 1];

/// Address bar contents.
static mut URL: [u8; MAX_URL] = [0; MAX_URL];
static mut URL_LEN: usize = 0;
static mut CARET: usize = 0;

/// Where the reader has been, and where they are in it.
static mut HIST: [u8; HIST_MAX * HIST_URL] = [0; HIST_MAX * HIST_URL];
static mut HIST_LEN: [u16; HIST_MAX] = [0; HIST_MAX];
static mut HIST_N: usize = 0;
static mut HIST_AT: usize = 0;

/// The phase the last paint was made in, so the window is only redrawn when
/// something has actually changed.
static mut LAST_PHASE: u8 = 255;
static mut LAST_BODY: usize = usize::MAX;
/// Set once the finished body has been copied out and laid out.
static mut CAPTURED: bool = false;

/// The page the browser opens with.  Kept short and plain on purpose: the
/// point is to prove the network works, not to funnel a modern site through a
/// text renderer.
pub const HOME: &str = "example.com/";

/// The page the boot self-test loads.  A small real one with every kind of
/// thing in it: markup, a stylesheet, scripts, a picture and links.  It is a
/// test target and not a home page — what the browser opens on is `HOME`.
///
/// It is deliberately small.  This runs on every boot, and a page with forty
/// references would spend longer fetching them than the rest of the kernel
/// spends starting up.
pub const SELFTEST_URL: &str = "https://www.iana.org/domains/reserved";

/// And one picture, fetched and decoded on its own.  From a site this browser
/// exists to reach, so that the thing the boot log proves is the thing that
/// matters rather than a fixture.
pub const SELFTEST_PICTURE: &str =
    "https://i0.hdslb.com/bfs/static/jinkela/long/images/512.png";

pub fn window_id() -> u64 {
    unsafe { BR_WIN_ID }
}

/// State setup without a window; see `terminal::init_state`.
pub fn init_state() {
    set_url(HOME);
    // Every page load frees everything the last one allocated, and this is
    // where "everything the last one allocated" starts.  Taken here rather
    // than left at zero, because a reset to zero is a reset to the beginning
    // of the heap, which is where the rest of the kernel lives.
    unsafe { LOAD_MARK = crate::mem::heap::mark() };
}

pub fn init() {
    init_state();
    let id = window::create(WIN_X, WIN_Y, WIN_W, WIN_H, "Browser");
    unsafe { BR_WIN_ID = id; }
    serial::print_str("[apps] browser: window created id=");
    serial::print_hex(id);
    serial::print_str("\n");
}

/// Open the browser, or raise it if it is already up.  A URL may be given.
pub fn open_with(url: &str) {
    if !window::exists(window_id()) {
        init();
    } else {
        window::focus(window_id());
    }
    if !url.is_empty() {
        set_url(url);
        navigate(url, true);
    }
}

pub fn open() {
    open_with("");
}

fn set_url(s: &str) {
    let n = s.len().min(MAX_URL);
    let base = core::ptr::addr_of_mut!(URL) as *mut u8;
    for i in 0..n {
        unsafe { core::ptr::write_volatile(base.add(i), s.as_bytes()[i]) };
    }
    unsafe {
        URL_LEN = n;
        CARET = n;
    }
}

/// The address bar contents.  The slice borrows the static directly: the bar
/// is the only writer and every caller consumes it before anything can type.
fn url_bytes() -> &'static [u8] {
    let n = unsafe { URL_LEN };
    unsafe { core::slice::from_raw_parts(core::ptr::addr_of!(URL) as *const u8, n) }
}

/// The page's URL, as the script sees it.
fn url_str() -> &'static str {
    let n = unsafe { URL_LEN };
    let base = core::ptr::addr_of!(URL) as *const u8;
    let mut buf = [0u8; MAX_URL];
    for (i, slot) in buf.iter_mut().enumerate().take(n) {
        *slot = unsafe { core::ptr::read_volatile(base.add(i)) };
    }
    unsafe {
        URL_VIEW = buf;
        core::str::from_utf8(&URL_VIEW[..n]).unwrap_or("")
    }
}
static mut URL_VIEW: [u8; MAX_URL] = [0; MAX_URL];

// ---------------------------------------------------------------------------
//  Where the reader has been
// ---------------------------------------------------------------------------

unsafe fn hist_str(i: usize) -> &'static str {
    if i >= HIST_N {
        return "";
    }
    let n = HIST_LEN[i] as usize;
    core::str::from_utf8(core::slice::from_raw_parts(hist_row(i), n)).unwrap_or("")
}

unsafe fn hist_row(i: usize) -> *const u8 {
    (core::ptr::addr_of!(HIST) as *const u8).add(i * HIST_URL)
}

unsafe fn hist_store(i: usize, s: &str) {
    let dst = hist_row(i) as *mut u8;
    let n = s.len().min(HIST_URL);
    for k in 0..n {
        core::ptr::write_volatile(dst.add(k), s.as_bytes()[k]);
    }
    HIST_LEN[i] = n as u16;
}

/// Record a new address, dropping whatever was in front of it.
///
/// A link someone clicked is a new place to be, so the pages they could have
/// gone forward to are no longer reachable — which is what every browser
/// does, and the alternative is a forward button that goes somewhere the
/// reader never chose.
unsafe fn hist_push(url: &str) {
    if HIST_N > 0 && hist_str(HIST_AT) == url {
        return;                                     // reloading is not a new place
    }
    HIST_N = HIST_AT + 1;
    if HIST_N >= HIST_MAX {
        // Full at the front: drop the oldest, which is the one nobody is
        // coming back for.
        for i in 1..HIST_MAX {
            core::ptr::copy_nonoverlapping(hist_row(i), hist_row(i) as *mut u8, HIST_URL);
            HIST_LEN[i - 1] = HIST_LEN[i];
        }
        HIST_N -= 1;
    }
    hist_store(HIST_N, url);
    HIST_N += 1;
    HIST_AT = HIST_N - 1;
}

/// Change the address without recording a new place.  A redirect — a header,
/// a meta tag or a script — is the same navigation arriving somewhere else.
unsafe fn hist_replace(url: &str) {
    if HIST_N == 0 {
        hist_push(url);
        return;
    }
    hist_store(HIST_AT, url);
}

/// Go to a URL.  `push` is false for a redirect.
fn navigate(url: &str, push: bool) {
    if url.is_empty() {
        return;
    }
    serial::print_str("[browser] navigate ");
    serial::print_str(url);
    serial::print_str("\n");
    unsafe {
        if push {
            hist_push(url);
        } else {
            hist_replace(url);
        }
        start_load(url);
    }
}

/// Move through the history rather than adding to it.
fn move_to(index: usize) {
    let n = unsafe { HIST_N };
    if index >= n {
        return;
    }
    unsafe { HIST_AT = index };
    let url = unsafe { hist_str(index) };
    let mut buf = [0u8; HIST_URL];
    let n = url.len().min(HIST_URL);
    buf[..n].copy_from_slice(&url.as_bytes()[..n]);
    let url = core::str::from_utf8(&buf[..n]).unwrap_or("");
    set_url(url);
    serial::print_str("[browser] back to ");
    serial::print_str(url);
    serial::print_str("\n");
    unsafe { start_load(url) };
}

fn back() {
    let at = unsafe { HIST_AT };
    if at > 0 {
        move_to(at - 1);
    }
}

fn forward() {
    let (at, n) = unsafe { (HIST_AT, HIST_N) };
    if at + 1 < n {
        move_to(at + 1);
    }
}

// ---------------------------------------------------------------------------
//  Input
// ---------------------------------------------------------------------------

/// A click puts the caret where it was clicked in the address bar, follows a
/// link in the page, or presses one of the buttons above it.
///
/// The browser has no forms, so there is exactly one place text can go and no
/// mode to get stuck in.  An earlier version had two, and the first fetch
/// moved the keyboard to the page with no way back short of closing the
/// window.
pub fn on_click(px: u32, py: u32) -> bool {
    let (wx, wy) = window::position(window_id()).unwrap_or((WIN_X, WIN_Y));
    let rel_y = py as i32 - wy as i32;
    if rel_y < (TITLE_BAR_H + TOOLBAR_H) as i32 {
        let rel_x = (px as i32 - wx as i32).max(0) as u32;
        let field_x = TOOLBAR_X + 2 * BUTTON_W;
        if (TOOLBAR_X..TOOLBAR_X + BUTTON_W).contains(&rel_x) {
            back();
        } else if (TOOLBAR_X + BUTTON_W..field_x).contains(&rel_x) {
            forward();
        } else if rel_x >= field_x + FIELD_PAD {
            // Clamp the caret to the text that is actually there.
            let col = ((rel_x - field_x - FIELD_PAD) as usize / CW as usize)
                .min(unsafe { URL_LEN });
            unsafe { CARET = col; }
        }
        return true;
    }

    // Below the toolbar: a link, or nothing at all.  Clicking the page does
    // not move the keyboard, because there is nowhere in the page for text to
    // go and stealing focus is how the address bar became unreachable.
    let top_y = (TITLE_BAR_H + TOOLBAR_H + 6) as i32;
    let row = ((rel_y - top_y).max(0) / CH as i32) as usize;
    let col = ((px as i32 - wx as i32 - 10).max(0) as usize) / CW as usize;
    if let Some(href) = link_under(row, col) {
        let base = base_url();
        if let Some(abs) = resolve_url(&base, href) {
            navigate(&abs, true);
        }
    }
    true
}

pub fn handle_key(key: u8) -> bool {
    match key {
        // Printable characters and the editing keys all go to the address bar.
        // There are no forms on a page, so this is the only place text can go
        // and there is no mode to be stuck in.
        KEY_ENTER => {
            let text = alloc::string::String::from_utf8_lossy(url_bytes()).into_owned();
            navigate(&text, true);
            true
        }
        KEY_BACKSPACE => {
            unsafe {
                if CARET > 0 {
                    let base = core::ptr::addr_of_mut!(URL) as *mut u8;
                    for i in CARET - 1..URL_LEN.saturating_sub(1) {
                        let v = core::ptr::read_volatile(base.add(i + 1));
                        core::ptr::write_volatile(base.add(i), v);
                    }
                    URL_LEN -= 1;
                    CARET -= 1;
                }
            }
            true
        }
        KEY_DELETE => {
            unsafe {
                if CARET < URL_LEN {
                    let base = core::ptr::addr_of_mut!(URL) as *mut u8;
                    for i in CARET..URL_LEN.saturating_sub(1) {
                        let v = core::ptr::read_volatile(base.add(i + 1));
                        core::ptr::write_volatile(base.add(i), v);
                    }
                    URL_LEN -= 1;
                }
            }
            true
        }
        KEY_LEFT => {
            unsafe {
                if CARET > 0 {
                    CARET -= 1;
                }
            }
            true
        }
        KEY_RIGHT => {
            unsafe {
                if CARET < URL_LEN {
                    CARET += 1;
                }
            }
            true
        }
        KEY_HOME => {
            unsafe { CARET = 0; }
            true
        }
        KEY_END => {
            unsafe { CARET = URL_LEN; }
            true
        }
        KEY_UP => {
            scroll(-1);
            true
        }
        KEY_DOWN => {
            scroll(1);
            true
        }
        KEY_PAGE_UP => {
            scroll(-(ROWS as isize));
            true
        }
        KEY_PAGE_DOWN => {
            scroll(ROWS as isize);
            true
        }
        ch if (0x20..0x7F).contains(&ch) => {
            unsafe {
                if URL_LEN < MAX_URL {
                    let base = core::ptr::addr_of_mut!(URL) as *mut u8;
                    for i in (CARET..URL_LEN).rev() {
                        let v = core::ptr::read_volatile(base.add(i));
                        core::ptr::write_volatile(base.add(i + 1), v);
                    }
                    core::ptr::write_volatile(base.add(CARET), ch);
                    URL_LEN += 1;
                    CARET += 1;
                }
            }
            true
        }
        _ => false,
    }
}

fn scroll(delta: isize) {
    let count = unsafe { LINE_COUNT };
    let max = count.saturating_sub(ROWS);
    unsafe {
        let t = TOP as isize + delta;
        TOP = t.clamp(0, max as isize) as usize;
    }
}

// ---------------------------------------------------------------------------
//  Links
// ---------------------------------------------------------------------------

/// The identifier for a URL, made once and shared by every run that points at
/// it.  A page's navigation menu is one URL in twenty places.
unsafe fn intern_link(href: &str) -> u8 {
    if href.is_empty() || LINK_N >= MAX_LINKS {
        return 0;
    }
    let body = core::slice::from_raw_parts(
        core::ptr::addr_of!(LINK_TEXT) as *const u8,
        LINK_BYTES,
    );
    for i in 0..LINK_N {
        let from = LINK_FROM[i] as usize;
        let to = LINK_FROM[i + 1] as usize;
        if &body[from..to] == href.as_bytes() {
            return (i + 1) as u8;
        }
    }
    let start = LINK_FROM[LINK_N] as usize;
    if start + href.len() > LINK_BYTES {
        return 0;
    }
    let dst = core::ptr::addr_of_mut!(LINK_TEXT) as *mut u8;
    for (k, &b) in href.as_bytes().iter().enumerate() {
        core::ptr::write_volatile(dst.add(start + k), b);
    }
    LINK_FROM[LINK_N + 1] = (start + href.len()) as u32;
    LINK_N += 1;
    LINK_N as u8
}

unsafe fn link_str(id: u8) -> &'static str {
    if id == 0 || id as usize > LINK_N {
        return "";
    }
    let i = id as usize - 1;
    let from = LINK_FROM[i] as usize;
    let to = LINK_FROM[i + 1] as usize;
    let body = core::ptr::addr_of!(LINK_TEXT) as *const u8;
    core::str::from_utf8(core::slice::from_raw_parts(body.add(from), to - from)).unwrap_or("")
}

/// The link under a screen row and column, if there is one.
///
/// A screen row is not a line index: a picture takes as many rows as it is
/// tall, so the only way to answer this is to walk the lines the way the
/// renderer walks them.  Two loops that have to agree are one loop too many,
/// but two loops over the same data in the same order is what this is.
fn link_under(row: usize, col: usize) -> Option<&'static str> {
    unsafe {
        let first = TOP;
        let mut at_row = 0usize;
        for idx in first..LINE_COUNT {
            let rows = if LINE_IMG[idx] != 0 { LINE_ROWS[idx] as usize } else { 1 };
            if row < at_row + rows {
                if LINE_IMG[idx] != 0 {
                    return None;                    // a picture is not a link
                }
                let off = LINE_AT[idx] as usize;
                let len = LINE_LEN[idx] as usize;
                if col >= len {
                    return None;
                }
                let id = LINK_AT_BYTE[off + col];
                if id == 0 {
                    return None;
                }
                return Some(link_str(id));
            }
            at_row += rows;
            if at_row > row {
                break;
            }
        }
        None
    }
}

// ---------------------------------------------------------------------------
//  Pictures
// ---------------------------------------------------------------------------

/// Where the pixels for an `<img src>` are, asked by the layout engine.
struct Pictures;

impl web::layout::ImageSource for Pictures {
    /// The slot holding this picture, and the size to draw it at in character
    /// cells.  `None` means the layout should fall back to the `alt` text,
    /// which is what happens for a picture that never arrived, one this
    /// browser cannot decode, and one the arena had no room for.
    fn resolve(&self, src: &str, avail_cols: usize) -> Option<(usize, usize, usize)> {
        let slot = unsafe { find_image(src)? };
        let (w, h) = unsafe { (IMG_W[slot] as usize, IMG_H[slot] as usize) };
        if w == 0 || h == 0 {
            return None;
        }
        // Never wider than the column and never taller than the window: a
        // picture that runs off the bottom is a picture nobody can see the
        // end of, and one wider than the page cannot be scrolled to.
        let max_cols = avail_cols.max(4);
        let max_rows = ROWS.saturating_sub(2).max(4);
        let mut cols = (w + CW as usize - 1) / CW as usize;
        let mut rows = (h + CH as usize - 1) / CH as usize;
        if cols > max_cols {
            rows = (rows * max_cols / cols).max(1);
            cols = max_cols;
        }
        if rows > max_rows {
            cols = (cols * max_rows / rows).max(1);
            rows = max_rows;
        }
        Some((slot, cols.max(1), rows.max(1)))
    }
}

/// Which picture has this source, as the markup wrote it.
unsafe fn find_image(src: &str) -> Option<usize> {
    let body = core::ptr::addr_of!(IMG_SRC) as *const u8;
    for i in 0..IMG_N {
        let from = IMG_SRC_AT[i] as usize;
        let to = IMG_SRC_AT[i + 1] as usize;
        let s = core::slice::from_raw_parts(body.add(from), to - from);
        if s == src.as_bytes() {
            return Some(i);
        }
    }
    None
}

/// Take a decoded picture into the arena and return its slot.
unsafe fn keep_image(src: &str, im: &web::img::Image) -> Option<usize> {
    if IMG_N >= IMG_MAX || im.w == 0 || im.h == 0 {
        return None;
    }
    let start = IMG_AT[IMG_N] as usize;
    let need = im.px.len();
    if start + need > IMG_BYTES {
        return None;
    }
    let dst = core::ptr::addr_of_mut!(IMG_PIX) as *mut u8;
    for (k, &b) in im.px.iter().enumerate() {
        core::ptr::write_volatile(dst.add(start + k), b);
    }
    IMG_AT[IMG_N + 1] = (start + need) as u32;
    IMG_W[IMG_N] = im.w as u32;
    IMG_H[IMG_N] = im.h as u32;

    let soff = IMG_SRC_AT[IMG_N] as usize;
    let n = src.len().min(IMG_SRC_BYTES.saturating_sub(soff));
    let sdst = core::ptr::addr_of_mut!(IMG_SRC) as *mut u8;
    for (k, &b) in src.as_bytes().iter().enumerate().take(n) {
        core::ptr::write_volatile(sdst.add(soff + k), b);
    }
    IMG_SRC_AT[IMG_N + 1] = (soff + n) as u32;
    IMG_N += 1;
    Some(IMG_N - 1)
}

fn forget_images() {
    unsafe {
        IMG_N = 0;
        IMG_AT[0] = 0;
        IMG_SRC_AT[0] = 0;
    }
}

// ---------------------------------------------------------------------------
//  Loading
// ---------------------------------------------------------------------------

/// Where a page load has got to.  The fetch itself is a state machine in
/// `net::http`; this is the one above it, which is a browser's actual loading
/// algorithm and the reason a page appears once rather than three times.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    /// Nothing in flight, or nothing left to do when the fetch lands.
    Idle,
    /// The document itself.
    Document,
    /// The stylesheets and scripts the markup named.
    Resources,
    /// The pictures, after the scripts have had their chance to add some.
    Pictures,
}

static mut STAGE: Stage = Stage::Idle;

/// What a subresource is, which decides where its body goes.
const K_CSS: u8 = 0;
const K_SCRIPT: u8 = 1;
const K_IMAGE: u8 = 2;

const MAX_CSS: usize = 8;
const MAX_JS: usize = 16;
/// Everything queued for one page, of every kind.  A page that names more
/// scripts than this is a page whose scripts would not run here anyway, and
/// fetching them one at a time would take longer than anyone would wait.
const QUEUE_MAX: usize = MAX_CSS + MAX_JS + IMG_MAX;
/// A ceiling on what all of a page's subresources may take up between them.
/// Without it a page can name sixteen scripts of a quarter of a megabyte each
/// and leave the document no room to be parsed in.
const SUB_TOTAL: usize = 1024 * 1024;

/// A subresource larger than this is not one: a stylesheet is tens of
/// kilobytes and a script is often more, but not megabytes.
const SUB_CAP: usize = 192 * 1024;

struct Queue {
    urls: [Option<String>; QUEUE_MAX],
    kind: [u8; QUEUE_MAX],
    /// Which `sheets` or `scripts` entry this fetch fills.  Decided when the
    /// fetch is queued, because the queue interleaves the two and they arrive
    /// in the order they were asked for.
    slot: [u8; QUEUE_MAX],
    count: usize,
    /// The next one to fetch; `count` when there are no more.
    at: usize,
    /// Set while a fetch is in flight.
    active: bool,
    /// Bytes of subresource bodies kept so far.
    bytes: usize,
    sheets: [String; MAX_CSS],
    sheet_count: usize,
    scripts: [String; MAX_JS],
    script_count: usize,
}

static mut Q: Queue = Queue {
    urls: [const { None }; QUEUE_MAX],
    kind: [0; QUEUE_MAX],
    slot: [0; QUEUE_MAX],
    count: 0,
    at: 0,
    active: false,
    bytes: 0,
    sheets: [const { String::new() }; MAX_CSS],
    sheet_count: 0,
    scripts: [const { String::new() }; MAX_JS],
    script_count: 0,
};

/// A navigation a script asked for, or a meta refresh, to be taken once the
/// layout is done.
static mut PENDING_NAV: Option<String> = None;
/// The parsed document, held across the subresource fetches because the layout
/// needs it at the end of them.
static mut PAGE_OBJ: Option<web::Page> = None;
/// What the document itself was, remembered when it arrived: by the time the
/// layout runs, the last fetch to land was a picture, and the HTTP client is
/// reporting on that one.
static mut DOC_LEN: usize = 0;
static mut DOC_TRUNCATED: bool = false;

/// Where the heap stood when this page load began.  Everything the load
/// allocates — the tree, the styles, the scripts, the layout — is one
/// allocation as far as the reclaiming is concerned.
static mut LOAD_MARK: usize = 0;

/// Turn a reference into something the HTTP client can fetch.
///
/// Three shapes, all of them common: a full URL, a root-relative path, and a
/// path relative to the directory the page is in.  The last is the one that
/// goes wrong quietly — `/assets/app.css` from `https://host/a/b/` is
/// `https://host/assets/app.css`, not `https://host/a/b/assets/app.css`.
pub fn resolve_url(base: &str, href: &str) -> Option<String> {
    let href = href.trim();
    if href.is_empty() || href.starts_with('#') || href.starts_with("data:")
        || href.starts_with("javascript:") || href.starts_with("mailto:")
    {
        return None;
    }
    if href.starts_with("http://") || href.starts_with("https://") {
        return Some(String::from(href));
    }
    let (scheme, rest) = match base.find("://") {
        Some(i) => (&base[..i], &base[i + 3..]),
        None => return None,
    };
    // A protocol-relative reference takes the page's scheme.
    if let Some(host_path) = href.strip_prefix("//") {
        return Some(alloc::format!("{}://{}", scheme, host_path));
    }
    let (host, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    if let Some(abs) = href.strip_prefix('/') {
        return Some(alloc::format!("{}://{}/{}", scheme, host, abs));
    }
    let dir = match path.rfind('/') {
        Some(i) => &path[..i + 1],
        None => "/",
    };
    Some(alloc::format!("{}://{}{}{}", scheme, host, dir, href))
}

/// Where the page is, for resolving against.  The address bar may hold a bare
/// host, which means https.
fn base_url() -> String {
    let raw = url_str();
    if raw.contains("://") {
        String::from(raw)
    } else {
        alloc::format!("https://{}/", raw.trim_end_matches('/'))
    }
}

/// Begin a page load: forget the last one and start the document fetch.
///
/// The two things that have to happen in this order and not the other are the
/// reset and the mark.  Everything the previous page allocated is freed here,
/// in one step, and the mark for this page is taken immediately afterwards —
/// so a load that fails still leaves the heap where it started rather than
/// where the failure left it.
unsafe fn start_load(url: &str) {
    // Drop the subresource bodies first.  Their allocations are inside the
    // span about to be freed, so a `String` left pointing into it would be a
    // use-after-free waiting for the next page that happened to read it.
    for s in Q.sheets.iter_mut() {
        *s = String::new();
    }
    for s in Q.scripts.iter_mut() {
        *s = String::new();
    }
    for u in Q.urls.iter_mut() {
        *u = None;
    }
    crate::mem::heap::reset_to(LOAD_MARK);
    LOAD_MARK = crate::mem::heap::mark();

    PAGE_LEN = 0;
    LINE_COUNT = 0;
    TOP = 0;
    CAPTURED = false;
    PENDING_NAV = None;
    PAGE_OBJ = None;
    STAGE = Stage::Document;
    Q.count = 0;
    Q.at = 0;
    Q.active = false;
    Q.bytes = 0;
    Q.sheet_count = 0;
    Q.script_count = 0;
    LINK_N = 0;
    LINK_FROM[0] = 0;
    forget_images();

    if !http::start(url) {
        STAGE = Stage::Idle;
        serial::print_str("[browser] request refused at start\n");
    }
}

/// Copy the current body into a string, bounded.
unsafe fn body_text(cap: usize) -> String {
    let n = http::body_len().min(cap);
    let mut out = String::with_capacity(n);
    let mut at = 0usize;
    while at < n {
        let mut chunk = [0u8; 512];
        let r = http::read_body(at, &mut chunk[..(n - at).min(512)]);
        if r == 0 {
            break;
        }
        out.push_str(&String::from_utf8_lossy(&chunk[..r]));
        at += r;
    }
    out
}

/// Add something to the fetch queue, unless it is already in it.
///
/// The deduplication is not an optimisation.  A page that names the same
/// stylesheet in a dozen places is ordinary, the queue is short, and the
/// alternative is a full queue spent on one file.
unsafe fn enqueue(href: &str, kind: u8, slot: u8) -> bool {
    let base = base_url();
    let Some(url) = resolve_url(&base, href) else { return false };
    for i in 0..Q.count {
        if Q.urls[i].as_deref() == Some(url.as_str()) {
            // Already on its way, with the slot the first request reserved.
            return false;
        }
    }
    if Q.count >= QUEUE_MAX {
        return false;
    }
    Q.urls[Q.count] = Some(url);
    Q.kind[Q.count] = kind;
    Q.slot[Q.count] = slot;
    Q.count += 1;
    true
}

/// Start fetching the next thing in the queue.  False when there is nothing
/// left.
unsafe fn start_next() -> bool {
    while Q.at < Q.count {
        let url = Q.urls[Q.at].clone();
        Q.at += 1;
        let Some(url) = url else { continue };
        serial::print_str("[browser] fetching ");
        serial::print_str(&url);
        serial::print_str("\n");
        Q.active = true;
        http::start(&url);
        return true;
    }
    Q.active = false;
    false
}

/// Step the load on after a fetch lands.
unsafe fn advance() {
    match STAGE {
        Stage::Idle => {}
        Stage::Document => {
            STAGE = Stage::Idle;
            capture();
        }
        Stage::Resources => {
            let i = Q.at.saturating_sub(1);
            let (kind, slot) = (Q.kind[i], Q.slot[i] as usize);
            sub_arrived(kind, slot);
        }
        Stage::Pictures => {
            picture_arrived();
        }
    }
}

/// The document has arrived: parse it, and work out what else it needs.
unsafe fn capture() {
    let n = http::body_len().min(PAGE_CAP);
    let base = core::ptr::addr_of_mut!(PAGE) as *mut u8;
    let mut got = 0usize;
    while got < n {
        let mut chunk = [0u8; 512];
        let r = http::read_body(got, &mut chunk[..(n - got).min(512)]);
        if r == 0 {
            break;
        }
        for (k, &b) in chunk[..r].iter().enumerate() {
            core::ptr::write_volatile(base.add(got + k), b);
        }
        got += r;
    }
    PAGE_LEN = got;
    DOC_LEN = http::body_len();
    DOC_TRUNCATED = http::body_truncated();

    let page = web::Page::parse(core::slice::from_raw_parts(base, got));
    Q.count = 0;
    Q.at = 0;
    Q.bytes = 0;
    for href in page.stylesheets.iter() {
        if Q.sheet_count >= MAX_CSS {
            break;
        }
        if enqueue(href, K_CSS, Q.sheet_count as u8) {
            Q.sheet_count += 1;
        }
    }
    for src in page.script_urls.iter() {
        if Q.script_count >= MAX_JS {
            break;
        }
        if enqueue(src, K_SCRIPT, Q.script_count as u8) {
            Q.script_count += 1;
        }
    }
    serial::print_str("[browser] ");
    serial::print_dec(got as u64);
    serial::print_str(" bytes of markup, ");
    serial::print_dec(Q.count as u64);
    serial::print_str(" stylesheet(s) and script(s)\n");

    *core::ptr::addr_of_mut!(PAGE_OBJ) = Some(page);
    STAGE = Stage::Resources;
    if !start_next() {
        after_resources();
    }
}

/// A stylesheet or script has arrived.
unsafe fn sub_arrived(kind: u8, slot: usize) {
    let text = body_text(SUB_CAP);
    // A stylesheet that 404s is the page's problem and not one this browser
    // can fix, so an empty body is simply not added.
    if !text.is_empty() && Q.bytes + text.len() <= SUB_TOTAL {
        Q.bytes += text.len();
        if kind == K_CSS && slot < MAX_CSS {
            Q.sheets[slot] = text;
        } else if kind == K_SCRIPT && slot < MAX_JS {
            Q.scripts[slot] = text;
        }
    }
    if !start_next() {
        after_resources();
    }
}

/// Everything the markup asked for has arrived: run the scripts, then find
/// out what pictures the page ended up with.
unsafe fn after_resources() {
    let slot = core::ptr::addr_of_mut!(PAGE_OBJ);
    let Some(page) = (*slot).as_mut() else {
        STAGE = Stage::Idle;
        return;
    };
    for i in 0..Q.sheet_count {
        if !Q.sheets[i].is_empty() {
            page.css.push(Q.sheets[i].clone());
        }
    }
    for i in 0..Q.script_count {
        if !Q.scripts[i].is_empty() {
            page.scripts.push(Q.scripts[i].clone());
        }
    }

    let url = base_url();
    let outcome = page.run_scripts(&url);
    if !outcome.log.is_empty() {
        serial::print_str("[browser] console: ");
        serial::print_str(&outcome.log);
        if !outcome.log.ends_with('\n') {
            serial::print_str("\n");
        }
    }
    if let Some(err) = &outcome.error {
        serial::print_str("[browser] script: ");
        serial::print_str(err);
        serial::print_str("\n");
    }
    PENDING_NAV = outcome.navigate.or_else(|| page.meta_refresh());

    // Now that the scripts have run, the document is what it is going to be —
    // which is the only moment at which asking what pictures it has gives the
    // right answer.
    Q.count = 0;
    Q.at = 0;
    for src in page.image_urls().iter() {
        if Q.count >= IMG_MAX {
            break;
        }
        // Pictures have no slot of their own: they go into the pixel arena,
        // which is packed in the order they arrive.
        enqueue(src, K_IMAGE, 0);
    }
    serial::print_str("[browser] ");
    serial::print_dec(Q.count as u64);
    serial::print_str(" picture(s)\n");

    STAGE = Stage::Pictures;
    if !start_next() {
        layout_now();
    }
}

/// A picture has arrived: decode it, keep it if it fits, next.
unsafe fn picture_arrived() {
    let src = Q.urls[Q.at.saturating_sub(1)].clone().unwrap_or_default();
    let body = body_bytes(SUB_CAP);
    match web::img::decode(&body, web::img::IMAGE_BUDGET, PAPER) {
        Ok(im) => {
            // The slot is looked up by the *attribute* the markup wrote, not
            // by the resolved URL the fetch used, because that is the string
            // the layout engine will ask about.
            let attr = attribute_for(&src);
            if keep_image(&attr, &im).is_none() {
                serial::print_str("[browser] no room for ");
                serial::print_str(&src);
                serial::print_str(" (");
                serial::print_dec(im.bytes() as u64);
                serial::print_str(" bytes)\n");
            }
        }
        Err(e) => {
            serial::print_str("[browser] cannot draw ");
            serial::print_str(&src);
            serial::print_str(": ");
            serial::print_str(e);
            serial::print_str("\n");
        }
    }
    if !start_next() {
        layout_now();
    }
}

/// The attribute text that resolved to this URL, for the layout to be asked
/// about.  The queue kept the resolved form; the document has the original.
unsafe fn attribute_for(resolved: &str) -> String {
    if let Some(page) = (*core::ptr::addr_of!(PAGE_OBJ)).as_ref() {
        let d = page.dom.borrow();
        let base = base_url();
        for &id in &d.by_tag("img") {
            if let Some(src) = d.attr(id, "src") {
                if resolve_url(&base, src).as_deref() == Some(resolved) {
                    return String::from(src);
                }
            }
        }
    }
    String::from(resolved)
}

/// Copy the current body out, bounded, as bytes rather than text.
unsafe fn body_bytes(cap: usize) -> alloc::vec::Vec<u8> {
    let n = http::body_len().min(cap);
    let mut out = alloc::vec::Vec::with_capacity(n);
    let mut at = 0usize;
    while at < n {
        let mut chunk = [0u8; 512];
        let r = http::read_body(at, &mut chunk[..(n - at).min(512)]);
        if r == 0 {
            break;
        }
        out.extend_from_slice(&chunk[..r]);
        at += r;
    }
    out
}

/// Everything is in: lay the page out, draw it out of the heap, and go
/// wherever the page asked to go.
unsafe fn layout_now() {
    STAGE = Stage::Idle;
    let slot = core::ptr::addr_of_mut!(PAGE_OBJ);
    let Some(page) = (*slot).take() else { return };

    let url = base_url();
    let lines = page.layout_with(COLS, &Pictures);
    copy_lines(&lines);

    // Anything the page asked for itself, with a script or a meta tag rather
    // than with a header.  baidu.com's https page is exactly this: a script
    // that rewrites the scheme, and a meta refresh saying the same thing to
    // anyone whose scripts did not run.  It is a redirect, so it replaces the
    // current history entry rather than adding one.
    let nav = PENDING_NAV.take();

    serial::print_str("[browser] laid out ");
    serial::print_dec(LINE_COUNT as u64);
    serial::print_str(" lines, ");
    serial::print_dec(IMG_N as u64);
    serial::print_str(" picture(s)\n");

    // The whole page is about to become unreachable, in one step.  Two things
    // outlive the block above and both are heap allocations held in statics:
    // the interpreter's prototypes and its DOM host.  Anything else still
    // reachable from here would read as a wild pointer rather than as a
    // use-after-free, which is not a thing worth debugging twice.
    web::js::value::forget_prototypes();
    web::js::domjs::forget();
    for s in Q.sheets.iter_mut() {
        *s = String::new();
    }
    for s in Q.scripts.iter_mut() {
        *s = String::new();
    }
    for u in Q.urls.iter_mut() {
        *u = None;
    }
    crate::mem::heap::reset_to(LOAD_MARK);

    if let Some(target) = nav {
        if let Some(abs) = resolve_url(&url, &target) {
            if abs != url {
                serial::print_str("[browser] the page asked to go to ");
                serial::print_str(&abs);
                serial::print_str("\n");
                set_url(&abs);
                navigate(&abs, false);
            }
        }
    }
}

// ---------------------------------------------------------------------------
//  The boot self-test
// ---------------------------------------------------------------------------

/// Load a page the way a reader would, at boot, where the log can show it.
///
/// The network's own self-test proves the wire and the page renderer's proves
/// the four layers under this one.  Neither proves the thing that joins them:
/// the four-step load, the links it interns, the pictures it decodes into the
/// arena, and the lines that come out.  All of that is invisible until someone
/// clicks something, and the only place a click can be observed from is the
/// serial log.
///
/// Returns the number of checks that failed, so a caller can say so plainly
/// rather than leaving the reader to compare a log against a memory of what it
/// used to say.
pub fn selftest(url: &str) -> usize {
    serial::print_str("[browser] self-test: loading ");
    serial::print_str(url);
    serial::print_str("\n");

    let mut failures = 0usize;
    set_url(url);
    navigate(url, false);

    // The two calls the desktop makes on every pass of its input loop, in the
    // order it makes them: the network moves frames and the browser notices
    // that a fetch has landed.  Driving the load any other way would test a
    // different program from the one that runs.
    let start = crate::interrupts::TIMER_TICKS.load(Ordering::Relaxed);
    let mut guard = 0u64;
    while unsafe { STAGE } != Stage::Idle
        && crate::interrupts::TIMER_TICKS.load(Ordering::Relaxed).saturating_sub(start)
            < SELFTEST_TICKS
    {
        crate::net::poll();
        tick();
        guard += 1;
        if guard > 200_000_000 {
            break;                                  // a wedged pump, not a slow site
        }
    }
    if unsafe { STAGE } != Stage::Idle {
        serial::print_str("[browser]   FAIL the load did not finish\n");
        unsafe { STAGE = Stage::Idle };
        failures += 1;
    }

    let lines = unsafe { LINE_COUNT };
    let images = unsafe { IMG_N };
    let links = unsafe { LINK_N };
    serial::print_str("[browser]   ");
    serial::print_dec(lines as u64);
    serial::print_str(" lines, ");
    serial::print_dec(links as u64);
    serial::print_str(" distinct link(s), ");
    serial::print_dec(images as u64);
    serial::print_str(" picture(s) drawn\n");

    // The first few lines, because "17 lines" and a page that reads are not
    // the same claim and a log can only make one of them.
    for i in 0..lines.min(4) {
        let (off, len) = unsafe {
            (
                core::ptr::read_volatile((core::ptr::addr_of!(LINE_AT) as *const u32).add(i)) as usize,
                core::ptr::read_volatile((core::ptr::addr_of!(LINE_LEN) as *const u32).add(i)) as usize,
            )
        };
        if unsafe { core::ptr::read_volatile((core::ptr::addr_of!(LINE_IMG) as *const u16).add(i)) } != 0 {
            serial::print_str("[browser]   | (picture)\n");
            continue;
        }
        let base = core::ptr::addr_of!(TEXT) as *const u8;
        let mut text = [0u8; 96];
        let n = len.min(96);
        for (k, slot) in text.iter_mut().enumerate().take(n) {
            *slot = unsafe { core::ptr::read_volatile(base.add(off + k)) };
        }
        serial::print_str("[browser]   | ");
        serial::print_str(core::str::from_utf8(&text[..n]).unwrap_or(""));
        serial::print_str("\n");
    }

    if lines == 0 {
        serial::print_str("[browser]   FAIL no lines were laid out\n");
        failures += 1;
    } else {
        serial::print_str("[browser]   ok   the page became lines\n");
        // The last line as well as the first four.  A page that was cut off
        // says so on its last line, and that is the one claim about a page
        // that cannot be checked by looking at the start of it.
        unsafe { print_line(lines - 1) };
    }
    if unsafe { DOC_TRUNCATED } {
        serial::print_str("[browser]   --   the server sent more than this browser holds\n");
    }

    // A link that cannot be found by clicking is a link that is drawn and not
    // a link, and this is the only place that can be told apart.
    let mut found = None;
    for row in 0..ROWS {
        for col in 0..64 {
            if let Some(href) = link_under(row, col) {
                if !href.is_empty() {
                    found = Some((row, col, href));
                    break;
                }
            }
        }
        if found.is_some() {
            break;
        }
    }
    match found {
        Some((row, col, href)) => {
            serial::print_str("[browser]   ok   a link is reachable at row ");
            serial::print_dec(row as u64);
            serial::print_str(", column ");
            serial::print_dec(col as u64);
            serial::print_str(": ");
            serial::print_str(&href[..href.len().min(72)]);
            serial::print_str("\n");
        }
        None => {
            // Not a failure.  A page may genuinely carry no links, and a page
            // that is mostly pictures need not have one on the first screen.
            serial::print_str("[browser]   --   no link on the first screen\n");
        }
    }

    failures += selftest_picture(SELFTEST_PICTURE);
    failures += selftest_draw();

    // Back to where the desktop expects to find it: a browser with nothing
    // open and the address bar on the page it starts from.
    unsafe {
        LINE_COUNT = 0;
        TOP = 0;
        STAGE = Stage::Idle;
    }
    set_url(HOME);
    failures
}

/// One laid-out line, as the log shows it.
unsafe fn print_line(i: usize) {
    if i >= LINE_COUNT {
        return;
    }
    if core::ptr::read_volatile((core::ptr::addr_of!(LINE_IMG) as *const u16).add(i)) != 0 {
        serial::print_str("[browser]   | (picture)
");
        return;
    }
    let off = core::ptr::read_volatile((core::ptr::addr_of!(LINE_AT) as *const u32).add(i)) as usize;
    let len = core::ptr::read_volatile((core::ptr::addr_of!(LINE_LEN) as *const u32).add(i)) as usize;
    let base = core::ptr::addr_of!(TEXT) as *const u8;
    let mut text = [0u8; 110];
    let n = len.min(110);
    for (k, slot) in text.iter_mut().enumerate().take(n) {
        *slot = core::ptr::read_volatile(base.add(off + k));
    }
    serial::print_str("[browser]   | ");
    serial::print_str(core::str::from_utf8(&text[..n]).unwrap_or(""));
    serial::print_str("
");
}

/// The picture path on its own, over a real picture.
///
/// The page above has no raster image in it — its logo is a format this
/// browser does not read, which proves the *fallback* and not the feature.  A
/// page with no picture in it cannot tell a browser that draws pictures from
/// one that drops them on the floor, so this fetches one, decodes it, and lays
/// out a document whose whole content is that picture.  It is the smallest
/// thing that can tell those two apart, and it is a picture from one of the
/// three sites this browser exists to reach.
fn selftest_picture(url: &str) -> usize {
    serial::print_str("[browser] self-test: one picture, ");
    serial::print_str(url);
    serial::print_str("\n");

    let mut failures = 0usize;
    if !http::start(url) {
        serial::print_str("[browser]   FAIL could not start the fetch\n");
        return 1;
    }
    let start = crate::interrupts::TIMER_TICKS.load(Ordering::Relaxed);
    while http::in_progress()
        && crate::interrupts::TIMER_TICKS.load(Ordering::Relaxed).saturating_sub(start)
            < SELFTEST_TICKS
    {
        crate::net::poll();
    }
    if http::phase() != http::Phase::Done {
        serial::print_str("[browser]   FAIL the fetch did not finish: ");
        serial::print_str(http::error());
        serial::print_str("\n");
        return 1;
    }

    let body = unsafe { body_bytes(SUB_CAP) };
    let mark = crate::mem::heap::mark();
    let im = match web::img::decode(&body, web::img::IMAGE_BUDGET, PAPER) {
        Ok(im) => im,
        Err(e) => {
            serial::print_str("[browser]   FAIL ");
            serial::print_str(e);
            serial::print_str("\n");
            return 1;
        }
    };
    serial::print_str("[browser]   ok   decoded ");
    serial::print_dec(im.w as u64);
    serial::print_str("x");
    serial::print_dec(im.h as u64);

    let slot = match unsafe { keep_image(url, &im) } {
        Some(slot) => slot,
        None => {
            serial::print_str(" -- FAIL no room in the arena\n");
            return 1;
        }
    };
    serial::print_str(", in the arena as slot ");
    serial::print_dec(slot as u64);
    serial::print_str("\n");

    // And now the layout, over a document that is that picture and nothing
    // else.  A picture line is not a text line and only the layout knows the
    // difference, so this is where "it decoded" becomes "it will be drawn".
    let html = alloc::format!("<html><body><img src=\"{}\" alt=\"?\"></body></html>", url);
    let page = web::Page::parse(html.as_bytes());
    let lines = page.layout_with(COLS, &Pictures);
    match lines.iter().find_map(|l| l.image.as_ref()) {
        Some(box_) => {
            serial::print_str("[browser]   ok   laid out as a picture line, ");
            serial::print_dec(box_.cols as u64);
            serial::print_str(" by ");
            serial::print_dec(box_.rows as u64);
            serial::print_str(" cells\n");
            if box_.slot != slot {
                serial::print_str("[browser]   FAIL the line points at the wrong picture\n");
                failures += 1;
            }
        }
        None => {
            serial::print_str("[browser]   FAIL it laid out as text, not as a picture\n");
            failures += 1;
        }
    }

    // The document tree and the decoded pixels are both heap allocations; the
    // pixels were copied into the arena and the tree is not needed again.
    unsafe { crate::mem::heap::reset_to(mark) };
    failures
}

/// Draw the page once, into the window's own buffer.
///
/// Everything above proves the page is *right*; this proves the code that puts
/// it on screen runs.  Turning a line into glyphs and a decoded picture into
/// pixels is the part of a browser where a mistake is a wild write rather than
/// a wrong word — an off-by-one in the sampling accumulator writes past the
/// end of the frame buffer and takes the machine with it — and it is the one
/// part nothing else here exercises, because the desktop comes up with nothing
/// open and a boot has no mouse.
///
/// The window is closed again immediately, so the desktop is exactly as it
/// would have been: nothing opened, which is what it is supposed to do.
///
/// There is no way for this to fail softly.  A fault is a CPU exception and a
/// stopped machine, which a boot log says plainly; getting to the end is the
/// whole of the evidence.
fn selftest_draw() -> usize {
    let id = window::create(WIN_X, WIN_Y, WIN_W, WIN_H, "Browser");
    let (wx, wy) = window::position(id).unwrap_or((WIN_X, WIN_Y));
    draw_toolbar(wx, wy);
    draw_page(wx, wy);
    draw_status(wx, wy);
    if let Some(i) = window::index_of(id) {
        window::close(i);
    }
    serial::print_str("[browser]   ok   the page was drawn and the window closed again\n");
    0
}

/// How long the boot page load may take.  Five fetches to a site on another
/// continent, at a second or two each, with room to spare for one that is
/// slow rather than absent.
const SELFTEST_TICKS: u64 = 4000;

/// Was the page cut short, and by how much?
///
/// Two things can do it and they mean different things.  The socket's receive
/// buffer filling up means the network delivered more than this browser was
/// willing to hold; the document buffer filling up means it arrived and was
/// then dropped on the floor.  Both read as a page that just ends, so both
/// have to be said out loud.
fn truncation_notice() -> Option<alloc::string::String> {
    let (truncated, len) = unsafe { (DOC_TRUNCATED, DOC_LEN) };
    if truncated {
        return Some(alloc::format!(
            "[the page was cut off: it was longer than the {} KiB this browser will hold]",
            PAGE_CAP / 1024
        ));
    }
    if len > PAGE_CAP {
        return Some(alloc::format!(
            "[the page was cut off at {} KiB]",
            PAGE_CAP / 1024
        ));
    }
    None
}

// ---------------------------------------------------------------------------
//  Markup to lines
// ---------------------------------------------------------------------------

/// The record for a style, made once and shared by every byte that has it.
fn intern(style: &web::css::Style) -> u8 {
    let mut rec = [style.color.0, style.color.1, style.color.2, 0u8];
    // A link is coloured when the page has not said otherwise.  Black is what
    // a style starts as, so "the page said otherwise" is exactly "this is not
    // black" — and a page that leaves its links the default colour gets links
    // that look like links.
    if style.link && style.color == web::css::Rgb::BLACK {
        rec[0] = 0x1A;
        rec[1] = 0x5F;
        rec[2] = 0xC8;
    }
    if style.bold {
        rec[3] |= ATTR_BOLD;
    }
    if style.italic {
        rec[3] |= ATTR_ITALIC;
    }
    if style.underline || style.link {
        rec[3] |= ATTR_UNDERLINE;
    }
    if style.strike {
        rec[3] |= ATTR_STRIKE;
    }
    unsafe {
        let tab = core::ptr::addr_of!(STYLE_TAB) as *const [u8; 4];
        for i in 0..STYLE_N {
            let e = core::ptr::read_volatile(tab.add(i));
            if e == rec {
                return i as u8;
            }
        }
        if STYLE_N >= MAX_STYLES {
            return 0;
        }
        let out = core::ptr::addr_of_mut!(STYLE_TAB) as *mut [u8; 4];
        core::ptr::write_volatile(out.add(STYLE_N), rec);
        STYLE_N += 1;
        (STYLE_N - 1) as u8
    }
}

/// Room kept back at the end of the text buffer for the truncation notice.
///
/// The notice is appended after the page has been copied, and the page it
/// most needs to appear on is the one that filled the buffer — so the buffer
/// has to be full *before* the last few hundred bytes, or the one case that
/// matters is the one case that cannot be reported.
const NOTICE_RESERVE: usize = 192;

/// Copy laid-out lines into the flat buffers the renderer draws from.
///
/// One byte of text is one character cell, which is what makes the hit test
/// exact rather than approximate: a tab is expanded here, where its width is
/// known, so that column *n* of a line is byte *n* of its text and a click can
/// name the run it landed on.
unsafe fn copy_lines(lines: &[web::layout::Line]) {
    let out = core::ptr::addr_of_mut!(TEXT) as *mut u8;
    let at = core::ptr::addr_of_mut!(LINE_AT) as *mut u32;
    let ln = core::ptr::addr_of_mut!(LINE_LEN) as *mut u32;
    let st = core::ptr::addr_of_mut!(STYLE_AT) as *mut u8;
    let lk = core::ptr::addr_of_mut!(LINK_AT_BYTE) as *mut u8;
    let im = core::ptr::addr_of_mut!(LINE_IMG) as *mut u16;
    let lc = core::ptr::addr_of_mut!(LINE_COLS) as *mut u16;
    let lr = core::ptr::addr_of_mut!(LINE_ROWS) as *mut u16;
    STYLE_N = 0;
    LINK_N = 0;
    LINK_FROM[0] = 0;
    intern(&web::css::Style::initial());

    // One line and one notice's worth of room held back.
    let room = PAGE_CAP.saturating_sub(NOTICE_RESERVE);

    let mut n = 0usize;
    let mut count = 0usize;
    for line in lines {
        if count + 1 >= MAX_LINES || n >= room {
            break;
        }
        let start = n;

        if let Some(box_) = &line.image {
            core::ptr::write_volatile(im.add(count), (box_.slot + 1) as u16);
            core::ptr::write_volatile(lc.add(count), box_.cols as u16);
            core::ptr::write_volatile(lr.add(count), box_.rows as u16);
        } else {
            core::ptr::write_volatile(im.add(count), 0);
        }

        // Indentation is part of the line, at the front, where the renderer
        // would otherwise have to remember to skip it.
        for _ in 0..line.indent {
            if n >= room {
                break;
            }
            core::ptr::write_volatile(out.add(n), b' ');
            core::ptr::write_volatile(st.add(n), intern(&web::css::Style::initial()));
            core::ptr::write_volatile(lk.add(n), 0);
            n += 1;
        }

        for run in &line.runs {
            let code = intern(&run.style);
            let id = match &run.link {
                Some(href) => intern_link(href),
                None => 0,
            };
            for b in run.text.bytes() {
                if n >= room {
                    break;
                }
                // A tab is four cells everywhere else in this file; it is
                // four cells here too, or the columns stop matching.
                let cells = if b == b'\t' { 4 } else { 1 };
                for _ in 0..cells {
                    if n >= room {
                        break;
                    }
                    core::ptr::write_volatile(out.add(n), if b == b'\t' { b' ' } else { b });
                    core::ptr::write_volatile(st.add(n), code);
                    core::ptr::write_volatile(lk.add(n), id);
                    n += 1;
                }
            }
        }
        // A blank line is still a line: the blank ones are what separate a
        // heading from what follows it.
        core::ptr::write_volatile(at.add(count), start as u32);
        core::ptr::write_volatile(ln.add(count), (n - start) as u32);
        count += 1;
    }

    // And, last, the one thing a text browser must never do quietly: show a
    // truncated page as though it were the whole page.
    if let Some(note) = truncation_notice() {
        if count < MAX_LINES && n + note.len() < PAGE_CAP {
            let start = n;
            let bad = intern(&web::css::Style::initial());
            for b in note.bytes() {
                core::ptr::write_volatile(out.add(n), b);
                core::ptr::write_volatile(st.add(n), bad);
                core::ptr::write_volatile(lk.add(n), 0);
                n += 1;
            }
            core::ptr::write_volatile(im.add(count), 0);
            core::ptr::write_volatile(at.add(count), start as u32);
            core::ptr::write_volatile(ln.add(count), (n - start) as u32);
            count += 1;
        }
    }

    LINE_COUNT = count;
    TOP = 0;
}

// ---------------------------------------------------------------------------
//  Drawing
// ---------------------------------------------------------------------------

pub fn render() {
    let id = window_id();
    let (wx, wy) = window::position(id).unwrap_or((WIN_X, WIN_Y));

    draw_toolbar(wx, wy);
    draw_page(wx, wy);
    draw_status(wx, wy);
}

/// Top of the address field, in window coordinates.
fn toolbar_y() -> u32 {
    TITLE_BAR_H + 4
}

fn draw_toolbar(wx: u32, wy: u32) {
    let y = wy + toolbar_y();

    // Two buttons rather than a menu: back is the one control a browser
    // cannot do without and the one thing here that a keyboard has no key
    // for, because this machine's keyboard has no modifiers to spare.
    let (at, n) = unsafe { (HIST_AT, HIST_N) };
    draw_button(wx + TOOLBAR_X, y, '<', n > 0 && at > 0);
    draw_button(wx + TOOLBAR_X + BUTTON_W, y, '>', at + 1 < n);

    let field_x = wx + TOOLBAR_X + 2 * BUTTON_W;
    let field_w = WIN_W - 2 * TOOLBAR_X - 2 * BUTTON_W;
    draw_rect(field_x, y, field_w, 20, (0x1E, 0x28, 0x3C));

    let raw = url_bytes();
    let cols = ((field_w - 2 * FIELD_PAD) / CW) as usize;
    // Scroll the text so the caret is always visible: a URL longer than the
    // field used to be drawn off the right edge with the caret gone with it.
    let caret = unsafe { CARET };
    let first = caret.saturating_sub(cols.saturating_sub(1));
    let mut x = field_x + FIELD_PAD;
    let ty = y + 2;
    for i in first..raw.len().min(first + cols) {
        let here = i == caret;
        let pal = if here { (0x10, 0xB9, 0x81) } else { (0xEA, 0xEA, 0xEA) };
        font::draw_char(raw[i], x, ty, pal.0, pal.1, pal.2);
        x += CW;
    }
    if caret >= raw.len() && (caret - first) < cols {
        draw_rect(x, ty + 1, 2, 14, (0x10, 0xB9, 0x81));
    }
}

fn draw_button(x: u32, y: u32, glyph: char, live: bool) {
    let (r, g, b) = if live { (0x2C, 0x3A, 0x58) } else { (0x22, 0x26, 0x33) };
    draw_rect(x, y, BUTTON_W - 2, 20, (r, g, b));
    let (fr, fg, fb) = if live { (0xEA, 0xEA, 0xEA) } else { (0x55, 0x5A, 0x66) };
    font::draw_char(glyph as u8, x + 6, y + 2, fr, fg, fb);
}

fn draw_page(wx: u32, wy: u32) {
    let top_y = wy + TITLE_BAR_H + TOOLBAR_H + 4;
    // The paper, so that a page with no background of its own is drawn on
    // what its stylesheet assumes, and so that a transparent pixel has
    // something to be transparent against.
    draw_rect(wx + 2, top_y - 2, WIN_W - 4, WIN_H - TITLE_BAR_H - TOOLBAR_H - STATUS_H,
              PAPER);

    let count = unsafe { LINE_COUNT };
    if count == 0 {
        let msg = match http::phase() {
            http::Phase::Idle => "Type a host and press Enter.",
            http::Phase::Done => "The page had no text.",
            http::Phase::Failed => http::error(),
            _ => "Loading...",
        };
        font::draw_str(msg, wx + 10, top_y + 2, 0x44, 0x44, 0x4C);
        return;
    }

    let mut row = 0usize;
    let first = unsafe { TOP };
    for idx in first..count {
        if row >= ROWS {
            break;
        }
        let y = top_y + (row as u32) * CH;
        let slot = unsafe { core::ptr::read_volatile(
            (core::ptr::addr_of!(LINE_IMG) as *const u16).add(idx)) };
        if slot != 0 {
            let cols = unsafe { core::ptr::read_volatile(
                (core::ptr::addr_of!(LINE_COLS) as *const u16).add(idx)) } as usize;
            let rows = unsafe { core::ptr::read_volatile(
                (core::ptr::addr_of!(LINE_ROWS) as *const u16).add(idx)) } as usize;
            draw_image(slot as usize - 1, wx + 10, y, cols as u32 * CW, rows as u32 * CH);
            row += rows;
        } else {
            draw_text_line(idx, wx + 10, y);
            row += 1;
        }
    }
}

fn draw_text_line(idx: usize, x0: u32, y: u32) {
    let count = unsafe { LINE_COUNT };
    if idx >= count {
        return;
    }
    let at = core::ptr::addr_of!(LINE_AT) as *const u32;
    let ln = core::ptr::addr_of!(LINE_LEN) as *const u32;
    let text = core::ptr::addr_of!(TEXT) as *const u8;
    let sty = core::ptr::addr_of!(STYLE_AT) as *const u8;
    let tab = core::ptr::addr_of!(STYLE_TAB) as *const [u8; 4];

    let off = unsafe { core::ptr::read_volatile(at.add(idx)) } as usize;
    let len = unsafe { core::ptr::read_volatile(ln.add(idx)) } as usize;
    let mut x = x0;
    let mut k = 0usize;
    while k < len {
        if x + CW > x0 + WIN_W - 12 {
            break;
        }
        let code = unsafe { core::ptr::read_volatile(sty.add(off + k)) } as usize;
        // A run: every following byte with the same style, drawn with the
        // same colour and drawn once.
        let mut run_len = 1usize;
        while k + run_len < len
            && unsafe { core::ptr::read_volatile(sty.add(off + k + run_len)) } as usize == code
        {
            run_len += 1;
        }
        let rec = if code < MAX_STYLES {
            unsafe { core::ptr::read_volatile(tab.add(code)) }
        } else {
            [0xE8, 0xE8, 0xE8, 0]
        };
        for j in 0..run_len {
            let b = unsafe { core::ptr::read_volatile(text.add(off + k + j)) };
            // No bold face and no italic face: the font is one bitmap, and
            // eight by sixteen pixels has no room to fake either.  Bold is a
            // second pass one pixel across, which is what a dot-matrix printer
            // did and reads as bold at this size.
            font::draw_char(b, x, y, rec[0], rec[1], rec[2]);
            if rec[3] & ATTR_BOLD != 0 && x + 1 < x0 + WIN_W - 12 {
                font::draw_char(b, x + 1, y, rec[0], rec[1], rec[2]);
            }
            if rec[3] & ATTR_UNDERLINE != 0 {
                draw_rect(x, y + CH - 2, CW, 1, (rec[0], rec[1], rec[2]));
            }
            if rec[3] & ATTR_STRIKE != 0 {
                draw_rect(x, y + CH / 2, CW, 1, (rec[0], rec[1], rec[2]));
            }
            x += CW;
            if x + CW > x0 + WIN_W - 12 {
                break;
            }
        }
        k += run_len;
    }
}

/// A picture, scaled to the cells the layout gave it.
///
/// Nearest neighbour, and the source position is carried in an accumulator
/// rather than recomputed: a seven-hundred-pixel-wide picture is a quarter of
/// a million divisions otherwise, all of them while the reader waits.
fn draw_image(slot: usize, x0: u32, y0: u32, w: u32, h: u32) {
    let n = unsafe { IMG_N };
    if slot >= n || w == 0 || h == 0 {
        return;
    }
    let (sw, sh) = unsafe { (IMG_W[slot] as usize, IMG_H[slot] as usize) };
    if sw == 0 || sh == 0 {
        return;
    }
    let base = core::ptr::addr_of!(IMG_PIX) as *const u8;
    let start = unsafe { IMG_AT[slot] as usize };
    let max_x = WIN_W - 12;

    let mut sy = 0usize;
    let mut y_acc = 0u32;
    for dy in 0..h {
        let row = start + sy * sw * 3;
        let mut sx = 0usize;
        let mut x_acc = 0u32;
        for dx in 0..w {
            if dx >= max_x {
                break;
            }
            let p = row + sx * 3;
            let (r, g, b) = unsafe {
                (
                    core::ptr::read_volatile(base.add(p)),
                    core::ptr::read_volatile(base.add(p + 1)),
                    core::ptr::read_volatile(base.add(p + 2)),
                )
            };
            crate::dev::framebuffer::put_pixel(x0 + dx, y0 + dy, r, g, b);
            x_acc += sw as u32;
            while x_acc >= w && sx + 1 < sw {
                x_acc -= w;
                sx += 1;
            }
        }
        y_acc += sh as u32;
        while y_acc >= h && sy + 1 < sh {
            y_acc -= h;
            sy += 1;
        }
    }
}

fn draw_status(wx: u32, wy: u32) {
    let y = wy + WIN_H - 18;
    draw_rect(wx + 4, y - 3, WIN_W - 8, 18, (0x16, 0x16, 0x1E));

    let mut x = wx + 10;
    let (colour, text) = if http::phase() == http::Phase::Failed {
        ((0xFF, 0x88, 0x88), http::error())
    } else {
        ((0x9A, 0xC8, 0xFF), http::phase_name())
    };
    font::draw_str(text, x, y, colour.0, colour.1, colour.2);
    x += (text.len() as u32) * CW + 16;

    let mut buf = [0u8; 24];
    if http::phase() == http::Phase::Done {
        let n = fmt_u64(http::status_code() as u64, &mut buf);
        font::draw_str(core::str::from_utf8(&buf[..n]).unwrap_or(""), x, y, 0xAA, 0xCC, 0xAA);
        x += (n as u32) * CW + 12;
        let n = fmt_u64(http::body_len() as u64, &mut buf);
        font::draw_str(core::str::from_utf8(&buf[..n]).unwrap_or(""), x, y, 0xAA, 0xCC, 0xAA);
        x += (n as u32) * CW + 6;
        font::draw_str("bytes", x, y, 0x88, 0xAA, 0x88);
        x += 6 * CW + 12;
        if unsafe { DOC_TRUNCATED } {
            font::draw_str("cut off", x, y, 0xFF, 0xAA, 0x55);
            x += 8 * CW;
        }
    }
    let images = unsafe { IMG_N };
    if images > 0 {
        let n = fmt_u64(images as u64, &mut buf);
        font::draw_str(core::str::from_utf8(&buf[..n]).unwrap_or(""), x, y, 0x88, 0x99, 0xBB);
        x += (n as u32) * CW + 6;
        font::draw_str("img", x, y, 0x77, 0x88, 0xAA);
    }

    // Scroll position, when there is anything to scroll.
    let count = unsafe { LINE_COUNT };
    if count > ROWS {
        let mut b = [0u8; 24];
        let n = fmt_u64(unsafe { TOP } as u64 + 1, &mut b);
        let sx = wx + WIN_W - 150;
        font::draw_str(core::str::from_utf8(&b[..n]).unwrap_or(""), sx, y, 0x88, 0x88, 0x99);
        font::draw_str("/", sx + (n as u32) * CW, y, 0x88, 0x88, 0x99);
        let n2 = fmt_u64(count as u64, &mut b);
        font::draw_str(core::str::from_utf8(&b[..n2]).unwrap_or(""),
                       sx + ((n + 1) as u32) * CW, y, 0x88, 0x88, 0x99);
    }
}

fn draw_rect(x: u32, y: u32, w: u32, h: u32, rgb: (u8, u8, u8)) {
    crate::dev::framebuffer::fill_rect(x, y, w, h, rgb.0, rgb.1, rgb.2);
}

fn fmt_u64(mut v: u64, buf: &mut [u8; 24]) -> usize {
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
    let n = buf.len() - i;
    buf.copy_within(i.., 0);
    n
}

/// Called once per input-loop pass.
///
/// The fetch itself is advanced by `net::tick`; all this does is notice when
/// the phase has changed, step the load on if a fetch has landed, and repaint
/// — so a page appears as soon as it lands without the compositor redrawing
/// the browser sixty times a second.
pub fn tick() -> bool {
    let phase = http::phase() as u8;
    let body = http::body_len();
    let changed = unsafe { phase != LAST_PHASE || body != LAST_BODY };
    if !changed {
        return false;
    }
    unsafe {
        LAST_PHASE = phase;
        LAST_BODY = body;
    }

    if http::phase() == http::Phase::Done && unsafe { STAGE } != Stage::Idle {
        unsafe { advance() };
    }
    // The load advances whether or not there is a window to draw it in: a
    // page started before the window was closed still finishes and is there
    // when it is opened again, and the boot self-test has no window at all.
    // What the window decides is whether the desktop needs repainting.
    window::exists(window_id())
}
