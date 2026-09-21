//! Syscall interface (Stage 4).
//!
//! For Stage 4 we use `int 0x80` (the classic Linux-style software
//! interrupt) instead of `syscall`/`sysret` (which require MSR setup
//! and are more complex).  The IDT vector 0x80 is wired to a handler
//! that dispatches to Rust syscall functions based on the syscall number
//! in RAX.
//!
//! Supported syscalls:
//! - 0: write(fd, buf, len) → bytes written
//! - 1: exit(code)         → never returns
//! - 2: getpid()           → current PID
//! - 3: yield()            → yield CPU
//! - 4: getticks()         → timer tick count

use crate::serial;
use super::{scheduler, process};
use core::sync::atomic::AtomicU64;

/// Syscall numbers.
pub const SYS_WRITE:   u64 = 0;
pub const SYS_EXIT:    u64 = 1;
pub const SYS_GETPID:  u64 = 2;
pub const SYS_YIELD:   u64 = 3;
pub const SYS_GETTICKS: u64 = 4;

/// Syscall call counter (for dashboard).
pub static SYSCALL_COUNT: AtomicU64 = AtomicU64::new(0);

/// Register file passed to the syscall handler.
#[repr(C)]
pub struct SyscallRegs {
    pub rax: u64,    // syscall number (in) / return value (out)
    pub rbx: u64,
    pub rcx: u64,
    pub rdx: u64,    // arg3
    pub rsi: u64,    // arg2
    pub rdi: u64,    // arg1
    pub r8:  u64,
    pub r9:  u64,
    pub r10: u64,
    pub r11: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,
    pub rbp: u64,
}

/// Dispatch a syscall.  Called from the `int 0x80` assembly stub.
#[no_mangle]
pub extern "C" fn syscall_handler(regs: &mut SyscallRegs) {
    SYSCALL_COUNT.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    let nr = regs.rax;

    match nr {
        SYS_WRITE => {
            // write(fd, buf, len) — fd ignored, buf=rsi, len=rdx
            let buf = regs.rsi as *const u8;
            let len = regs.rdx as usize;
            // Print to serial.
            for i in 0..len {
                unsafe {
                    let b = buf.add(i).read_volatile();
                    serial::write_char(b);
                }
            }
            regs.rax = len as u64;
        }
        SYS_EXIT => {
            // exit(code) — mark current process as Exited, yield.
            let code = regs.rdi;
            serial::print_str("[syscall] exit(code=");
            serial::print_hex(code);
            serial::print_str(")\n");
            // Mark current slot as exited.
            let slot = scheduler::current_pid();
            // (In a real OS we'd find the slot by PID; for Stage 4 we just yield.)
            scheduler::yield_cpu();
            regs.rax = 0;
        }
        SYS_GETPID => {
            regs.rax = scheduler::current_pid();
        }
        SYS_YIELD => {
            scheduler::yield_cpu();
            regs.rax = 0;
        }
        SYS_GETTICKS => {
            regs.rax = crate::interrupts::TIMER_TICKS.load(core::sync::atomic::Ordering::Relaxed);
        }
        _ => {
            serial::print_str("[syscall] unknown syscall: ");
            serial::print_hex(nr);
            serial::print_str("\n");
            regs.rax = u64::MAX; // -1
        }
    }
}

/// Test syscalls from kernel mode (simulates what a user program would do).
pub fn test() {
    serial::print_str("[syscall] test: getpid() = ");
    serial::print_hex(scheduler::current_pid());
    serial::print_str("\n");

    serial::print_str("[syscall] test: getticks() = ");
    serial::print_hex(crate::interrupts::TIMER_TICKS.load(core::sync::atomic::Ordering::Relaxed));
    serial::print_str("\n");

    serial::print_str("[syscall] test: write(\"hello from syscall\")\n");
    let msg = b"hello from syscall\n";
    // Call the write handler directly (kernel-mode test).
    let mut regs = SyscallRegs {
        rax: SYS_WRITE,
        rsi: msg.as_ptr() as u64,
        rdx: msg.len() as u64,
        rdi: 1, // fd = stdout
        rbx: 0, rcx: 0, r8: 0, r9: 0, r10: 0, r11: 0,
        r12: 0, r13: 0, r14: 0, r15: 0, rbp: 0,
    };
    syscall_handler(&mut regs);

    serial::print_str("[syscall] total syscalls: ");
    serial::print_hex(SYSCALL_COUNT.load(core::sync::atomic::Ordering::Relaxed));
    serial::print_str("\n");
}
