//! Kernel thread spawn/exit + context switch.
//!
//! A kernel thread is a process that runs in Ring 0 with its own stack.
//! `spawn(fn, name)` creates a new PCB, allocates a stack (from the
//! frame allocator), and sets up the initial context so that when the
//! scheduler switches to it, `context_switch` will "return" into `fn`.
//!
//! For Stage 4, threads are cooperative (they call `yield_cpu` to
//! give up the CPU).  Stage 4b will add preemptive timer-driven switches.

use crate::{serial, mem::frame_alloc::BitmapFrameAllocator};
use super::{process, context::context_switch};
use core::sync::atomic::{AtomicUsize, Ordering};

/// Thread entry point: receives a `&[u8]` argument (thread name or data).
pub type ThreadEntry = fn(&[u8]);

/// Allocate a thread stack from the frame allocator.
/// Returns (vaddr, phys) — we identity-map so they're the same.
fn alloc_stack(fa: &mut BitmapFrameAllocator, pages: usize) -> u64 {
    let phys = fa.alloc_contig(pages);
    if phys == 0 {
        serial::print_str("[thread] FATAL: cannot allocate stack\n");
        return 0;
    }
    // Zero the stack.
    unsafe {
        let p = phys as *mut u8;
        let mut i = 0;
        while i < pages * 4096 {
            p.add(i).write_volatile(0);
            i += 1;
        }
    }
    phys
}

/// We need a global frame allocator for thread stacks.  The one passed
/// to `mem::init` is local; we store a pointer to it here.
/// (Stage 4 is single-threaded until the scheduler runs, so this is safe.)
static mut FRAME_ALLOC: Option<&'static mut BitmapFrameAllocator> = None;

/// Set the global frame allocator (called from `proc::init` in main.rs).
pub fn set_frame_allocator(fa: &'static mut BitmapFrameAllocator) {
    unsafe { FRAME_ALLOC = Some(fa); }
}

/// Spawn a new kernel thread.
/// `entry` is the function, `name` is a byte-slice label.
/// Returns the PID (0 = failure).
pub fn spawn(entry: ThreadEntry, name: &[u8]) -> u64 {
    // Allocate a PCB slot.
    let slot = match process::alloc_slot() {
        Some(s) => s,
        None => {
            serial::print_str("[thread] process table full\n");
            return 0;
        }
    };

    // Allocate a stack (4 pages = 16 KiB).
    let fa = match unsafe { FRAME_ALLOC.as_mut() } {
        Some(f) => f,
        None => {
            serial::print_str("[thread] no frame allocator\n");
            return 0;
        }
    };
    let stack_pages = 4;
    let stack_base = alloc_stack(fa, stack_pages);
    if stack_base == 0 {
        return 0;
    }
    let stack_top = stack_base + (stack_pages * 4096) as u64;

    // Build the initial stack frame so that `context_switch` will "return"
    // into `thread_entry_trampoline(entry, name)`.
    // The trampoline saves the args, calls `entry(arg)`, then calls `exit`.
    //
    // Stack layout (grows down from stack_top):
    //   [stack_top]
    //   rflags = 0x202 (IF=1)
    //   rip = thread_entry_trampoline address
    //   rdi = entry fn pointer
    //   rsi = name pointer (we store the name in the PCB, pass &name)
    //   rbp = 0
    //   ... (callee-saved regs zeroed)
    //   <-- initial RSP

    let pid = process::alloc_pid();
    {
        let t = process::table();
        let pcb = &mut t[slot];
        pcb.pid = pid;
        pcb.state = process::ProcessState::Ready;
        pcb.set_name(name);
        pcb.stack_top = stack_top;
        pcb.ticks = 0;
        pcb.switches = 0;

        // Store the name in the PCB so the trampoline can pass it.
        // (We already did set_name above.)

        // Build initial stack.
        let mut sp = stack_top;
        unsafe {
            // rflags (with IF=1)
            sp -= 8;
            (sp as *mut u64).write_volatile(0x202);
            // rip → trampoline
            sp -= 8;
            (sp as *mut u64).write_volatile(thread_entry_trampoline as u64);
            // callee-saved regs (zeroed — context_switch pops these)
            sp -= 8; (sp as *mut u64).write_volatile(0); // rbp
            sp -= 8; (sp as *mut u64).write_volatile(0); // rbx
            sp -= 8; (sp as *mut u64).write_volatile(0); // r12
            sp -= 8; (sp as *mut u64).write_volatile(0); // r13
            sp -= 8; (sp as *mut u64).write_volatile(0); // r14
            sp -= 8; (sp as *mut u64).write_volatile(0); // r15
        }
        pcb.rsp = sp;
    }

    serial::print_str("[thread] spawned PID");
    serial::print_hex(pid);
    serial::print_str(" \"");
    serial::print_str(core::str::from_utf8(name).unwrap_or("?"));
    serial::print_str("\" stack=0x");
    serial::print_hex(stack_top);
    serial::print_str("\n");

    pid
}

/// Trampoline that all new threads start in.
/// Sets up the args and calls the actual entry function.
///
/// # Safety
/// This is called via context switch — RDI = entry fn ptr, but we need
/// to get the name from the PCB.  For simplicity, we pass the slot index
/// via RDI and look up the name.
#[no_mangle]
extern "C" fn thread_entry_trampoline(slot: u64) -> ! {
    let slot = slot as usize;
    let t = process::table();
    // Copy the name out of the PCB.
    let mut name_buf = [0u8; 16];
    let name_len = t[slot].name_len;
    name_buf[..name_len].copy_from_slice(&t[slot].name[..name_len]);

    // Mark as running.
    t[slot].state = process::ProcessState::Running;

    serial::print_str("[thread] trampoline PID");
    serial::print_hex(t[slot].pid);
    serial::print_str("\n");

    // We need the entry fn pointer.  For Stage 4, we store it in
    // pcb.context.rdi (a hack, but works for a trampoline).
    let entry: ThreadEntry = unsafe { core::mem::transmute(t[slot].context.rdi) };
    entry(&name_buf[..name_len]);

    // Thread returned — mark as exited.
    exit(slot);
}

/// Exit the current thread.
fn exit(slot: usize) -> ! {
    let t = process::table();
    t[slot].state = process::ProcessState::Exited;
    serial::print_str("[thread] PID");
    serial::print_hex(t[slot].pid);
    serial::print_str(" exited\n");
    // Yield forever — scheduler will pick another.
    loop {
        super::scheduler::yield_cpu();
    }
}

/// Counter for context switches.
pub static CONTEXT_SWITCHES: AtomicUsize = AtomicUsize::new(0);
