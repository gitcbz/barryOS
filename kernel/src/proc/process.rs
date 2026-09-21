//! Process Control Block (PCB) + process table.
//!
//! Each thread has a PCB containing its ID, stack, saved registers,
//! state (Ready/Running/Blocked/Exited), and a name.

use crate::serial;
use super::context::CpuContext;
use core::sync::atomic::{AtomicU64, Ordering};

/// Maximum number of processes/threads.
pub const MAX_PROCESSES: usize = 16;

/// Thread stack size: 16 KiB (4 pages).
pub const STACK_SIZE: usize = 16 * 4096;

/// Process states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessState {
    Free       = 0,
    Ready      = 1,
    Running    = 2,
    Blocked    = 3,
    Exited     = 4,
}

impl ProcessState {
    pub fn name(self) -> &'static str {
        match self {
            Self::Free    => "FREE",
            Self::Ready   => "RDY",
            Self::Running => "RUN",
            Self::Blocked => "BLK",
            Self::Exited  => "EXT",
        }
    }
}

/// Process Control Block.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ProcessControlBlock {
    pub pid: u64,
    pub state: ProcessState,
    pub name: [u8; 16],
    pub name_len: usize,
    pub rsp: u64,          // saved kernel stack pointer
    pub stack_top: u64,    // top of allocated stack (for cleanup)
    pub context: CpuContext,
    pub ticks: u64,        // total CPU ticks consumed
    pub switches: u64,     // number of context switches into this process
}

impl ProcessControlBlock {
    pub const fn empty() -> Self {
        Self {
            pid: 0,
            state: ProcessState::Free,
            name: [0; 16],
            name_len: 0,
            rsp: 0,
            stack_top: 0,
            context: CpuContext::empty(),
            ticks: 0,
            switches: 0,
        }
    }

    pub fn set_name(&mut self, name: &[u8]) {
        let n = name.len().min(15);
        self.name[..n].copy_from_slice(&name[..n]);
        self.name[n] = 0;
        self.name_len = n;
    }

    pub fn name_str(&self) -> &str {
        core::str::from_utf8(&self.name[..self.name_len]).unwrap_or("?")
    }
}

/// Static process table — initialized at runtime by `init()`.
static mut PROC_TABLE: [ProcessControlBlock; MAX_PROCESSES] =
    [ProcessControlBlock {
        pid: 0,
        state: ProcessState::Free,
        name: [0; 16],
        name_len: 0,
        rsp: 0,
        stack_top: 0,
        context: CpuContext { r15: 0, r14: 0, r13: 0, r12: 0, r11: 0, r10: 0,
                             r9: 0, r8: 0, rdi: 0, rsi: 0, rbp: 0, rdx: 0,
                             rcx: 0, rbx: 0, rax: 0, rip: 0, rflags: 0 },
        ticks: 0,
        switches: 0,
    }; MAX_PROCESSES];

/// Next PID counter.
static NEXT_PID: AtomicU64 = AtomicU64::new(1);

/// Get a reference to the process table (unsafe — Rust 2024 static mut).
pub fn table() -> &'static mut [ProcessControlBlock; MAX_PROCESSES] {
    unsafe { &mut *core::ptr::addr_of_mut!(PROC_TABLE) }
}

/// Initialize the process table — all slots are already Free from the
/// static initializer; this just prints confirmation.
pub fn init() {
    serial::print_str("[proc] process table: ");
    serial::print_hex(MAX_PROCESSES as u64);
    serial::print_str(" slots\n");
}

/// Allocate a free PCB slot.  Returns index or None if full.
pub fn alloc_slot() -> Option<usize> {
    let t = table();
    for i in 0..MAX_PROCESSES {
        if t[i].state == ProcessState::Free {
            return Some(i);
        }
    }
    None
}

/// Allocate a PID.
pub fn alloc_pid() -> u64 {
    NEXT_PID.fetch_add(1, Ordering::SeqCst)
}

/// Create the idle process (PID 0).  This process just halts in a loop
/// when there's nothing else to run.
pub fn create_idle() {
    let t = table();
    t[0].pid = 0;
    t[0].state = ProcessState::Ready;
    t[0].set_name(b"idle");
    t[0].ticks = 0;
    t[0].switches = 0;
    // The idle process uses the current kernel stack (we don't switch to it
    // yet — it's the fallback when no other process is runnable).
    serial::print_str("[proc] idle process created: PID 0\n");
}

/// Print the process table (for diagnostics + dashboard).
pub fn print_table() {
    let t = table();
    serial::print_str("[proc] process table:\n");
    let mut count = 0;
    for i in 0..MAX_PROCESSES {
        if t[i].state != ProcessState::Free {
            count += 1;
            serial::print_str("  PID");
            serial::print_hex(t[i].pid);
            serial::print_str(" [");
            serial::print_str(t[i].state.name());
            serial::print_str("] ");
            serial::print_str(t[i].name_str());
            serial::print_str(" ticks=");
            serial::print_hex(t[i].ticks);
            serial::print_str(" sw=");
            serial::print_hex(t[i].switches);
            serial::print_str("\n");
        }
    }
    serial::print_str("[proc] total: ");
    serial::print_hex(count as u64);
    serial::print_str(" processes\n");
}

/// Get the number of non-free processes.
pub fn count() -> usize {
    let t = table();
    let mut n = 0;
    for i in 0..MAX_PROCESSES {
        if t[i].state != ProcessState::Free {
            n += 1;
        }
    }
    n
}
