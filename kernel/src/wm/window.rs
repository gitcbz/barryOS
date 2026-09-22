//! Window manager: window table, z-order, hit testing and window chrome.
//!
//! The table *is* the z-order: entry 0 is the bottom-most window and the last
//! active entry is the top-most.  Raising a window means shuffling it to the
//! end of the table, so `for_each_active` (used by the compositor) and `at`
//! (used for hit testing) agree on what is on top of what.

use crate::dev::framebuffer;
use crate::serial;
use crate::wm::font;
use crate::wm::widgets;
use core::sync::atomic::{AtomicU64, Ordering};

pub const MAX_WINDOWS: usize = 16;
pub const TITLE_BAR_H: u32 = 20;

/// Title-bar button geometry.  Index 0 is the right-most (close).
const BTN_W: u32 = 14;
const BTN_H: u32 = 12;
const BTN_GAP: u32 = 2;
const BTN_MARGIN: u32 = 6;

/// Which title-bar button was hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitleButton {
    Minimize,
    Maximize,
    Close,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum WinState {
    Free = 0,
    Normal = 1,
    Minimized = 2,
}

impl WinState {
    fn from_u32(v: u32) -> Self {
        match v {
            1 => Self::Normal,
            2 => Self::Minimized,
            _ => Self::Free,
        }
    }
}

/// Window struct.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct Window {
    pub id: u64,
    pub state: u32,          // WinState as u32 so the whole struct stays Copy+const
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    /// Geometry to return to when un-maximising.
    pub rst_x: u32,
    pub rst_y: u32,
    pub rst_w: u32,
    pub rst_h: u32,
    pub maximized: bool,
    pub title: [u8; 24],
    pub title_len: usize,
    pub bg_r: u8,
    pub bg_g: u8,
    pub bg_b: u8,
}

impl Window {
    pub const fn empty() -> Self {
        Self {
            id: 0, state: WinState::Free as u32,
            x: 0, y: 0, w: 0, h: 0,
            rst_x: 0, rst_y: 0, rst_w: 0, rst_h: 0, maximized: false,
            title: [0; 24], title_len: 0,
            bg_r: 0, bg_g: 0, bg_b: 0,
        }
    }

    pub fn wstate(&self) -> WinState {
        WinState::from_u32(self.state)
    }

    pub fn visible(&self) -> bool {
        self.wstate() == WinState::Normal
    }

    pub fn set_title(&mut self, title: &str) {
        let b = title.as_bytes();
        let n = b.len().min(23);
        unsafe {
            let p = self.title.as_mut_ptr();
            for i in 0..n {
                core::ptr::write_volatile(p.add(i), b[i]);
            }
            core::ptr::write_volatile(p.add(n), 0);
        }
        self.title_len = n;
    }

    pub fn title_str(&self) -> &str {
        core::str::from_utf8(&self.title[..self.title_len]).unwrap_or("?")
    }

    /// Rectangle of title-bar button `i` (0 = close, 1 = maximise, 2 = minimise).
    fn button_rect(&self, i: u32) -> (u32, u32, u32, u32) {
        let bx = self.x + self.w
            - BTN_MARGIN
            - BTN_W
            - i * (BTN_W + BTN_GAP);
        (bx, self.y + (TITLE_BAR_H - BTN_H) / 2, BTN_W, BTN_H)
    }
}

/// Static window table.
static mut WINDOWS: [Window; MAX_WINDOWS] = [Window::empty(); MAX_WINDOWS];
static NEXT_WID: AtomicU64 = AtomicU64::new(1);

fn windows_ptr() -> *mut Window {
    unsafe { core::ptr::addr_of_mut!(WINDOWS) as *mut Window }
}

#[inline]
fn win_at(i: usize) -> &'static Window {
    unsafe { &*(windows_ptr().add(i) as *const Window) }
}

#[inline]
fn win_at_mut(i: usize) -> &'static mut Window {
    unsafe { &mut *windows_ptr().add(i) }
}

pub fn init() {
    NEXT_WID.store(1, Ordering::SeqCst);
    unsafe {
        let p = windows_ptr();
        for i in 0..MAX_WINDOWS {
            core::ptr::write_volatile(p.add(i), Window::empty());
        }
    }
    serial::print_str("[wm] window table: ");
    serial::print_hex(MAX_WINDOWS as u64);
    serial::print_str(" slots\n");
}

/// Create a window on top of the stack.  Returns its id, or 0 if the table is
/// full.
pub fn create(x: u32, y: u32, w: u32, h: u32, title: &str) -> u64 {
    // Clamp onto the screen: the GOP mode varies by firmware, and a window
    // hanging off the edge just renders clipped.
    let (_addr, sw, sh, _bpp) = framebuffer::info();
    let x = if sw > w { x.min(sw - w) } else { 0 };
    let y = if sh > h { y.min(sh - h) } else { 0 };

    let id = NEXT_WID.fetch_add(1, Ordering::SeqCst);
    for i in 0..MAX_WINDOWS {
        if win_at(i).wstate() == WinState::Free {
            let w_ = win_at_mut(i);
            *w_ = Window::empty();
            w_.id = id;
            w_.state = WinState::Normal as u32;
            w_.x = x;
            w_.y = y;
            w_.w = w;
            w_.h = h;
            w_.set_title(title);
            w_.bg_r = 0x1A;
            w_.bg_g = 0x1F;
            w_.bg_b = 0x35;

            // Free slots sit in the middle of the table, so a freshly created
            // window would otherwise open *under* existing ones.
            bring_to_front(i);

            serial::print_str("[wm] created window: id=");
            serial::print_hex(id);
            serial::print_str(" \"");
            serial::print_str(title);
            serial::print_str("\"\n");
            return id;
        }
    }
    serial::print_str("[wm] window table full\n");
    0
}

/// Table index of the window with this id.
pub fn index_of(id: u64) -> Option<usize> {
    if id == 0 {
        return None;
    }
    for i in 0..MAX_WINDOWS {
        let w = win_at(i);
        if w.id == id && w.wstate() != WinState::Free {
            return Some(i);
        }
    }
    None
}

/// Does a window with this id exist?
pub fn exists(id: u64) -> bool {
    index_of(id).is_some()
}

/// Id of the window at this table index.
pub fn id_at(idx: usize) -> u64 {
    win_at(idx).id
}

/// Geometry of the window at this table index.
pub fn geometry_by_index(idx: usize) -> Option<(u32, u32, u32, u32)> {
    let w = win_at(idx);
    if w.wstate() == WinState::Free {
        None
    } else {
        Some((w.x, w.y, w.w, w.h))
    }
}

/// Is the window at this index minimised?
pub fn is_minimized(idx: usize) -> bool {
    win_at(idx).wstate() == WinState::Minimized
}

/// Raise the window at `idx` to the top of the z-order.
pub fn bring_to_front(idx: usize) {
    if idx >= MAX_WINDOWS {
        return;
    }
    let p = windows_ptr();
    unsafe {
        // Bubble it to the last slot.  Free slots get shuffled along, which is
        // harmless, and the relative order of every other window is preserved.
        for i in idx..MAX_WINDOWS - 1 {
            let a = p.add(i);
            let b = p.add(i + 1);
            let tmp = core::ptr::read_volatile(a);
            core::ptr::write_volatile(a, core::ptr::read_volatile(b));
            core::ptr::write_volatile(b, tmp);
        }
    }
}

/// Raise the window with this id.
pub fn focus(id: u64) {
    if let Some(i) = index_of(id) {
        bring_to_front(i);
    }
}

/// Top-most visible window containing the point, and its table index.
pub fn at(x: u32, y: u32) -> Option<usize> {
    for i in (0..MAX_WINDOWS).rev() {
        let w = win_at(i);
        if !w.visible() {
            continue;
        }
        if x >= w.x && x < w.x + w.w && y >= w.y && y < w.y + w.h {
            return Some(i);
        }
    }
    None
}

/// Is this point inside the window's title bar?
pub fn in_title_bar(idx: usize, x: u32, y: u32) -> bool {
    let w = win_at(idx);
    w.visible() && x >= w.x && x < w.x + w.w && y >= w.y && y < w.y + TITLE_BAR_H
}

/// Which title-bar button, if any, is under this point?
pub fn button_at(idx: usize, x: u32, y: u32) -> Option<TitleButton> {
    if !win_at(idx).visible() {
        return None;
    }
    for (i, btn) in [TitleButton::Close, TitleButton::Maximize, TitleButton::Minimize]
        .iter()
        .enumerate()
    {
        let (bx, by, bw, bh) = win_at(idx).button_rect(i as u32);
        if x >= bx && x < bx + bw && y >= by && y < by + bh {
            return Some(*btn);
        }
    }
    None
}

/// Retitle a window (e.g. the editor showing which file it has open).
pub fn set_title(id: u64, title: &str) {
    if let Some(i) = index_of(id) {
        win_at_mut(i).set_title(title);
    }
}

/// Move a window, keeping it on screen.
pub fn move_to(idx: usize, x: i32, y: i32) {
    let (_a, sw, sh, _b) = framebuffer::info();
    let w = win_at_mut(idx);
    if w.maximized {
        return;                                 // a maximised window does not drag
    }
    w.x = x.clamp(0, sw.saturating_sub(w.w) as i32) as u32;
    w.y = y.clamp(0, sh.saturating_sub(w.h) as i32) as u32;
}

/// Hide a window without destroying it.
pub fn minimize(idx: usize) {
    win_at_mut(idx).state = WinState::Minimized as u32;
}

/// Show a minimised window again and raise it.
pub fn restore(idx: usize) {
    win_at_mut(idx).state = WinState::Normal as u32;
    bring_to_front(idx);
}

/// Toggle maximise for a window, remembering its previous geometry.
pub fn toggle_maximize(idx: usize) {
    let (ax, ay, aw, ah) = work_area();
    let w = win_at_mut(idx);
    if w.maximized {
        w.x = w.rst_x;
        w.y = w.rst_y;
        w.w = w.rst_w;
        w.h = w.rst_h;
        w.maximized = false;
    } else {
        w.rst_x = w.x;
        w.rst_y = w.y;
        w.rst_w = w.w;
        w.rst_h = w.h;
        w.x = ax;
        w.y = ay;
        w.w = aw;
        w.h = ah;
        w.maximized = true;
    }
}

/// Destroy a window.  The app behind it notices the id is gone and can be
/// reopened from the launcher.
pub fn close(idx: usize) {
    let w = win_at_mut(idx);
    serial::print_str("[wm] closed window id=");
    serial::print_hex(w.id);
    serial::print_str("\n");
    *w = Window::empty();
}

/// Usable desktop area: below the status bar, above the dock.
pub fn work_area() -> (u32, u32, u32, u32) {
    let (sw, sh) = framebuffer::size();
    let top = widgets::STATUS_BAR_H;
    let h = sh.saturating_sub(top + widgets::DOCK_H);
    (0, top, sw, h)
}

/// Visit every visible window in z-order (bottom to top), passing its index.
pub fn for_each_active(mut f: impl FnMut(usize, u64)) {
    for i in 0..MAX_WINDOWS {
        let w = win_at(i);
        if w.visible() {
            f(i, w.id);
        }
    }
}

/// Visit every window that is not free — including minimised ones.  The dock
/// uses this so a minimised window still has a taskbar button.
pub fn for_each_open(mut f: impl FnMut(usize, u64)) {
    for i in 0..MAX_WINDOWS {
        let w = win_at(i);
        if w.wstate() != WinState::Free {
            f(i, w.id);
        }
    }
}

/// Position and size of the window with this id.
pub fn geometry(id: u64) -> Option<(u32, u32, u32, u32)> {
    index_of(id).map(|i| {
        let w = win_at(i);
        (w.x, w.y, w.w, w.h)
    })
}

/// Position of the window with this id.
pub fn position(id: u64) -> Option<(u32, u32)> {
    geometry(id).map(|(x, y, _, _)| (x, y))
}

/// Background colour of the window with this id.
pub fn background(id: u64) -> Option<(u8, u8, u8)> {
    index_of(id).map(|i| {
        let w = win_at(i);
        (w.bg_r, w.bg_g, w.bg_b)
    })
}

/// Title of the window with this id.
pub fn title_of(id: u64) -> Option<(&'static str, usize)> {
    index_of(id).map(|i| {
        let w = win_at(i);
        (w.title_str(), w.title_len)
    })
}

/// Number of windows that are not free.
pub fn count() -> usize {
    let mut n = 0;
    for i in 0..MAX_WINDOWS {
        if win_at(i).wstate() != WinState::Free {
            n += 1;
        }
    }
    n
}

/// Draw one window's chrome: shadow, body, title bar, buttons.
pub fn render_window(idx: usize) {
    let w = win_at(idx);
    if !w.visible() {
        return;
    }

    // Drop shadow.
    framebuffer::fill_rect(w.x + 2, w.y + 2, w.w, w.h, 0x08, 0x0A, 0x12);
    // Body.
    framebuffer::fill_rect(w.x, w.y, w.w, w.h, w.bg_r, w.bg_g, w.bg_b);

    // Title bar.  A maximised window looks the same; a focused one is brighter,
    // which is how you can tell which window has the keyboard.
    let focused = is_topmost(idx);
    let (tr, tg, tb) = if focused { (0x10, 0xB9, 0x81) } else { (0x1C, 0x5A, 0x48) };
    framebuffer::fill_rect(w.x, w.y, w.w, TITLE_BAR_H, tr, tg, tb);
    framebuffer::fill_rect(w.x, w.y + TITLE_BAR_H - 1, w.w, 1, 0x06, 0x6B, 0x4D);

    // Border.
    framebuffer::fill_rect(w.x, w.y, w.w, 1, tr, tg, tb);
    framebuffer::fill_rect(w.x, w.y, 1, w.h, tr, tg, tb);
    framebuffer::fill_rect(w.x + w.w - 1, w.y, 1, w.h, tr, tg, tb);
    framebuffer::fill_rect(w.x, w.y + w.h - 1, w.w, 1, tr, tg, tb);

    // Title text, clipped so it cannot run under the buttons.
    let max_chars = ((w.w.saturating_sub(BTN_MARGIN + 3 * (BTN_W + BTN_GAP) + 8)) / font::GLYPH_WIDTH)
        .saturating_sub(1) as usize;
    let mut used = 0u32;
    let mut cx = w.x + 8;
    for &ch in w.title_str().as_bytes() {
        if used as usize >= max_chars {
            break;
        }
        font::draw_char(ch, cx, w.y + 2, 0xFF, 0xFF, 0xFF);
        cx += font::GLYPH_WIDTH;
        used += 1;
    }

    // Buttons: minimise, maximise, close.
    let (mx, my, mw, mh) = w.button_rect(2);
    framebuffer::fill_rect(mx, my, mw, mh, 0x2A, 0x33, 0x50);
    framebuffer::fill_rect(mx + 3, my + mh / 2, mw - 6, 2, 0xFF, 0xFF, 0xFF);

    let (zx, zy, zw, zh) = w.button_rect(1);
    framebuffer::fill_rect(zx, zy, zw, zh, 0x2A, 0x33, 0x50);
    framebuffer::fill_rect(zx + 3, zy + 3, zw - 6, 2, 0xFF, 0xFF, 0xFF);
    framebuffer::fill_rect(zx + 3, zy + 3, 2, zh - 6, 0xFF, 0xFF, 0xFF);
    framebuffer::fill_rect(zx + 3, zy + zh - 5, zw - 6, 2, 0xFF, 0xFF, 0xFF);

    let (cx2, cy2, cw, chh) = w.button_rect(0);
    framebuffer::fill_rect(cx2, cy2, cw, chh, 0xD0, 0x38, 0x38);
    // A cross, drawn as two diagonals.
    let mut i = 0;
    while i < cw.min(chh) / 2 {
        framebuffer::put_pixel(cx2 + 3 + i, cy2 + 3 + i, 0xFF, 0xFF, 0xFF);
        framebuffer::put_pixel(cx2 + cw - 4 - i, cy2 + 3 + i, 0xFF, 0xFF, 0xFF);
        i += 1;
    }
}

/// Id of the top-most visible window — the one that has focus, and therefore
/// the one keyboard input goes to.
pub fn topmost_id() -> u64 {
    let mut top = 0u64;
    for i in 0..MAX_WINDOWS {
        let w = win_at(i);
        if w.visible() {
            top = w.id;
        }
    }
    top
}

/// Is this the top-most visible window (i.e. the one with focus)?
fn is_topmost(idx: usize) -> bool {
    let mut top = None;
    for i in 0..MAX_WINDOWS {
        if win_at(i).visible() {
            top = Some(i);
        }
    }
    top == Some(idx)
}
