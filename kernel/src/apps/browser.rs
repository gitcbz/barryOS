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

static mut BR_WIN_ID: u64 = 0;

const WIN_X: u32 = 56;
const WIN_Y: u32 = 40;
const WIN_W: u32 = 720;
const WIN_H: u32 = 440;

/// Glyph cell.
const CW: u32 = 8;
const CH: u32 = 16;

/// Above the page: the address bar, measured from the bottom of the title bar
/// so the drawing and the click test cannot drift apart.  They did: the hit
/// test put the boundary at `wy + 26` while the field was *drawn* from `wy+20`
/// to `wy+40`, so clicking the address bar almost always landed in the page
/// and handed the keyboard to the scroller.
const TOOLBAR_H: u32 = 28;
/// Below it: where the fetch got to.
const STATUS_H: u32 = 22;

/// Text column limit, from the window width less a small margin.
const COLS: usize = ((WIN_W - 20) / CW) as usize;
/// Rows of page text visible at once.
const ROWS: usize = ((WIN_H - TITLE_BAR_H - TOOLBAR_H - STATUS_H - 8) / CH) as usize;

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

/// State setup without a window; see `terminal::init_state`.
pub fn init_state() {
    set_url(HOME);
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
        PENDING_NAV = None;
        PAGE_OBJ = None;
        RES.active = false;
        RES.count = 0;
        RES.at = 0;
        RES.sheet_count = 0;
        RES.script_count = 0;
    }
    if !http::start(text) {
        serial::print_str("[browser] request refused at start\n");
    }
}

// ---------------------------------------------------------------------------
//  Input
// ---------------------------------------------------------------------------

/// A click puts the caret where it was clicked in the address bar, or — below
/// the toolbar — scrolls the page to the top.
///
/// The browser has no forms, so there is exactly one place text can go and no
/// mode to get stuck in.  The previous version had two, and the first fetch
/// moved the keyboard to the page with no way back short of closing the window.
pub fn on_click(px: u32, py: u32) -> bool {
    let (wx, wy) = window::position(window_id()).unwrap_or((WIN_X, WIN_Y));
    let rel_y = py as i32 - wy as i32;
    if rel_y < (TITLE_BAR_H + TOOLBAR_H) as i32 {
        // Clamp the caret to the text that is actually there.
        let rel_x = (px as i32 - wx as i32 - 10).max(0) as usize;
        let col = (rel_x / CW as usize).min(unsafe { URL_LEN });
        unsafe { CARET = col; }
    } else {
        unsafe { TOP = 0; }
    }
    true
}

pub fn handle_key(key: u8) -> bool {
    match key {
        // Printable characters and the editing keys all go to the address bar.
        // There are no forms on a page, so this is the only place text can go
        // and there is no mode to be stuck in.
        KEY_ENTER => {
            go();
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
//  Markup to lines
// ---------------------------------------------------------------------------

/// Style records, and one index per byte of laid-out text.
///
/// The renderer draws from a flat byte buffer, so a style has to be reachable
/// from a byte offset.  A record is four bytes — three of colour and one of
/// attributes — and there are at most sixty-four of them on a page, because a
/// page's text has a handful of appearances rather than thousands.
const MAX_STYLES: usize = 64;
static mut STYLE_TAB: [[u8; 4]; MAX_STYLES] = [[0; 4]; MAX_STYLES];
static mut STYLE_N: usize = 0;
static mut STYLE_AT: [u8; PAGE_CAP] = [0; PAGE_CAP];

const ATTR_BOLD: u8 = 1;
const ATTR_ITALIC: u8 = 2;
const ATTR_UNDERLINE: u8 = 4;
const ATTR_STRIKE: u8 = 8;

/// The record for a style, made once and shared by every byte that has it.
fn intern(style: &web::css::Style) -> u8 {
    let mut rec = [style.color.0, style.color.1, style.color.2, 0u8];
    if style.bold {
        rec[3] |= ATTR_BOLD;
    }
    if style.italic {
        rec[3] |= ATTR_ITALIC;
    }
    if style.underline {
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
//  Subresources
// ---------------------------------------------------------------------------
//
// A page is rarely one file.  Its stylesheets are `<link href>` and its
// scripts are `<script src>`, and a browser that fetches only the HTML renders
// the markup with no styling and no behaviour — which is the difference
// between the feature working and appearing to.
//
// The HTTP client does one request at a time, so these are fetched in
// sequence after the document and the layout waits for them.  Both lists are
// bounded: a page that references two hundred scripts is a page whose scripts
// would not run here anyway, and fetching them one at a time would take longer
// than anyone would wait.

const MAX_SUBS: usize = 10;
const MAX_CSS: usize = 4;
const MAX_JS: usize = 6;
/// A subresource larger than this is not one: a stylesheet is tens of
/// kilobytes and a script is often more, but not megabytes.
const SUB_CAP: usize = 24 * 1024;

struct Resources {
    urls: [Option<String>; MAX_SUBS],
    css: [bool; MAX_SUBS],
    count: usize,
    /// The next one to fetch; `count` when there are no more.
    at: usize,
    /// Set while a fetch is in flight.
    active: bool,
    sheets: [String; MAX_CSS],
    sheet_count: usize,
    scripts: [String; MAX_JS],
    script_count: usize,
}

static mut RES: Resources = Resources {
    urls: [None, None, None, None, None, None, None, None, None, None],
    css: [false; MAX_SUBS],
    count: 0,
    at: 0,
    active: false,
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

/// Copy the current body into a string, bounded.
unsafe fn body_text(cap: usize) -> String {
    let n = http::body_len().min(cap);
    let mut out = String::with_capacity(n);
    let mut at = 0usize;
    while at < n {
        let mut chunk = [0u8; 512];
        let want = (n - at).min(512);
        let r = http::read_body(at, &mut chunk);
        if r == 0 {
            break;
        }
        out.push_str(&String::from_utf8_lossy(&chunk[..r]));
        at += r;
    }
    out
}

/// Start fetching the next thing the page referenced, if there is one.
/// Returns false when there is nothing left to wait for.
unsafe fn start_next_sub() -> bool {
    let res = &mut *core::ptr::addr_of_mut!(RES);
    while res.at < res.count {
        let url = res.urls[res.at].clone();
        res.at += 1;
        let Some(url) = url else { continue };
        serial::print_str("[browser] fetching ");
        serial::print_str(&url);
        serial::print_str("\n");
        res.active = true;
        http::start(&url);
        return true;
    }
    res.active = false;
    false
}

/// The subresource that was in flight has arrived.
unsafe fn sub_arrived() {
    let res = &mut *core::ptr::addr_of_mut!(RES);
    res.active = false;
    let is_css = res.css[res.at - 1];
    let text = body_text(SUB_CAP);
    // A stylesheet that 404s is the page's problem and not one this browser
    // can fix, so an empty body is simply not added.
    if !text.is_empty() {
        if is_css {
            if res.sheet_count < MAX_CSS {
                res.sheets[res.sheet_count] = text;
                res.sheet_count += 1;
            }
        } else if res.script_count < MAX_JS {
            res.scripts[res.script_count] = text;
            res.script_count += 1;
        }
    }
    if !start_next_sub() {
        finish();
    }
}

/// Everything is in: apply the styles, run the scripts, lay the page out.
unsafe fn finish() {
    let slot = core::ptr::addr_of_mut!(PAGE_OBJ);
    let Some(mut page) = (*slot).take() else { return };
    let res = &mut *core::ptr::addr_of_mut!(RES);
    for sheet in res.sheets.iter().take(res.sheet_count) {
        page.css.push(sheet.clone());
    }
    for script in res.scripts.iter().take(res.script_count) {
        page.scripts.push(script.clone());
    }

    let url = base_url();
    let mark = crate::mem::heap::mark();
    {
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

        let lines = page.layout(COLS);
        copy_lines(&lines);
    }
    // Everything the pipeline allocated, in one step — after dropping the
    // two things that outlive it: the interpreter's prototypes and the DOM
    // host, both held in statics and both heap allocations.  Anything else
    // still reachable from here is a bug that would read as a wild pointer
    // rather than as a use-after-free.
    web::js::value::forget_prototypes();
    web::js::domjs::forget();
    crate::mem::heap::reset_to(mark);

    serial::print_str("[browser] laid out ");
    serial::print_dec(LINE_COUNT as u64);
    serial::print_str(" lines\n");

    // A redirect the page asked for itself, with a script or a meta tag,
    // rather than with a header.  baidu.com's https page is exactly this: a
    // script that rewrites the scheme, and a meta refresh saying the same
    // thing to anyone whose scripts did not run.
    if let Some(target) = PENDING_NAV.take() {
        if let Some(abs) = resolve_url(&url, &target) {
            if abs != url {
                serial::print_str("[browser] the page asked to go to ");
                serial::print_str(&abs);
                serial::print_str("\n");
                set_url(&abs);
                go();
            }
        }
    }
}

/// Copy laid-out lines into the flat buffers the renderer draws from.
unsafe fn copy_lines(lines: &[web::layout::Line]) {
    let out = core::ptr::addr_of_mut!(TEXT) as *mut u8;
    let at = core::ptr::addr_of_mut!(LINE_AT) as *mut u32;
    let ln = core::ptr::addr_of_mut!(LINE_LEN) as *mut u32;
    let st = core::ptr::addr_of_mut!(STYLE_AT) as *mut u8;
    STYLE_N = 0;
    intern(&web::css::Style::initial());

    let mut n = 0usize;
    let mut count = 0usize;
    for line in lines {
        if count >= MAX_LINES || n >= PAGE_CAP {
            break;
        }
        let start = n;
        for run in &line.runs {
            let code = intern(&run.style);
            for b in run.text.bytes() {
                if n >= PAGE_CAP {
                    break;
                }
                core::ptr::write_volatile(out.add(n), b);
                core::ptr::write_volatile(st.add(n), code);
                n += 1;
            }
        }
        // A blank line is still a line: the blank ones are what separate a
        // heading from what follows it.
        core::ptr::write_volatile(at.add(count), start as u32);
        core::ptr::write_volatile(ln.add(count), (n - start) as u32);
        count += 1;
    }
    LINE_COUNT = count;
    TOP = 0;
}

/// Pull the body out of HTTP's receive buffer into ours, the first time the
/// page is complete, and work out what else it needs.
///
/// This is where a fetch becomes a page rather than a document: the markup is
/// parsed, the stylesheets and scripts it names are queued, and the layout is
/// held back until they have arrived.  Laying out first and styling later
/// would show the page twice, once wrong.
unsafe fn capture() {
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
            core::ptr::write_volatile(base.add(got + k), b);
        }
        got += r;
    }
    PAGE_LEN = got;

    let page = web::Page::parse(core::slice::from_raw_parts(base, got));
    let res = &mut *core::ptr::addr_of_mut!(RES);
    res.count = 0;
    res.at = 0;
    res.sheet_count = 0;
    res.script_count = 0;
    res.active = false;
    let base = base_url();
    let mut queue = |href: &str, is_css: bool, res: &mut Resources| {
        if res.count >= MAX_SUBS {
            return;
        }
        if let Some(url) = resolve_url(&base, href) {
            res.urls[res.count] = Some(url);
            res.css[res.count] = is_css;
            res.count += 1;
        }
    };
    for href in page.stylesheets.iter() {
        queue(href, true, res);
    }
    for src in page.script_urls.iter() {
        queue(src, false, res);
    }
    serial::print_str("[browser] ");
    serial::print_dec(got as u64);
    serial::print_str(" bytes of markup, ");
    serial::print_dec(res.count as u64);
    serial::print_str(" subresource(s)\n");

    *core::ptr::addr_of_mut!(PAGE_OBJ) = Some(page);
    if !start_next_sub() {
        finish();
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

/// Top of the address field, in window coordinates.
fn toolbar_y() -> u32 {
    TITLE_BAR_H + 4
}

fn draw_toolbar(wx: u32, wy: u32) {
    // A filled bar rather than a border: at 8x16 there is no room for both.
    draw_rect(wx + 6, wy + toolbar_y(), WIN_W - 12, 20, (0x1E, 0x28, 0x3C));

    let raw = url_bytes();
    let cols = ((WIN_W - 24) / CW) as usize;
    // Scroll the text so the caret is always visible: a URL longer than the
    // field used to be drawn off the right edge with the caret gone with it.
    let caret = unsafe { CARET };
    let first = caret.saturating_sub(cols.saturating_sub(1));
    let mut x = wx + 10;
    let ty = wy + toolbar_y() + 2;
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

fn draw_page(wx: u32, wy: u32) {
    let top_y = wy + TITLE_BAR_H + TOOLBAR_H + 6;
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
    let sty = core::ptr::addr_of!(STYLE_AT) as *const u8;
    let tab = core::ptr::addr_of!(STYLE_TAB) as *const [u8; 4];
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
        let mut k = 0usize;
        while k < len {
            if x + CW > wx + WIN_W - 6 {
                break;
            }
            let code = unsafe { core::ptr::read_volatile(sty.add(off + k)) } as usize;
            // A run: every following byte with the same style, drawn with the
            // same colour and drawn once.
            let mut run_len = 1usize;
            while k + run_len < len
                && unsafe { core::ptr::read_volatile(sty.add(off + k + run_len)) } as usize
                    == code
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
                if b == b'\t' {
                    x += CW * 4;
                    continue;
                }
                font::draw_char(b, x, y, rec[0], rec[1], rec[2]);
                // No bold face and no italic face: the font is one bitmap,
                // and eight by sixteen pixels has no room to fake either.
                // Bold is a second pass one pixel across, which is what a
                // dot-matrix printer did and reads as bold at this size.
                if rec[3] & ATTR_BOLD != 0 && x + 1 < wx + WIN_W - 6 {
                    font::draw_char(b, x + 1, y, rec[0], rec[1], rec[2]);
                }
                if rec[3] & ATTR_UNDERLINE != 0 {
                    draw_rect(x, y + CH - 2, CW, 1, (rec[0], rec[1], rec[2]));
                }
                if rec[3] & ATTR_STRIKE != 0 {
                    draw_rect(x, y + CH / 2, CW, 1, (rec[0], rec[1], rec[2]));
                }
                x += CW;
                if x + CW > wx + WIN_W - 6 {
                    break;
                }
            }
            k += run_len;
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
    // Nothing to repaint while the window is closed.  The fetch itself is
    // driven by net::tick regardless, so a page started before the window was
    // closed still finishes and is there when it is opened again.
    if !window::exists(window_id()) {
        return false;
    }
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

    if http::phase() == http::Phase::Done {
        unsafe {
            if RES.active {
                sub_arrived();
            } else if !CAPTURED {
                CAPTURED = true;
                capture();
            }
        }
    }
    true
}
