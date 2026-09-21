//! barryOS kernel — Stage 4 process subsystem.
//!
//! Modules:
//! - `process`:    PCB (Process Control Block) + process table + states.
//! - `scheduler`:  Round-robin preemptive scheduler (timer-driven).
//! - `thread`:     Kernel thread spawn/exit/join + context switch.
//! - `syscall`:    syscall/sysret ABI + basic calls (write/exit/getpid/yield).
//! - `context`:    CPU context (saved register set) for context switch.

pub mod process;
pub mod scheduler;
pub mod thread;
pub mod syscall;
pub mod context;

use core::sync::atomic::{AtomicBool, Ordering};
use crate::serial;

pub static INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Initialize the process subsystem:
///   1. Process table (static array of PCB slots).
///   2. Create kernel "idle" process (PID 0).
///   3. Create init/test kernel threads (PID 1, 2, ...).
///   4. Enable preemptive scheduling via timer IRQ.
pub fn init() {
    serial::print_str("[proc] step 1: init process table\n");
    process::init();

    serial::print_str("[proc] step 2: create idle process (PID 0)\n");
    process::create_idle();

    serial::print_str("[proc] step 3: spawn kernel threads\n");
    thread::spawn(test_thread_a, b"thread-A");
    thread::spawn(test_thread_b, b"thread-B");
    thread::spawn(test_thread_c, b"thread-C");

    serial::print_str("[proc] step 4: enable scheduler\n");
    scheduler::enable();

    INITIALIZED.store(true, Ordering::Release);
    serial::print_str("[proc] process subsystem online\n");

    // Run a few scheduler ticks to prove threads switch.
    serial::print_str("[proc] running 20 scheduler ticks...\n");
    for i in 0..20u32 {
        scheduler::tick();
        serial::print_str("[proc] tick ");
        serial::print_hex(i as u64);
        serial::print_str(": current=PID");
        serial::print_hex(scheduler::current_pid() as u64);
        serial::print_str("\n");
    }

    serial::print_str("[proc] syscall test:\n");
    syscall::test();
}

/// Test kernel thread A — prints and yields.
fn test_thread_a(arg: &[u8]) {
    let name = core::str::from_utf8(arg).unwrap_or("?");
    serial::print_str("[thread] ");
    serial::print_str(name);
    serial::print_str(" started\n");
    for i in 0..3u32 {
        serial::print_str("[thread] ");
        serial::print_str(name);
        serial::print_str(" iter ");
        serial::print_hex(i as u64);
        serial::print_str("\n");
        scheduler::yield_cpu();
    }
    serial::print_str("[thread] ");
    serial::print_str(name);
    serial::print_str(" done\n");
}

fn test_thread_b(arg: &[u8]) {
    let name = core::str::from_utf8(arg).unwrap_or("?");
    serial::print_str("[thread] ");
    serial::print_str(name);
    serial::print_str(" started\n");
    for i in 0..3u32 {
        serial::print_str("[thread] ");
        serial::print_str(name);
        serial::print_str(" iter ");
        serial::print_hex(i as u64);
        serial::print_str("\n");
        scheduler::yield_cpu();
    }
    serial::print_str("[thread] ");
    serial::print_str(name);
    serial::print_str(" done\n");
}

fn test_thread_c(arg: &[u8]) {
    let name = core::str::from_utf8(arg).unwrap_or("?");
    serial::print_str("[thread] ");
    serial::print_str(name);
    serial::print_str(" started\n");
    for i in 0..2u32 {
        serial::print_str("[thread] ");
        serial::print_str(name);
        serial::print_str(" iter ");
        serial::print_hex(i as u64);
        serial::print_str("\n");
        scheduler::yield_cpu();
    }
    serial::print_str("[thread] ");
    serial::print_str(name);
    serial::print_str(" done\n");
}
