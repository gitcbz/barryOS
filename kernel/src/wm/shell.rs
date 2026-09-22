//! Desktop shell: turns mouse input into window-manager actions.
//!
//! Everything the pointer can do lives here — raising and dragging windows,
//! the title-bar buttons, the taskbar, and the app launcher.  The input loop
//! calls `on_mouse` whenever the pointer reported movement or a button change,
//! then recomposites.
//!
//! There is no dirty-rectangle tracking: any change repaints the whole
//! desktop.  At these resolutions that costs a few milliseconds, which is
//! cheaper than the bookkeeping would be.
//!
//! The window being dragged is tracked by *id*, not table index: raising a
//! window shuffles the table, so an index captured at press time would point
//! at a different window by the time the first move packet arrives.

use crate::apps;
use crate::dev::framebuffer;
use crate::dev::mouse;
use crate::serial;
use crate::wm::widgets::{self, MAX_DOCK_BUTTONS};
use crate::wm::window::{self, TitleButton};
use core::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, AtomicUsize, Ordering};

/// No window is being dragged.
const NO_DRAG: u64 = 0;
/// Sentinel for "no launcher row hovered".
const NO_HOVER: usize = usize::MAX;

/// Previous button state, for press/release edges.
static PREV_BUTTONS: AtomicUsize = AtomicUsize::new(0);
/// Id of the dragged window (0 = none) and the grab offset within it.
static DRAG_ID: AtomicU64 = AtomicU64::new(NO_DRAG);
static DRAG_DX: AtomicI32 = AtomicI32::new(0);
static DRAG_DY: AtomicI32 = AtomicI32::new(0);

/// Is the app launcher popup showing?
static LAUNCHER_OPEN: AtomicBool = AtomicBool::new(false);
/// Which launcher row the pointer is over (for highlighting).
static LAUNCHER_HOVER: AtomicUsize = AtomicUsize::new(NO_HOVER);

pub fn launcher_open() -> bool {
    LAUNCHER_OPEN.load(Ordering::Relaxed)
}

/// Which launcher row the pointer is over, for highlighting.
pub fn launcher_hover() -> Option<usize> {
    let h = LAUNCHER_HOVER.load(Ordering::Relaxed);
    if h == NO_HOVER { None } else { Some(h) }
}

/// Handle one mouse update.  Returns true if anything changed on screen.
pub fn on_mouse() -> bool {
    let buttons = mouse::buttons();
    let prev = PREV_BUTTONS.swap(buttons as usize, Ordering::Relaxed) as u8;
    let pressed = buttons & !prev & 0x01 != 0;
    let released = prev & !buttons & 0x01 != 0;

    let (px, py) = mouse::position();
    let (px, py) = (px.max(0) as u32, py.max(0) as u32);

    let mut changed = false;

    // Hover highlight for the launcher, even without a click.
    if launcher_open() {
        let hover = launcher_hit(px, py).unwrap_or(NO_HOVER);
        if LAUNCHER_HOVER.load(Ordering::Relaxed) != hover {
            LAUNCHER_HOVER.store(hover, Ordering::Relaxed);
            changed = true;
        }
    }

    if pressed {
        changed |= on_press(px, py);
    }
    if released && DRAG_ID.load(Ordering::Relaxed) != NO_DRAG {
        DRAG_ID.store(NO_DRAG, Ordering::Relaxed);
        changed = true;
    }

    let drag_id = DRAG_ID.load(Ordering::Relaxed);
    if drag_id != NO_DRAG {
        if let Some(idx) = window::index_of(drag_id) {
            let nx = px as i32 - DRAG_DX.load(Ordering::Relaxed);
            let ny = py as i32 - DRAG_DY.load(Ordering::Relaxed);
            window::move_to(idx, nx, ny);
        }
        changed = true;
    }

    changed
}

/// Left button went down at (px, py).
fn on_press(px: u32, py: u32) -> bool {
    // 1. An open launcher swallows the click.
    if launcher_open() {
        if let Some(i) = launcher_hit(px, py) {
            serial::print_str("[shell] launching app index ");
            serial::print_hex(i as u64);
            serial::print_str("\n");
            apps::launch(i);
        }
        LAUNCHER_OPEN.store(false, Ordering::Relaxed);
        LAUNCHER_HOVER.store(NO_HOVER, Ordering::Relaxed);
        return true;
    }

    // 2. The taskbar.
    let (_sw, sh) = framebuffer::size();
    if py >= sh.saturating_sub(widgets::DOCK_H) {
        return on_dock_click(px, py);
    }

    // 3. A window.  Decide everything that depends on the hit position *before*
    //    raising, because raising changes the table index.
    if let Some(idx) = window::at(px, py) {
        let id = window::id_at(idx);
        let was_top = is_topmost(idx);
        let in_title = window::in_title_bar(idx, px, py);

        // Title-bar buttons only act on a window that already has focus, so a
        // stray click on an inactive window's close box doesn't destroy it.
        if was_top {
            if let Some(btn) = window::button_at(idx, px, py) {
                match btn {
                    TitleButton::Close => window::close(idx),
                    TitleButton::Minimize => window::minimize(idx),
                    TitleButton::Maximize => window::toggle_maximize(idx),
                }
                return true;
            }
        }

        if in_title {
            if let Some((wx, wy, _, _)) = window::geometry_by_index(idx) {
                DRAG_DX.store(px as i32 - wx as i32, Ordering::Relaxed);
                DRAG_DY.store(py as i32 - wy as i32, Ordering::Relaxed);
                DRAG_ID.store(id, Ordering::Relaxed);
            }
        }

        window::focus(id);

        // The rest of the window belongs to the app behind it.
        if !in_title {
            apps::on_click(id, px, py);
        }
        return true;
    }

    // 4. Bare desktop: nothing to do (yet).
    false
}

/// A click landed in the dock.
fn on_dock_click(px: u32, py: u32) -> bool {
    let mut btns =
        [widgets::DockButton { x: 0, y: 0, w: 0, h: 0, win_id: 0 }; MAX_DOCK_BUTTONS];
    let n = widgets::dock_buttons(&mut btns);

    for b in btns.iter().take(n) {
        if px >= b.x && px < b.x + b.w && py >= b.y && py < b.y + b.h {
            if b.win_id == 0 {
                // Launcher toggle.
                LAUNCHER_OPEN.store(!launcher_open(), Ordering::Relaxed);
                LAUNCHER_HOVER.store(NO_HOVER, Ordering::Relaxed);
                return true;
            }
            // Taskbar button: minimised -> restore, focused -> minimise,
            // otherwise raise.  What any desktop taskbar does.
            return match window::index_of(b.win_id) {
                Some(idx) => {
                    if window::is_minimized(idx) {
                        window::restore(idx);
                    } else if is_topmost(idx) {
                        window::minimize(idx);
                    } else {
                        window::bring_to_front(idx);
                    }
                    true
                }
                None => false,
            };
        }
    }
    false
}

/// Which launcher row, if any, is under this point.
fn launcher_hit(px: u32, py: u32) -> Option<usize> {
    let n = apps::count();
    for i in 0..n {
        let (x, y, w, h) = widgets::launcher_item_rect(n, i);
        if px >= x && px < x + w && py >= y && py < y + h {
            return Some(i);
        }
    }
    None
}

fn is_topmost(idx: usize) -> bool {
    let mut top = None;
    window::for_each_active(|i, _| top = Some(i));
    top == Some(idx)
}
