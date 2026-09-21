//! 80x25 VGA text-mode framebuffer at physical 0xB8000.
//!
//! Self-developed: direct volatile writes to the text buffer.  Two bytes per
//! cell: [ASCII char][colour attribute].  Default attribute is white-on-blue
//! for a recognisable barryOS look.

const VGA_ADDR: usize = 0xB8000;
const VGA_COLS: usize = 80;
const VGA_ROWS: usize = 25;

const ATTR: u8 = 0x1F; // white-on-blue

static mut CURSOR_ROW: usize = 0;
static mut CURSOR_COL: usize = 0;

#[inline]
unsafe fn cell(row: usize, col: usize) -> *mut u16 {
    (VGA_ADDR + (row * VGA_COLS + col) * 2) as *mut u16
}

pub fn clear() {
    unsafe {
        for off in 0..(VGA_COLS * VGA_ROWS) {
            *((VGA_ADDR + off * 2) as *mut u16) = (ATTR as u16) << 8 | b' ' as u16;
        }
        CURSOR_ROW = 0;
        CURSOR_COL = 0;
    }
}

pub fn init() {
    clear();
}

fn newline() {
    unsafe {
        CURSOR_COL = 0;
        CURSOR_ROW += 1;
        if CURSOR_ROW >= VGA_ROWS {
            scroll_up();
            CURSOR_ROW = VGA_ROWS - 1;
        }
    }
}

unsafe fn scroll_up() {
    for r in 1..VGA_ROWS {
        for c in 0..VGA_COLS {
            let src = cell(r, c).read_volatile();
            cell(r - 1, c).write_volatile(src);
        }
    }
    for c in 0..VGA_COLS {
        cell(VGA_ROWS - 1, c).write_volatile((ATTR as u16) << 8 | b' ' as u16);
    }
}

pub fn print_str(s: &str) {
    unsafe {
        for &b in s.as_bytes() {
            if b == b'\n' {
                newline();
                continue;
            }
            cell(CURSOR_ROW, CURSOR_COL).write_volatile((ATTR as u16) << 8 | b as u16);
            CURSOR_COL += 1;
            if CURSOR_COL >= VGA_COLS {
                newline();
            }
        }
    }
}
