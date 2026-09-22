//! Win32 compatibility — real implementations for the functions a console
//! program actually imports.
//!
//! The previous version was a table of ten function *names* with nothing
//! behind them: no call dispatch, no import binding, no way to reach any of
//! it.  These are callable, and the PE loader binds a program's import address
//! table straight to them.
//!
//! Calling convention: x86-64 Windows uses the MS ABI, which Rust spells
//! `extern "win64"` — first four integer arguments in RCX, RDX, R8, R9, and
//! 32 bytes of shadow space on the stack.  `ExitProcess` is deliberately not a
//! Rust function; it is an assembly symbol in `pe_loader`, because returning
//! from a program means unwinding the stack the image was using.

use crate::interrupts;
use crate::serial;
use core::sync::atomic::{AtomicU32, Ordering};

/// The handle `GetStdHandle` hands out.  Not a kernel object — just something
/// non-zero that the write calls recognise.
const CONSOLE_HANDLE: u64 = 0x0000_0000_0000_00C0;

const STD_INPUT: u32 = (-10i32) as u32;
const STD_OUTPUT: u32 = (-11i32) as u32;
const STD_ERROR: u32 = (-12i32) as u32;

static LAST_ERROR: AtomicU32 = AtomicU32::new(0);

extern "C" {
    /// Defined in `pe_loader`'s assembly: sets the exit code and jumps back to
    /// the kernel's call site.
    fn barryos_win32_ExitProcess();
}

// ---------------------------------------------------------------------------
//  Lookup
// ---------------------------------------------------------------------------

/// Resolve an imported name to the address of its implementation.
///
/// Returns None for anything not implemented, which the loader reports by name
/// rather than binding a null and faulting later.
pub fn resolve(name: &str) -> Option<usize> {
    // Case-insensitive: the import table's spelling is the linker's business.
    let addr: *const () = match name {
        n if eq(n, "GetStdHandle") => barryos_win32_GetStdHandle as *const (),
        n if eq(n, "WriteFile") => barryos_win32_WriteFile as *const (),
        n if eq(n, "WriteConsoleA") => barryos_win32_WriteConsoleA as *const (),
        n if eq(n, "ReadFile") => barryos_win32_ReadFile as *const (),
        n if eq(n, "SetConsoleTextAttribute") => barryos_win32_SetConsoleTextAttribute as *const (),
        n if eq(n, "GetLastError") => barryos_win32_GetLastError as *const (),
        n if eq(n, "SetLastError") => barryos_win32_SetLastError as *const (),
        n if eq(n, "GetTickCount") => barryos_win32_GetTickCount as *const (),
        n if eq(n, "GetTickCount64") => barryos_win32_GetTickCount64 as *const (),
        n if eq(n, "GetModuleHandleA") => barryos_win32_GetModuleHandleA as *const (),
        n if eq(n, "GetCommandLineA") => barryos_win32_GetCommandLineA as *const (),
        n if eq(n, "GetProcessHeap") => barryos_win32_GetProcessHeap as *const (),
        n if eq(n, "HeapAlloc") => barryos_win32_HeapAlloc as *const (),
        n if eq(n, "HeapFree") => barryos_win32_HeapFree as *const (),
        n if eq(n, "Sleep") => barryos_win32_Sleep as *const (),
        n if eq(n, "lstrlenA") => barryos_win32_lstrlenA as *const (),
        n if eq(n, "ExitProcess") => barryos_win32_ExitProcess as *const (),
        _ => return None,
    };
    Some(addr as usize)
}

fn eq(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).all(|(x, y)| x.eq_ignore_ascii_case(&y))
}

// ---------------------------------------------------------------------------
//  Console
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "win64" fn barryos_win32_GetStdHandle(which: u32) -> u64 {
    match which {
        STD_INPUT => 0,                 // no stdin yet
        STD_OUTPUT | STD_ERROR => CONSOLE_HANDLE,
        _ => 0,
    }
}

/// Write to the console: the serial port and the terminal window.
///
/// `WriteFile(HANDLE, const void *buf, DWORD n, DWORD *written, OVERLAPPED *)`
#[no_mangle]
pub extern "win64" fn barryos_win32_WriteFile(
    handle: u64,
    buf: *const u8,
    n: u32,
    written: *mut u32,
    _overlapped: u64,
) -> i32 {
    if handle != CONSOLE_HANDLE || buf.is_null() {
        LAST_ERROR.store(6, Ordering::Relaxed);     // ERROR_INVALID_HANDLE
        return 0;
    }
    let len = (n as usize).min(64 * 1024);
    // Copy out first: the buffer belongs to the image, and it would be
    // unreasonable to hold a &[u8] onto it across the kernel's code.
    let mut tmp = [0u8; 512];
    let mut done = 0usize;
    while done < len {
        let chunk = (len - done).min(tmp.len());
        for i in 0..chunk {
            tmp[i] = unsafe { core::ptr::read_volatile(buf.add(done + i)) };
        }
        if let Ok(s) = core::str::from_utf8(&tmp[..chunk]) {
            serial::print_str(s);
            crate::apps::terminal::write_external(s);
        }
        done += chunk;
    }
    if !written.is_null() {
        unsafe { core::ptr::write_unaligned(written, len as u32) };
    }
    1
}

/// `WriteConsoleA(HANDLE, const void *buf, DWORD n, DWORD *written, void *)`
#[no_mangle]
pub extern "win64" fn barryos_win32_WriteConsoleA(
    handle: u64,
    buf: *const u8,
    n: u32,
    written: *mut u32,
    reserved: u64,
) -> i32 {
    barryos_win32_WriteFile(handle, buf, n, written, reserved)
}

/// No colours to set: the terminal palette is fixed.
#[no_mangle]
pub extern "win64" fn barryos_win32_SetConsoleTextAttribute(_handle: u64, _attr: u16) -> i32 {
    1
}

/// There is no stdin yet, so a read reports end-of-file rather than lying.
#[no_mangle]
pub extern "win64" fn barryos_win32_ReadFile(
    _handle: u64,
    _buf: *mut u8,
    _n: u32,
    read: *mut u32,
    _overlapped: u64,
) -> i32 {
    if !read.is_null() {
        unsafe { core::ptr::write_unaligned(read, 0) };
    }
    LAST_ERROR.store(38, Ordering::Relaxed);        // ERROR_HANDLE_EOF
    0
}

// ---------------------------------------------------------------------------
//  Process and misc
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "win64" fn barryos_win32_GetLastError() -> u32 {
    LAST_ERROR.load(Ordering::Relaxed)
}

#[no_mangle]
pub extern "win64" fn barryos_win32_SetLastError(e: u32) {
    LAST_ERROR.store(e, Ordering::Relaxed);
}

/// Milliseconds since boot, from the PIT.
#[no_mangle]
pub extern "win64" fn barryos_win32_GetTickCount() -> u32 {
    let ticks = interrupts::TIMER_TICKS.load(Ordering::Relaxed);
    (ticks.wrapping_mul(10)) as u32            // the PIT runs at 100 Hz
}

#[no_mangle]
pub extern "win64" fn barryos_win32_GetTickCount64() -> u64 {
    interrupts::TIMER_TICKS.load(Ordering::Relaxed).wrapping_mul(10)
}

/// A program asking for its own module base.  We have no loader bookkeeping
/// for that, so it gets a non-null placeholder — enough for the usual
/// "is this a valid HMODULE" check.
#[no_mangle]
pub extern "win64" fn barryos_win32_GetModuleHandleA(_name: *const u8) -> u64 {
    0x0040_0000
}

#[no_mangle]
pub extern "win64" fn barryos_win32_GetCommandLineA() -> *const u8 {
    b"barryos-program\0".as_ptr()
}

/// A token heap handle.  There is one heap and `HeapAlloc` ignores the handle.
#[no_mangle]
pub extern "win64" fn barryos_win32_GetProcessHeap() -> u64 {
    1
}

/// Allocate from the kernel heap.
///
/// A faithful `HeapAlloc` would keep its own arena and honour `HEAP_ZERO_MEMORY`;
/// this hands the request to the global allocator, which is a bump allocator,
/// so the memory is never reclaimed — `HeapFree` is a no-op.  Fine for a
/// program that allocates a buffer and exits; wrong for one that loops.
#[no_mangle]
pub extern "win64" fn barryos_win32_HeapAlloc(_heap: u64, flags: u32, size: usize) -> *mut u8 {
    const HEAP_ZERO_MEMORY: u32 = 0x0000_0008;
    if size == 0 {
        return core::ptr::null_mut();
    }
    let Ok(layout) = alloc::alloc::Layout::from_size_align(size, 16) else {
        return core::ptr::null_mut();
    };
    let p = unsafe { alloc::alloc::alloc(layout) };
    if !p.is_null() && flags & HEAP_ZERO_MEMORY != 0 {
        for i in 0..size {
            unsafe { core::ptr::write_volatile(p.add(i), 0) };
        }
    }
    p
}

#[no_mangle]
pub extern "win64" fn barryos_win32_HeapFree(_heap: u64, _flags: u32, _ptr: *mut u8) -> i32 {
    1                                       // "succeeded"; nothing was reclaimed
}

/// `Sleep(ms)`, using the PIT as the clock.
#[no_mangle]
pub extern "win64" fn barryos_win32_Sleep(ms: u32) {
    let want = (ms / 10) as u64;            // 100 ticks per second
    if want == 0 {
        return;
    }
    let start = interrupts::TIMER_TICKS.load(Ordering::Relaxed);
    while interrupts::TIMER_TICKS.load(Ordering::Relaxed).wrapping_sub(start) < want {
        unsafe { core::arch::asm!("hlt", options(nostack, nomem, preserves_flags)) };
    }
}

/// `lstrlenA(const char *)`
#[no_mangle]
pub extern "win64" fn barryos_win32_lstrlenA(s: *const u8) -> i32 {
    if s.is_null() {
        return 0;
    }
    let mut n = 0i32;
    while n < 1 << 20 {
        if unsafe { core::ptr::read_volatile(s.add(n as usize)) } == 0 {
            break;
        }
        n += 1;
    }
    n
}

// ---------------------------------------------------------------------------
//  Reporting
// ---------------------------------------------------------------------------

/// The names this layer implements, for diagnostics.
pub const NAMES: [&str; 16] = [
    "GetStdHandle", "WriteFile", "WriteConsoleA", "ReadFile",
    "SetConsoleTextAttribute", "GetLastError", "SetLastError",
    "GetTickCount", "GetTickCount64", "GetModuleHandleA", "GetCommandLineA",
    "GetProcessHeap", "HeapAlloc", "HeapFree", "Sleep", "lstrlenA",
];

pub fn init() {
    serial::print_str("[win32] ");
    serial::print_hex(NAMES.len() as u64);
    serial::print_str(" KERNEL32 functions implemented (plus ExitProcess)\n");
}
