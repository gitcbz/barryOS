//! Login screen.
//!
//! Shown between the boot splash and the desktop.  Blocks until someone
//! authenticates against `/etc/shadow`, then sets the uid the rest of the
//! system runs as — so permissions mean something from the first command.
//!
//! Failed attempts are deliberately slowed down: the PIT gives us a clock, and
//! a login prompt that answers instantly is a free password oracle.

use crate::dev::{framebuffer, keyboard};
use crate::fs::perm;
use crate::interrupts;
use crate::serial;
use crate::wm::font;
use core::sync::atomic::Ordering;

const BG: (u8, u8, u8) = (0x08, 0x0A, 0x14);
const PANEL: (u8, u8, u8) = (0x14, 0x1A, 0x2C);
const EDGE: (u8, u8, u8) = (0x10, 0xB9, 0x81);
const TEXT: (u8, u8, u8) = (0xE0, 0xE0, 0xE0);
const DIM: (u8, u8, u8) = (0x60, 0x78, 0x90);
const ERR: (u8, u8, u8) = (0xFF, 0x55, 0x55);
const HINT: (u8, u8, u8) = (0x40, 0x58, 0x70);

const FIELD_MAX: usize = 24;
const PANEL_W: u32 = 420;
const PANEL_H: u32 = 200;

#[derive(PartialEq, Eq, Clone, Copy)]
enum Field {
    User,
    Pass,
}

static mut USER: [u8; FIELD_MAX + 1] = [0; FIELD_MAX + 1];
static mut USER_LEN: usize = 0;
static mut PASS: [u8; FIELD_MAX + 1] = [0; FIELD_MAX + 1];
static mut PASS_LEN: usize = 0;
static mut FIELD: Field = Field::User;
static mut MESSAGE: [u8; 56] = [0; 56];
static mut MESSAGE_LEN: usize = 0;

/// Ticks to wait after a bad password (the PIT runs at 100 Hz).
const FAIL_DELAY_TICKS: u64 = 100;

fn set_message(msg: &str, _colour: (u8, u8, u8)) {
    unsafe {
        let n = msg.len().min(MESSAGE.len());
        MESSAGE[..n].copy_from_slice(&msg.as_bytes()[..n]);
        MESSAGE_LEN = n;
    }
}

fn panel_origin() -> (u32, u32) {
    let (w, h) = framebuffer::size();
    (
        w.saturating_sub(PANEL_W) / 2,
        h.saturating_sub(PANEL_H) / 2 + 20,
    )
}

fn draw() {
    let (w, h) = framebuffer::size();
    if w == 0 || h == 0 {
        return;
    }
    framebuffer::fill_rect(0, 0, w, h, BG.0, BG.1, BG.2);

    // Title.
    let title = "barryOS";
    let scale = 2;
    let tw = font::text_width(title, scale);
    font::draw_str_scaled(title, w.saturating_sub(tw) / 2, h / 2 - 170, scale, EDGE.0, EDGE.1, EDGE.2);

    let (px, py) = panel_origin();
    framebuffer::fill_rect(px + 3, py + 3, PANEL_W, PANEL_H, 0x04, 0x05, 0x0A);
    framebuffer::fill_rect(px, py, PANEL_W, PANEL_H, PANEL.0, PANEL.1, PANEL.2);
    framebuffer::fill_rect(px, py, PANEL_W, 2, EDGE.0, EDGE.1, EDGE.2);

    let field = unsafe { FIELD };
    let lx = px + 24;
    let fx = px + 130;
    let fw = PANEL_W - 130 - 24;

    // Username row.
    font::draw_str("Username", lx, py + 34, DIM.0, DIM.1, DIM.2);
    let active = field == Field::User;
    framebuffer::fill_rect(fx, py + 30, fw, 22, 0x0C, 0x10, 0x1C);
    if active {
        framebuffer::fill_rect(fx, py + 51, fw, 1, EDGE.0, EDGE.1, EDGE.2);
    }
    unsafe {
        if let Ok(s) = core::str::from_utf8(&USER[..USER_LEN]) {
            font::draw_str(s, fx + 6, py + 33, TEXT.0, TEXT.1, TEXT.2);
        }
        if active {
            let cx = fx + 6 + (USER_LEN as u32) * font::GLYPH_WIDTH;
            framebuffer::fill_rect(cx, py + 33, 2, font::GLYPH_HEIGHT, EDGE.0, EDGE.1, EDGE.2);
        }
    }

    // Password row — always masked.
    font::draw_str("Password", lx, py + 78, DIM.0, DIM.1, DIM.2);
    let active = field == Field::Pass;
    framebuffer::fill_rect(fx, py + 74, fw, 22, 0x0C, 0x10, 0x1C);
    if active {
        framebuffer::fill_rect(fx, py + 95, fw, 1, EDGE.0, EDGE.1, EDGE.2);
    }
    unsafe {
        let n = PASS_LEN;
        let mut cx = fx + 6;
        for _ in 0..n {
            font::draw_char(b'*', cx, py + 77, TEXT.0, TEXT.1, TEXT.2);
            cx += font::GLYPH_WIDTH;
        }
        if active {
            framebuffer::fill_rect(cx, py + 77, 2, font::GLYPH_HEIGHT, EDGE.0, EDGE.1, EDGE.2);
        }
    }

    // Message line.
    unsafe {
        if MESSAGE_LEN > 0 {
            if let Ok(s) = core::str::from_utf8(&MESSAGE[..MESSAGE_LEN]) {
                let colour = if s.starts_with("Login incorrect") { ERR } else { DIM };
                font::draw_str(s, lx, py + 118, colour.0, colour.1, colour.2);
            }
        }
    }

    // Hints: what to press, and where the accounts come from.
    font::draw_str("Enter to continue   Tab to switch fields", lx, py + 146, HINT.0, HINT.1, HINT.2);
    font::draw_str("accounts: /etc/passwd, hashes: /etc/shadow", lx, py + 166, HINT.0, HINT.1, HINT.2);

    framebuffer::flip();
}

/// Run the login screen until someone authenticates.  Returns the uid to run
/// the session as.
pub fn run() -> u32 {
    serial::print_str("[login] login screen up\n");
    unsafe {
        FIELD = Field::User;
        USER_LEN = 0;
        PASS_LEN = 0;
        MESSAGE_LEN = 0;
    }
    set_message("default passwords are the account names", DIM);
    draw();

    // A login prompt nobody can type into is indistinguishable, from the
    // outside, from one nobody has typed into yet — and the difference matters
    // enormously to whoever is sitting in front of it.  So: after a few
    // seconds with no keystroke at all, say so, once.  Silence after that
    // means the reader has not typed; this line means their typing is not
    // reaching the machine, which is a different problem with a different
    // answer.
    let opened = interrupts::TIMER_TICKS.load(Ordering::Relaxed);
    let mut warned = false;

    loop {
        let Some(key) = keyboard::poll() else {
            if !warned
                && interrupts::TIMER_TICKS.load(Ordering::Relaxed).saturating_sub(opened)
                    > 500
                && keyboard::total() == 0
            {
                warned = true;
                serial::print_str(
                    "[login] no keystroke has reached this machine yet -- if one has been
                     [login] typed, the keyboard is not getting through to the guest
");
            }
            // Idle until the next interrupt; the PIT wakes us 100 times a
            // second, which is plenty for a keyboard poll.
            unsafe { core::arch::asm!("hlt", options(nostack, nomem, preserves_flags)) };
            continue;
        };
        warned = true;

        match key {
            keyboard::KEY_TAB => unsafe {
                FIELD = if FIELD == Field::User { Field::Pass } else { Field::User };
            },
            keyboard::KEY_BACKSPACE => unsafe {
                match FIELD {
                    Field::User => {
                        if USER_LEN > 0 { USER_LEN -= 1; }
                    }
                    Field::Pass => {
                        if PASS_LEN > 0 { PASS_LEN -= 1; }
                    }
                }
            },
            keyboard::KEY_ENTER => {
                if unsafe { FIELD } == Field::User {
                    unsafe { FIELD = Field::Pass };
                } else if let Some(uid) = try_login() {
                    return uid;
                }
            }
            ch if ch >= 0x20 && ch < 0x7F => unsafe {
                match FIELD {
                    Field::User => {
                        if USER_LEN < FIELD_MAX {
                            USER[USER_LEN] = ch;
                            USER_LEN += 1;
                        }
                    }
                    Field::Pass => {
                        if PASS_LEN < FIELD_MAX {
                            PASS[PASS_LEN] = ch;
                            PASS_LEN += 1;
                        }
                    }
                }
            },
            _ => {}
        }
        draw();
    }
}

/// Check the entered credentials.
fn try_login() -> Option<u32> {
    let mut name = [0u8; FIELD_MAX + 1];
    let mut pw = [0u8; FIELD_MAX + 1];
    let (nlen, plen) = unsafe {
        name[..USER_LEN].copy_from_slice(&USER[..USER_LEN]);
        pw[..PASS_LEN].copy_from_slice(&PASS[..PASS_LEN]);
        (USER_LEN, PASS_LEN)
    };
    let user = core::str::from_utf8(&name[..nlen]).unwrap_or("");
    let password = core::str::from_utf8(&pw[..plen]).unwrap_or("");

    if !user.is_empty() && perm::verify_password(user, password) {
        if let Some(u) = perm::user_by_name(user) {
            serial::print_str("[login] ");
            serial::print_str(u.name_str());
            serial::print_str(" logged in (uid ");
            serial::print_hex(u.uid as u64);
            serial::print_str(")\n");
            return Some(u.uid);
        }
    }

    serial::print_str("[login] failed login for \"");
    serial::print_str(user);
    serial::print_str("\"\n");

    // Slow the next attempt down, and make the user re-enter the password.
    let start = interrupts::TIMER_TICKS.load(Ordering::Relaxed);
    while interrupts::TIMER_TICKS.load(Ordering::Relaxed) - start < FAIL_DELAY_TICKS {
        unsafe { core::arch::asm!("hlt", options(nostack, nomem, preserves_flags)) };
    }

    unsafe {
        PASS_LEN = 0;
    }
    set_message("Login incorrect", ERR);
    None
}
