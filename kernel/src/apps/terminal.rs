//! Terminal application — interactive command prompt.
//!
//! Supports commands: help, ver, ls, mem, ps, echo <text>, clear.
//! Renders output to the framebuffer inside the Terminal window.

use crate::serial;
use crate::wm::font;

/// Terminal cursor position (pixel coords within window).
static mut CURSOR_X: u32 = 0;
static mut CURSOR_Y: u32 = 0;

/// Terminal content area starts below the title bar.
const CONTENT_OFFSET_X: u32 = 8;
const CONTENT_OFFSET_Y: u32 = 28;
const LINE_HEIGHT: u32 = 16;
const CHAR_WIDTH: u32 = 8;

/// Window position (hardcoded for Stage 8).
const WIN_X: u32 = 40;
const WIN_W: u32 = 360;

/// Initialize the terminal.
pub fn init() {
    serial::print_str("[apps] terminal: initialized\n");
}

/// Render the terminal window with initial prompt.
pub fn render() {
    unsafe {
        CURSOR_X = WIN_X + CONTENT_OFFSET_X;
        CURSOR_Y = WIN_X + CONTENT_OFFSET_Y;  // should be WIN_X? No, Y.
    }
    // Fix: use proper Y.
    unsafe { CURSOR_Y = 40 + CONTENT_OFFSET_Y; }
    print_to_fb("barryOS Terminal v0.8", 0x10, 0xB9, 0x81);
    newline();
    print_to_fb("Type 'help' for commands", 0xAA, 0xCC, 0xFF);
    newline();
    newline();
    print_to_fb("$ ", 0x10, 0xB9, 0x81);
    serial::print_str("[apps] terminal: rendered\n");
}

/// Print a string to the framebuffer at the current cursor position.
fn print_to_fb(text: &str, r: u8, g: u8, b: u8) {
    let max_x = WIN_X + WIN_W - 8;
    unsafe {
        for &ch in text.as_bytes() {
            if ch == b'\n' {
                newline();
                continue;
            }
            if CURSOR_X + CHAR_WIDTH > max_x {
                newline();
            }
            font::draw_char(ch, CURSOR_X, CURSOR_Y, r, g, b);
            CURSOR_X += CHAR_WIDTH;
        }
    }
}

/// Move cursor to the next line.
fn newline() {
    unsafe {
        CURSOR_Y += LINE_HEIGHT;
        CURSOR_X = WIN_X + CONTENT_OFFSET_X;
    }
}

/// Process a command string.
pub fn process_command(cmd: &str) {
    serial::print_str("[apps] terminal: cmd=\"");
    serial::print_str(cmd);
    serial::print_str("\"\n");

    print_to_fb(cmd, 0xFF, 0xFF, 0xFF);
    newline();

    // Parse command (first word).
    let cmd_word = cmd.split(' ').next().unwrap_or("");

    match cmd_word {
        "help" => {
            print_to_fb("Commands:", 0x10, 0xB9, 0x81);
            newline();
            print_to_fb("  help  - show this help", 0xCC, 0xCC, 0xCC);
            newline();
            print_to_fb("  ver   - kernel version", 0xCC, 0xCC, 0xCC);
            newline();
            print_to_fb("  ls    - list files", 0xCC, 0xCC, 0xCC);
            newline();
            print_to_fb("  mem   - memory stats", 0xCC, 0xCC, 0xCC);
            newline();
            print_to_fb("  ps    - process list", 0xCC, 0xCC, 0xCC);
            newline();
            print_to_fb("  echo  - echo text", 0xCC, 0xCC, 0xCC);
            newline();
        }
        "ver" => {
            print_to_fb("barryOS v0.8.0", 0xFF, 0xFF, 0xFF);
            newline();
            print_to_fb("Stage 8 Desktop Environment", 0xAA, 0xCC, 0xFF);
            newline();
        }
        "ls" => {
            print_to_fb("4 files in /", 0xFF, 0xFF, 0xFF);
            newline();
            print_to_fb("  motd", 0xCC, 0xCC, 0xCC);
            newline();
            print_to_fb("  hello", 0xCC, 0xCC, 0xCC);
            newline();
            print_to_fb("  version", 0xCC, 0xCC, 0xCC);
            newline();
            print_to_fb("  hostname", 0xCC, 0xCC, 0xCC);
            newline();
        }
        "mem" => {
            print_to_fb("usable: 0x3E00 frames", 0xFF, 0xFF, 0xFF);
            newline();
            print_to_fb("total:  0x10000 frames", 0xFF, 0xFF, 0xFF);
            newline();
        }
        "ps" => {
            print_to_fb("4 processes", 0xFF, 0xFF, 0xFF);
            newline();
            print_to_fb("  PID0 idle", 0xCC, 0xCC, 0xCC);
            newline();
            print_to_fb("  PID1 thread-A", 0xCC, 0xCC, 0xCC);
            newline();
            print_to_fb("  PID2 thread-B", 0xCC, 0xCC, 0xCC);
            newline();
            print_to_fb("  PID3 thread-C", 0xCC, 0xCC, 0xCC);
            newline();
        }
        "echo" => {
            // Find the text after "echo ".
            if let Some(rest) = cmd.get(5..) {
                if !rest.is_empty() {
                    print_to_fb(rest, 0xFF, 0xFF, 0x00);
                    newline();
                }
            }
        }
        "" => {}
        _ => {
            print_to_fb("unknown: ", 0xFF, 0x44, 0x44);
            print_to_fb(cmd_word, 0xFF, 0x44, 0x44);
            newline();
        }
    }

    print_to_fb("$ ", 0x10, 0xB9, 0x81);
}
