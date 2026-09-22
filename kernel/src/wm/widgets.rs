//! Desktop chrome: the status bar and the taskbar (dock).
//!
//! The dock used to be a row of identical decorative squares with no hit
//! testing behind them.  It is now a real taskbar: one button for the app
//! launcher plus one per open window, and `dock_buttons` is the single source
//! of truth for both what gets drawn and what a click lands on.

use crate::dev::framebuffer;
use crate::wm::font;
use crate::wm::window;

pub const STATUS_BAR_H: u32 = 20;
pub const DOCK_H: u32 = 36;

/// Launcher button width, and each window button's width.
const LAUNCHER_W: u32 = 92;
const WINBTN_W: u32 = 104;

/// Menu geometry for the app launcher popup.
pub const MENU_ITEM_H: u32 = 22;
pub const MENU_W: u32 = 168;

pub const MAX_DOCK_BUTTONS: usize = 12;

/// A clickable region of the dock.  `win_id == 0` means the app launcher.
#[derive(Clone, Copy)]
pub struct DockButton {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    pub win_id: u64,
}

/// Lay out the dock.  Returns the number of buttons written into `out`.
pub fn dock_buttons(out: &mut [DockButton; MAX_DOCK_BUTTONS]) -> usize {
    let (sw, sh) = framebuffer::size();
    let y = sh.saturating_sub(DOCK_H) + 4;
    let h = DOCK_H - 8;

    let mut n = 0usize;
    let mut x = 6u32;
    out[n] = DockButton { x, y, w: LAUNCHER_W, h, win_id: 0 };
    n += 1;
    x += LAUNCHER_W + 8;

    // Collect ids first: `for_each_open` takes a closure and we cannot borrow
    // `out` mutably from inside it.
    let mut ids = [0u64; MAX_DOCK_BUTTONS];
    let mut cnt = 0usize;
    window::for_each_open(|_i, id| {
        if cnt < MAX_DOCK_BUTTONS {
            ids[cnt] = id;
            cnt += 1;
        }
    });

    for &id in ids.iter().take(cnt) {
        if n >= MAX_DOCK_BUTTONS || x + WINBTN_W > sw.saturating_sub(6) {
            break;
        }
        out[n] = DockButton { x, y, w: WINBTN_W, h, win_id: id };
        n += 1;
        x += WINBTN_W + 6;
    }
    n
}

/// Rectangle of the launcher popup for `items` entries.
pub fn launcher_rect(items: usize) -> (u32, u32, u32, u32) {
    let (_sw, sh) = framebuffer::size();
    let h = (items as u32) * MENU_ITEM_H + 8;
    let y = sh.saturating_sub(DOCK_H + h + 6);
    (6, y, MENU_W, h)
}

/// Rectangle of launcher menu row `i`.
pub fn launcher_item_rect(items: usize, i: usize) -> (u32, u32, u32, u32) {
    let (x, y, w, _h) = launcher_rect(items);
    (x + 4, y + 4 + (i as u32) * MENU_ITEM_H, w - 8, MENU_ITEM_H)
}

/// Draw the status bar.  Shows the focused window's title, so you can tell
/// where the keyboard is going when windows overlap.
pub fn draw_status_bar(screen_w: u32, focused_title: Option<&str>) {
    framebuffer::fill_rect(0, 0, screen_w, STATUS_BAR_H, 0x0A, 0x0D, 0x18);
    framebuffer::fill_rect(0, STATUS_BAR_H - 1, screen_w, 1, 0x10, 0xB9, 0x81);
    font::draw_str("barryOS", 8, 2, 0x10, 0xB9, 0x81);
    if let Some(t) = focused_title {
        font::draw_str(t, 96, 2, 0xAA, 0xCC, 0xFF);
    }
}

/// Draw the dock: background, launcher button, one button per open window.
pub fn draw_dock(focused_id: u64, launcher_open: bool) {
    let (sw, sh) = framebuffer::size();
    let dock_y = sh.saturating_sub(DOCK_H);

    framebuffer::fill_rect(0, dock_y, sw, DOCK_H, 0x12, 0x16, 0x24);
    framebuffer::fill_rect(0, dock_y, sw, 1, 0x10, 0xB9, 0x81);

    let mut btns = [DockButton { x: 0, y: 0, w: 0, h: 0, win_id: 0 }; MAX_DOCK_BUTTONS];
    let n = dock_buttons(&mut btns);

    for (i, b) in btns.iter().enumerate().take(n) {
        let (r, g, bl) = if i == 0 {
            // Launcher: highlight while its menu is open.
            if launcher_open { (0x10, 0xB9, 0x81) } else { (0x24, 0x4A, 0x62) }
        } else if b.win_id == focused_id {
            (0x10, 0xB9, 0x81)
        } else {
            (0x24, 0x2C, 0x42)
        };
        framebuffer::fill_rect(b.x, b.y, b.w, b.h, r, g, bl);
        framebuffer::fill_rect(b.x, b.y, b.w, 1, 0x10, 0xB9, 0x81);

        if i == 0 {
            font::draw_str("All Apps", b.x + 8, b.y + 6, 0xFF, 0xFF, 0xFF);
        } else if let Some((title, len)) = window::title_of(b.win_id) {
            // Clip the label to the button.
            let max_chars = ((b.w - 16) / font::GLYPH_WIDTH) as usize;
            let chars = len.min(max_chars);
            let label = core::str::from_utf8(&title.as_bytes()[..chars]).unwrap_or("?");
            font::draw_str(label, b.x + 8, b.y + 6, 0xFF, 0xFF, 0xFF);
        }
    }
}

/// Draw the app launcher popup.
pub fn draw_launcher(names: &[&str], hover: Option<usize>) {
    let (x, y, w, h) = launcher_rect(names.len());
    framebuffer::fill_rect(x + 2, y + 2, w, h, 0x08, 0x0A, 0x12);
    framebuffer::fill_rect(x, y, w, h, 0x1E, 0x24, 0x38);
    framebuffer::fill_rect(x, y, w, 1, 0x10, 0xB9, 0x81);
    framebuffer::fill_rect(x, y + h - 1, w, 1, 0x10, 0xB9, 0x81);
    framebuffer::fill_rect(x, y, 1, h, 0x10, 0xB9, 0x81);
    framebuffer::fill_rect(x + w - 1, y, 1, h, 0x10, 0xB9, 0x81);

    for (i, name) in names.iter().enumerate() {
        let (ix, iy, iw, ih) = launcher_item_rect(names.len(), i);
        if hover == Some(i) {
            framebuffer::fill_rect(ix, iy, iw, ih, 0x2E, 0x6E, 0x58);
        }
        font::draw_str(name, ix + 8, iy + 3, 0xFF, 0xFF, 0xFF);
    }
}
