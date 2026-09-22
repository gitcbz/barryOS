//! Desktop compositor: background, status bar, windows, dock, launcher.
//!
//! One pass, bottom to top.  Each window's chrome is drawn immediately
//! followed by that window's content, so a window is only opaque if the two
//! happen together — drawing every frame first and every app's text after let
//! a lower window's text float over a higher window's body.

use crate::apps;
use crate::dev::framebuffer;
use crate::serial;
use crate::wm::shell;
use crate::wm::widgets;
use crate::wm::window;

/// Create the default desktop.  In Stage 8, apps create their own
/// windows, so this just initializes the WM if needed.
pub fn create_desktop() {
    serial::print_str("[wm] desktop created (apps create their own windows)\n");
}

/// Render the complete desktop.
pub fn render() {
    let (fb_addr, w, h, _bpp) = framebuffer::info();
    if fb_addr == 0 || w == 0 || h == 0 {
        serial::print_str("[wm] framebuffer not available\n");
        return;
    }

    // Desktop background (dark navy).
    framebuffer::fill_rect(0, 0, w, h, 0x0A, 0x0D, 0x18);

    // The focused window is the top-most visible one (the table is z-ordered).
    let mut focused: Option<usize> = None;
    window::for_each_active(|idx, _| focused = Some(idx));
    let focused_id = focused.map(window::id_at).unwrap_or(0);
    let focused_title = focused.and_then(|i| window::title_of(window::id_at(i)))
        .map(|(t, _)| t);

    widgets::draw_status_bar(w, focused_title);

    // Composite bottom-to-top: frame then content, per window.
    window::for_each_active(|idx, id| {
        window::render_window(idx);
        apps::render_content(id);
    });

    widgets::draw_dock(focused_id, shell::launcher_open());

    if shell::launcher_open() {
        let mut names: [&str; 8] = [""; 8];
        let n = apps::count().min(names.len());
        for (i, slot) in names.iter_mut().enumerate().take(n) {
            *slot = apps::name(i);
        }
        widgets::draw_launcher(&names[..n], shell::launcher_hover());
    }

    serial::print_str("[wm] desktop rendered (background + status bar + ");
    serial::print_hex(window::count() as u64);
    serial::print_str(" windows + dock)\n");
}
