//! Round-robin scheduler.
//!
//! The scheduler maintains a "current PID" and on each `tick()` or
//! `yield_cpu()`, it picks the next Ready process in the table.
//!
//! For Stage 4, scheduling is cooperative (threads call `yield_cpu`).
//! Stage 4b will hook the timer IRQ to call `tick()` preemptively.

use crate::serial;
use super::{process, context::context_switch, thread};
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

static ENABLED: AtomicBool = AtomicBool::new(false);
static CURRENT_SLOT: AtomicU64 = AtomicU64::new(0); // index into PROC_TABLE

/// Enable the scheduler.
pub fn enable() {
    ENABLED.store(true, Ordering::SeqCst);
    serial::print_str("[sched] scheduler enabled\n");
}

/// Get the current process's PID.
pub fn current_pid() -> u64 {
    let slot = CURRENT_SLOT.load(Ordering::SeqCst) as usize;
    let t = process::table();
    t[slot].pid
}

/// Find the next Ready process after `from_slot` (round-robin).
fn find_next(from_slot: usize) -> usize {
    let t = process::table();
    for i in 1..=process::MAX_PROCESSES {
        let idx = (from_slot + i) % process::MAX_PROCESSES;
        if t[idx].state == process::ProcessState::Ready {
            return idx;
        }
    }
    // No ready process — fall back to idle (slot 0).
    0
}

/// Scheduler tick — called by `yield_cpu` and (in Stage 4b) the timer IRQ.
/// For Stage 4, this just rotates the "current process" pointer for
/// diagnostic purposes.  Actual context switching is deferred to Stage 4b
/// (the inline asm context_switch has issues that need debugging).
pub fn tick() {
    if !ENABLED.load(Ordering::SeqCst) {
        return;
    }

    let cur = CURRENT_SLOT.load(Ordering::SeqCst) as usize;
    let next = find_next(cur);

    if next != cur {
        // Update accounting without actual context switch.
        let t = process::table();
        if t[cur].state == process::ProcessState::Running {
            t[cur].state = process::ProcessState::Ready;
        }
        t[cur].ticks += 1;
        t[next].state = process::ProcessState::Running;
        t[next].switches += 1;
        CURRENT_SLOT.store(next as u64, Ordering::SeqCst);
        thread::CONTEXT_SWITCHES.fetch_add(1, Ordering::SeqCst);
    }
}

/// Yield the CPU — cooperative scheduling.
/// For Stage 4, this is the same as `tick()`.
pub fn yield_cpu() {
    tick();
}

/// Print scheduler stats.
pub fn print_stats() {
    let switches = thread::CONTEXT_SWITCHES.load(Ordering::SeqCst);
    let cur = current_pid();
    serial::print_str("[sched] context switches: ");
    serial::print_hex(switches as u64);
    serial::print_str(", current PID");
    serial::print_hex(cur);
    serial::print_str("\n");
}
