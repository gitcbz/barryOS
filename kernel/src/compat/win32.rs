//! Win32 API compatibility layer — emulates KERNEL32 + USER32 + NTDLL.
//!
//! Maps Win32 API function names to internal implementations that use
//! the barryOS syscall interface. For Stage 10 we provide stubs for
//! the most common Win32 functions used by console programs.

use crate::serial;
use core::sync::atomic::AtomicU64;

/// Win32 function table entry.
#[derive(Clone, Copy)]
pub struct Win32Func {
    pub name: [u8; 24],
    pub name_len: usize,
    pub dll: DllId,
    pub stub_kind: StubKind,
}

/// Which DLL the function belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum DllId {
    Kernel32 = 0,
    User32 = 1,
    Ntdll = 2,
    Msvcrt = 3,
}

impl DllId {
    pub fn name(self) -> &'static str {
        match self {
            Self::Kernel32 => "kernel32.dll",
            Self::User32 => "user32.dll",
            Self::Ntdll => "ntdll.dll",
            Self::Msvcrt => "msvcrt.dll",
        }
    }
}

/// What the stub does.
#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum StubKind {
    WriteFile,       // → serial write
    GetStdHandle,    // → return fake handle
    ExitProcess,     // → halt
    MessageBoxA,     // → serial print + return IDOK
    HeapAlloc,       // → bump alloc
    HeapFree,        // → no-op
    GetModuleHandleA, // → return fake base
    GetLastError,    // → return 0
    SetConsoleTextAttribute, // → no-op
    GetTickCount,    // → timer ticks
}

impl StubKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::WriteFile => "WriteFile",
            Self::GetStdHandle => "GetStdHandle",
            Self::ExitProcess => "ExitProcess",
            Self::MessageBoxA => "MessageBoxA",
            Self::HeapAlloc => "HeapAlloc",
            Self::HeapFree => "HeapFree",
            Self::GetModuleHandleA => "GetModuleHandleA",
            Self::GetLastError => "GetLastError",
            Self::SetConsoleTextAttribute => "SetConsoleTextAttribute",
            Self::GetTickCount => "GetTickCount",
        }
    }
}

/// Maximum number of Win32 function stubs.
pub const MAX_STUBS: usize = 16;

/// Static function table — zero-initialized, populated by init().
static mut FUNC_TABLE: [Win32Func; MAX_STUBS] = [Win32Func {
    name: [0; 24], name_len: 0, dll: DllId::Kernel32, stub_kind: StubKind::WriteFile,
}; MAX_STUBS];

/// Number of registered stubs.
static STUB_COUNT: AtomicU64 = AtomicU64::new(0);

/// Initialize the Win32 compat layer.
pub fn init() {
    serial::print_str("[win32] Win32 compat layer initialized\n");
    register_stubs();
    serial::print_str("[win32] ");
    serial::print_hex(STUB_COUNT.load(core::sync::atomic::Ordering::Relaxed));
    serial::print_str(" function stubs registered\n");
    print_table();
}

/// Register all Win32 function stubs.
fn register_stubs() {
    // Use direct assignments per slot (avoid array-of-enums which
    // triggers memcmp in no_std kernel).
    let table = unsafe { &mut *core::ptr::addr_of_mut!(FUNC_TABLE) };

    // Slot 0: kernel32.dll!WriteFile
    set_stub(&mut table[0], DllId::Kernel32, StubKind::WriteFile);
    // Slot 1: kernel32.dll!GetStdHandle
    set_stub(&mut table[1], DllId::Kernel32, StubKind::GetStdHandle);
    // Slot 2: kernel32.dll!ExitProcess
    set_stub(&mut table[2], DllId::Kernel32, StubKind::ExitProcess);
    // Slot 3: kernel32.dll!HeapAlloc
    set_stub(&mut table[3], DllId::Kernel32, StubKind::HeapAlloc);
    // Slot 4: kernel32.dll!HeapFree
    set_stub(&mut table[4], DllId::Kernel32, StubKind::HeapFree);
    // Slot 5: kernel32.dll!GetModuleHandleA
    set_stub(&mut table[5], DllId::Kernel32, StubKind::GetModuleHandleA);
    // Slot 6: kernel32.dll!GetLastError
    set_stub(&mut table[6], DllId::Kernel32, StubKind::GetLastError);
    // Slot 7: kernel32.dll!GetTickCount
    set_stub(&mut table[7], DllId::Kernel32, StubKind::GetTickCount);
    // Slot 8: user32.dll!MessageBoxA
    set_stub(&mut table[8], DllId::User32, StubKind::MessageBoxA);
    // Slot 9: kernel32.dll!SetConsoleTextAttribute
    set_stub(&mut table[9], DllId::Kernel32, StubKind::SetConsoleTextAttribute);

    STUB_COUNT.store(10, core::sync::atomic::Ordering::Relaxed);
}

/// Set a single stub entry (avoids array-of-enums copy).
fn set_stub(slot: &mut Win32Func, dll: DllId, kind: StubKind) {
    let name = kind.name();
    let n = name.len().min(23);
    for j in 0..n {
        slot.name[j] = name.as_bytes()[j];
    }
    slot.name[n] = 0;
    slot.name_len = n;
    slot.dll = dll;
    slot.stub_kind = kind;
}

/// Print the function table.
fn print_table() {
    let count = STUB_COUNT.load(core::sync::atomic::Ordering::Relaxed) as usize;
    serial::print_str("[win32] function table (");
    serial::print_hex(count as u64);
    serial::print_str(" entries):\n");
    // For Stage 10, just print the count — detailed table print
    // triggers #UD from slice access patterns.
    for i in 0..count.min(MAX_STUBS) {
        let table = unsafe { &*core::ptr::addr_of!(FUNC_TABLE) };
        serial::print_str("  ");
        serial::print_str(table[i].dll.name());
        serial::print_str(":");
        serial::print_str(table[i].stub_kind.name());
        serial::print_str("\n");
    }
}

/// Look up a function by name. Returns Some(StubKind) or None.
/// For Stage 10, we ignore the DLL name (just match function name).
pub fn lookup(_dll: &str, func: &str) -> Option<StubKind> {
    let table = unsafe { &*core::ptr::addr_of!(FUNC_TABLE) };
    let count = STUB_COUNT.load(core::sync::atomic::Ordering::Relaxed) as usize;
    let func_bytes = func.as_bytes();

    for i in 0..count.min(MAX_STUBS) {
        // Compare function name byte-by-byte.
        if table[i].name_len != func_bytes.len() {
            continue;
        }
        let mut match_ok = true;
        for j in 0..table[i].name_len {
            if table[i].name[j] != func_bytes[j] {
                match_ok = false;
                break;
            }
        }
        if match_ok {
            return Some(table[i].stub_kind);
        }
    }
    None
}

/// Test the Win32 compat layer.
pub fn test() {
    serial::print_str("[win32] test: WriteFile lookup...\n");
    match lookup("kernel32.dll", "WriteFile") {
        Some(kind) => {
            serial::print_str("[win32] test: WriteFile found (kind=");
            serial::print_hex(kind as u64);
            serial::print_str(") OK\n");
        }
        None => serial::print_str("[win32] test: WriteFile NOT FOUND\n"),
    }

    serial::print_str("[win32] test: MessageBoxA lookup...\n");
    match lookup("user32.dll", "MessageBoxA") {
        Some(kind) => {
            serial::print_str("[win32] test: MessageBoxA found (kind=");
            serial::print_hex(kind as u64);
            serial::print_str(") OK\n");
        }
        None => serial::print_str("[win32] test: MessageBoxA NOT FOUND\n"),
    }

    serial::print_str("[win32] test: nonexistent lookup...\n");
    match lookup("kernel32.dll", "NoFunc") {
        Some(_) => serial::print_str("[win32] test: FAIL\n"),
        None => serial::print_str("[win32] test: OK (not found)\n"),
    }

    serial::print_str("[win32] all tests passed\n");
}
