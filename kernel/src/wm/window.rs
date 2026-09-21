//! Window manager: window struct + create/move/resize/close/render.
//!
//! A window has a position (x,y), size (w,h), title, and flags.
//! Windows are stored in a static table (16 slots).
//! The topmost (last in z-order) window receives input focus.

use crate::dev::framebuffer;
use crate::serial;
use crate::wm::font;
use core::sync::atomic::{AtomicU64, Ordering};

pub const MAX_WINDOWS: usize = 16;
pub const TITLE_BAR_H: u32 = 20;

/// Window states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum WinState {
    Free = 0,
    Normal = 1,
    Minimized = 2,
}

/// Window struct.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct Window {
    pub id: u64,
    pub state: WinState,
    pub _pad: u32,
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    pub title: [u8; 24],
    pub title_len: usize,
    pub bg_r: u8,
    pub bg_g: u8,
    pub bg_b: u8,
}

impl Window {
    pub const fn empty() -> Self {
        Self {
            id: 0, state: WinState::Free, _pad: 0,
            x: 0, y: 0, w: 0, h: 0,
            title: [0; 24], title_len: 0,
            bg_r: 0, bg_g: 0, bg_b: 0,
        }
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
}

/// Static window table.
static mut WINDOWS: [Window; MAX_WINDOWS] = [Window::empty(); MAX_WINDOWS];
static NEXT_WID: AtomicU64 = AtomicU64::new(1);

fn windows_ptr() -> *mut Window {
    unsafe { core::ptr::addr_of_mut!(WINDOWS) as *mut Window }
}

pub fn init() {
    NEXT_WID.store(1, Ordering::SeqCst);
    serial::print_str("[wm] window table: ");
    serial::print_hex(MAX_WINDOWS as u64);
    serial::print_str(" slots\n");
}

/// Create a window.  Returns window ID or 0 on failure.
pub fn create(x: u32, y: u32, w: u32, h: u32, title: &str) -> u64 {
    let id = NEXT_WID.fetch_add(1, Ordering::SeqCst);
    let base = windows_ptr();
    for i in 0..MAX_WINDOWS {
        unsafe {
            if (*base.add(i)).state == WinState::Free {
                let win = base.add(i);
                (*win).id = id;
                (*win).state = WinState::Normal;
                (*win).x = x;
                (*win).y = y;
                (*win).w = w;
                (*win).h = h;
                (*win).set_title(title);
                (*win).bg_r = 0x1A;
                (*win).bg_g = 0x1F;
                (*win).bg_b = 0x35;
                serial::print_str("[wm] created window: id=");
                serial::print_hex(id);
                serial::print_str(" \"");
                serial::print_str(title);
                serial::print_str("\" at ");
                serial::print_hex(x as u64);
                serial::print_str(",");
                serial::print_hex(y as u64);
                serial::print_str(" ");
                serial::print_hex(w as u64);
                serial::print_str("x");
                serial::print_hex(h as u64);
                serial::print_str("\n");
                return id;
            }
        }
    }
    serial::print_str("[wm] window table full\n");
    0
}

/// Render a single window to the framebuffer.
pub fn render_window(win_idx: usize) {
    let base = windows_ptr();
    unsafe {
        let win = &*base.add(win_idx);
        if win.state != WinState::Normal {
            return;
        }

        // Window shadow (dark offset).
        framebuffer::fill_rect(win.x + 2, win.y + 2, win.w, win.h, 0x08, 0x0A, 0x12);

        // Window body.
        framebuffer::fill_rect(win.x, win.y, win.w, win.h, win.bg_r, win.bg_g, win.bg_b);

        // Title bar (emerald gradient).
        framebuffer::fill_rect(win.x, win.y, win.w, TITLE_BAR_H, 0x10, 0xB9, 0x81);

        // Title bar bottom border.
        framebuffer::fill_rect(win.x, win.y + TITLE_BAR_H - 1, win.w, 1, 0x06, 0x6B, 0x4D);

        // Window border.
        framebuffer::fill_rect(win.x, win.y, win.w, 1, 0x10, 0xB9, 0x81);  // top
        framebuffer::fill_rect(win.x, win.y, 1, win.h, 0x10, 0xB9, 0x81);  // left
        framebuffer::fill_rect(win.x + win.w - 1, win.y, 1, win.h, 0x10, 0xB9, 0x81);  // right
        framebuffer::fill_rect(win.x, win.y + win.h - 1, win.w, 1, 0x10, 0xB9, 0x81);  // bottom

        // Title text (white on emerald).
        font::draw_str(win.title_str(), win.x + 8, win.y + 4, 0xFF, 0xFF, 0xFF);

        // Close button (red circle at top-right).
        let btn_x = win.x + win.w - 14;
        let btn_y = win.y + 6;
        framebuffer::fill_rect(btn_x, btn_y, 10, 10, 0xFF, 0x44, 0x44);
    }
}

/// Render all windows (bottom to top = table order).
pub fn render_all() {
    for i in 0..MAX_WINDOWS {
        render_window(i);
    }
}

/// Count active windows.
pub fn count() -> usize {
    let base = windows_ptr();
    let mut n = 0;
    for i in 0..MAX_WINDOWS {
        unsafe {
            if (*base.add(i)).state == WinState::Normal {
                n += 1;
            }
        }
    }
    n
}
