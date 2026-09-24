//! Browser — fetches an HTTP page and displays it as text.
//!
//! What this is: a window with an address bar, a scrollable view of a page's
//! text, and a status line that says what the network is doing.  What it is
//! not: an HTML renderer.  Tags are used for one thing — deciding where the
//! line breaks go — and everything else about them is discarded, so a page
//! comes out as its prose with the pictures and the styling missing.  There is
//! no CSS, no scripting, no images, and no links to click yet.
//!
//! The fetch is not run here.  `net::http` owns it and advances it from
//! `net::tick`, which the input loop calls; this module starts a request and
//! then paints whatever state that request is in.  That is why the window
//! keeps redrawing, and why the pointer keeps moving, while a page loads.

use crate::dev::keyboard::{
    KEY_BACKSPACE, KEY_DOWN, KEY_ENTER, KEY_ESC, KEY_LEFT, KEY_RIGHT, KEY_TAB, KEY_UP,
};
use crate::net::http;
use crate::serial;
use crate::wm::{font, window};

static mut BR_WIN_ID: u64 = 0;

const WIN_X: u32 = 56;
const WIN_Y: u32 = 40;
const WIN_W: u32 = 720;
const WIN_H: u32 = 440;

/// Glyph cell.
const CW: u32 = 8;
const CH: u32 = 16;

/// Above the page: the address bar.
const TOOLBAR_H: u32 = 26;
/// Below it: where the fetch got to.
const STATUS_H: u32 = 22;

/// Text column limit, from the window width less a small margin.
const COLS: usize = ((WIN_W - 20) / CW) as usize;
/// Rows of page text visible at once.
const ROWS: usize = ((WIN_H - TOOLBAR_H - STATUS_H - 8) / CH) as usize;

const PAGE_CAP: usize = 32 * 1024;
const MAX_LINES: usize = 2048;
const MAX_URL: usize = 96;

/// The page as bytes, straight out of the HTTP body.
static mut PAGE: [u8; PAGE_CAP] = [0; PAGE_CAP];
static mut PAGE_LEN: usize = 0;

/// The same bytes with the markup resolved into line breaks, and the lines
/// as (offset, length) pairs.  Rendering happens once, when the fetch
/// finishes; drawing then just walks this table.
static mut TEXT: [u8; PAGE_CAP] = [0; PAGE_CAP];
static mut LINE_AT: [u32; MAX_LINES] = [0; MAX_LINES];
static mut LINE_LEN: [u32; MAX_LINES] = [0; MAX_LINES];
static mut LINE_COUNT: usize = 0;
/// Which line the view is scrolled to.
static mut TOP: usize = 0;

/// Address bar contents.
static mut URL: [u8; MAX_URL] = [0; MAX_URL];
static mut URL_LEN: usize = 0;
static mut CARET: usize = 0;

/// Where typing goes.  Tab switches.
#[derive(Clone, Copy, PartialEq)]
enum Focus {
    Address,
    Page,
}
static mut FOCUS: Focus = Focus::Address;

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

pub fn window_id() -> u64 {
    unsafe { BR_WIN_ID }
}

pub fn init() {
    let id = window::create(WIN_X, WIN_Y, WIN_W, WIN_H, "Browser");
    unsafe { BR_WIN_ID = id; }
    set_url(HOME);
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
        go();
    }
}

pub fn open() {
    open_with("");
}

fn set_url(s: &str) {
    let base = core::ptr::addr_of_mut!(URL) as *mut u8;
    let n = s.len().min(MAX_URL);
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

/// Start fetching what is in the address bar.
fn go() {
    let raw = url_bytes();
    let text = core::str::from_utf8(raw).unwrap_or("");
    if text.is_empty() {
        return;
    }
    serial::print_str("[browser] go ");
    serial::print_str(text);
    serial::print_str("\n");
    unsafe {
        PAGE_LEN = 0;
        LINE_COUNT = 0;
        TOP = 0;
        CAPTURED = false;
        FOCUS = Focus::Page;
    }
    if !http::start(text) {
        serial::print_str("[browser] request refused at start\n");
    }
}

// ---------------------------------------------------------------------------
//  Input
// ---------------------------------------------------------------------------

pub fn on_click(_px: u32, py: u32) -> bool {
    let (_wx, wy) = window::position(window_id()).unwrap_or((WIN_X, WIN_Y));
    // A click in the toolbar gives the address bar the keyboard; anywhere
    // else gives it to the page, which is what makes the arrows scroll.
    unsafe {
        FOCUS = if (py as i32 - wy as i32) < TOOLBAR_H as i32 {
            Focus::Address
        } else {
            Focus::Page
        };
    }
    true
}

pub fn handle_key(key: u8) -> bool {
    let focused = unsafe { FOCUS };
    match key {
        KEY_TAB => {
            unsafe {
                FOCUS = if focused == Focus::Address { Focus::Page } else { Focus::Address };
            }
            true
        }
        KEY_ENTER => {
            go();
            true
        }
        KEY_ESC => {
            http::start("");            // resets the client
            unsafe { PAGE_LEN = 0; LINE_COUNT = 0; TOP = 0; }
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
        KEY_LEFT => {
            if focused == Focus::Address {
                unsafe {
                    if CARET > 0 {
                        CARET -= 1;
                    }
                }
            } else {
                scroll(-(ROWS as isize));
            }
            true
        }
        KEY_RIGHT => {
            if focused == Focus::Address {
                unsafe {
                    if CARET < URL_LEN {
                        CARET += 1;
                    }
                }
            } else {
                scroll(ROWS as isize);
            }
            true
        }
        KEY_BACKSPACE => {
            if focused == Focus::Address {
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
            } else {
                false
            }
        }
        ch if (0x20..0x7F).contains(&ch) => {
            if focused == Focus::Address {
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
            } else {
                false
            }
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
//  Markup to lines
// ---------------------------------------------------------------------------

/// Case-insensitive tag-name comparison against a lowercase literal.
fn tag_is(name: &[u8], want: &[u8]) -> bool {
    name.len() == want.len()
        && name.iter().zip(want).all(|(a, b)| a.to_ascii_lowercase() == *b)
}

/// Turn the fetched body into displayable lines.
///
/// The rule is simple: block-level tags end a line, every other tag is
/// dropped, entities are decoded, and whitespace runs collapse to one space.
/// Script and style *contents* are dropped too — they are not prose and
/// showing them would bury the page.
unsafe fn layout() {
    let body = &raw const PAGE;
    let body = core::slice::from_raw_parts(body as *const u8, PAGE_LEN);
    let out = core::ptr::addr_of_mut!(TEXT) as *mut u8;
    let at = core::ptr::addr_of_mut!(LINE_AT) as *mut u32;
    let ln = core::ptr::addr_of_mut!(LINE_LEN) as *mut u32;

    let mut n = 0usize;             // bytes written to TEXT
    let mut lines = 0usize;         // lines emitted
    let mut line_start = 0usize;
    let mut col = 0usize;

    // The word being accumulated.  Wrapping happens *between* words, so a line
    // never ends in the middle of one.  The first version simply broke at the
    // column limit, which cut "Avoid use of..." into "Avoid use" and then a
    // fragment on the next line.
    let mut word = [0u8; 96];
    let mut wl = 0usize;
    let mut pending_space = false;

    macro_rules! end_line {
        () => {
            if lines < MAX_LINES && n > line_start {
                core::ptr::write_volatile(at.add(lines), line_start as u32);
                core::ptr::write_volatile(ln.add(lines), (n - line_start) as u32);
                lines += 1;
            }
            line_start = n;
            col = 0;
            pending_space = false;
        };
    }

    // Place the buffered word, breaking first if it will not fit.
    macro_rules! flush_word {
        () => {
            if wl > 0 && lines < MAX_LINES {
                let lead = usize::from(pending_space && col > 0);
                if col + lead + wl > COLS {
                    end_line!();
                }
                if pending_space && col > 0 {
                    core::ptr::write_volatile(out.add(n), b' ');
                    n += 1;
                    col += 1;
                }
                for k in 0..wl {
                    core::ptr::write_volatile(out.add(n), word[k]);
                    n += 1;
                }
                col += wl;
                wl = 0;
                pending_space = false;
            }
        };
    }

    // Every byte of text goes through here: whitespace ends the current word
    // and is remembered as a single separating space, anything else joins the
    // word.  Bytes below 0x20 that are not whitespace are dropped; bytes at
    // 0x80 and up are kept, because they are the pieces of a UTF-8 sequence
    // and removing them would corrupt the character that follows.
    macro_rules! push_byte {
        ($b:expr) => {{
            let b: u8 = $b;
            if b == b' ' || b == 0x0A || b == 0x0D || b == 0x09 {
                if wl > 0 {
                    flush_word!();
                }
                if n > line_start {
                    pending_space = true;
                }
            } else if b >= 0x20 && wl < word.len() {
                word[wl] = b;
                wl += 1;
            }
        }};
    }

    let mut i = 0usize;
    while i < body.len() && lines < MAX_LINES && n < PAGE_CAP - 1 {
        let c = body[i];

        if c == b'<' {
            // Read the tag name.
            let mut j = i + 1;
            let closing = j < body.len() && body[j] == b'/';
            if closing {
                j += 1;
            }
            let name_start = j;
            while j < body.len() && (body[j].is_ascii_alphanumeric() || body[j] == b'!') {
                j += 1;
            }
            let name = &body[name_start..j];
            // Skip to the end of the tag.
            while j < body.len() && body[j] != b'>' {
                j += 1;
            }
            i = j + 1;

            let eq = |t: &[u8]| tag_is(name, t);

            // Script and style contents are not text.  Skip to the closing tag.
            if !closing && (eq(b"script") || eq(b"style")) {
                let close: &[u8] = if eq(b"script") { b"</script" } else { b"</style" };
                while i + close.len() <= body.len() {
                    let mut hit = true;
                    for (a, &b) in close.iter().enumerate() {
                        if body[i + a].to_ascii_lowercase() != b {
                            hit = false;
                            break;
                        }
                    }
                    if hit {
                        break;
                    }
                    i += 1;
                }
                continue;
            }

            // Block-level ends the line; inline is simply dropped.
            if eq(b"p") || eq(b"br") || eq(b"div") || eq(b"li") || eq(b"tr")
                || eq(b"ul") || eq(b"ol") || eq(b"table") || eq(b"hr")
                || eq(b"h1") || eq(b"h2") || eq(b"h3") || eq(b"h4") || eq(b"h5")
                || eq(b"h6") || eq(b"pre") || eq(b"blockquote") || eq(b"form")
                || eq(b"section") || eq(b"article") || eq(b"header") || eq(b"footer")
                || eq(b"title")
            {
                flush_word!();
                end_line!();
            }
            continue;
        }

        if c == b'&' {
            // A handful of entities, which is all a plain-text view needs.
            let rest = &body[i..];
            let (rep, used): (&[u8], usize) = if rest.starts_with(b"&amp;") {
                (b"&", 5)
            } else if rest.starts_with(b"&lt;") {
                (b"<", 4)
            } else if rest.starts_with(b"&gt;") {
                (b">", 4)
            } else if rest.starts_with(b"&quot;") {
                (b"\"", 6)
            } else if rest.starts_with(b"&#39;") || rest.starts_with(b"&apos;") {
                (b"'", 5)
            } else if rest.starts_with(b"&nbsp;") {
                (b" ", 6)
            } else {
                (b"&", 1)               // unknown: leave it alone
            };
            for &b in rep {
                push_byte!(b);
            }
            i += used;
            continue;
        }

        push_byte!(c);
        i += 1;
    }

    flush_word!();
    end_line!();
    LINE_COUNT = lines;
}

/// Pull the body out of HTTP's receive buffer into ours, the first time the
/// page is complete.
fn capture() {
    let n = http::body_len().min(PAGE_CAP);
    let base = core::ptr::addr_of_mut!(PAGE) as *mut u8;
    let mut got = 0usize;
    while got < n {
        let mut chunk = [0u8; 512];
        let want = (n - got).min(512);
        let r = http::read_body(got, &mut chunk);
        if r == 0 {
            break;
        }
        for (k, &b) in chunk[..r].iter().enumerate() {
            unsafe { core::ptr::write_volatile(base.add(got + k), b) };
        }
        got += r;
    }
    unsafe {
        PAGE_LEN = got;
        layout();
    }
    serial::print_str("[browser] laid out ");
    serial::print_dec(unsafe { LINE_COUNT } as u64);
    serial::print_str(" lines from ");
    serial::print_dec(got as u64);
    serial::print_str(" bytes\n");

    // Show the head of the result.  Everything above proves the network
    // worked; this proves the markup stripper produced the page's prose
    // rather than its tags, which is the part that goes wrong quietly.
    unsafe {
        let at = core::ptr::addr_of!(LINE_AT) as *const u32;
        let ln = core::ptr::addr_of!(LINE_LEN) as *const u32;
        let text = core::ptr::addr_of!(TEXT) as *const u8;
        for i in 0..unsafe { LINE_COUNT }.min(3) {
            let off = core::ptr::read_volatile(at.add(i)) as usize;
            let len = core::ptr::read_volatile(ln.add(i)) as usize;
            let mut buf = [0u8; 160];
            let n = len.min(159);
            for k in 0..n {
                buf[k] = core::ptr::read_volatile(text.add(off + k));
            }
            if n == 0 {
                continue;
            }
            serial::print_str("[browser]   | ");
            serial::print_str(core::str::from_utf8(&buf[..n]).unwrap_or("?"));
            serial::print_str("\n");
        }
    }
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

fn draw_toolbar(wx: u32, wy: u32) {
    let focused = unsafe { FOCUS } == Focus::Address;
    // A filled bar rather than a border: at 8x16 there is no room for both.
    draw_rect(wx + 6, wy + 20, WIN_W - 12, 20,
              if focused { (0x1E, 0x28, 0x3C) } else { (0x18, 0x18, 0x20) });

    let raw = url_bytes();
    let mut x = wx + 10;
    for (i, &b) in raw.iter().enumerate() {
        if i as u32 >= (WIN_W - 24) / CW {
            break;
        }
        // A caret drawn as a reversed cell is the only cursor this font has.
        let here = unsafe { focused && i == CARET };
        let pal = if here { (0x10, 0xB9, 0x81) } else { (0xEA, 0xEA, 0xEA) };
        font::draw_char(b, x, wy + 22, pal.0, pal.1, pal.2);
        x += CW;
    }
    if unsafe { focused && CARET >= raw.len() } {
        draw_rect(x, wy + 21, 2, 17, (0x10, 0xB9, 0x81));
    }
}

fn draw_page(wx: u32, wy: u32) {
    let top_y = wy + TOOLBAR_H + 20;
    let count = unsafe { LINE_COUNT };
    if count == 0 {
        let msg = match http::phase() {
            http::Phase::Idle => "Type a host and press Enter.",
            http::Phase::Done => "The page had no text.",
            http::Phase::Failed => http::error(),
            _ => "Loading...",
        };
        font::draw_str(msg, wx + 10, top_y, 0xAA, 0xAA, 0xAA);
        return;
    }

    let at = core::ptr::addr_of!(LINE_AT) as *const u32;
    let ln = core::ptr::addr_of!(LINE_LEN) as *const u32;
    let text = core::ptr::addr_of!(TEXT) as *const u8;
    let first = unsafe { TOP };
    for row in 0..ROWS {
        let idx = first + row;
        if idx >= count {
            break;
        }
        let off = unsafe { core::ptr::read_volatile(at.add(idx)) } as usize;
        let len = unsafe { core::ptr::read_volatile(ln.add(idx)) } as usize;
        let mut x = wx + 10;
        let y = top_y + (row as u32) * CH;
        for k in 0..len {
            let b = unsafe { core::ptr::read_volatile(text.add(off + k)) };
            font::draw_char(b, x, y, 0xE8, 0xE8, 0xE8);
            x += CW;
            if x + CW > wx + WIN_W - 6 {
                break;
            }
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
        if http::is_redirect() {
            font::draw_str("redirected", x, y, 0xEE, 0xCC, 0x66);
            x += 11 * CW;
        }
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
/// the phase has changed and repaint, so a page appears as soon as it lands
/// without the compositor redrawing the browser sixty times a second.
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

    if http::phase() == http::Phase::Done && !unsafe { CAPTURED } {
        unsafe { CAPTURED = true; }
        capture();
    }
    true
}
