//! CPU context (saved register set) for context switching.
//!
//! When the scheduler switches from one thread to another, it saves the
//! current thread's registers into a `CpuContext` on its stack, then
//! loads the next thread's saved registers.  This is a cooperative switch
//! (called from `yield_cpu`); preemptive switching will use the timer IRQ
//! in Stage 4b.

/// Saved CPU state during a context switch.
///
/// Layout matches the push/pop order in `context_switch.S` / inline asm.
/// The order is: r15 r14 r13 r12 r11 r10 r9 r8 rdi rsi rbp rdx rcx rbx rax.
/// RSP is stored separately in the PCB.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct CpuContext {
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub r11: u64,
    pub r10: u64,
    pub r9:  u64,
    pub r8:  u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rbp: u64,
    pub rdx: u64,
    pub rcx: u64,
    pub rbx: u64,
    pub rax: u64,
    pub rip: u64,  // return address (where to resume)
    pub rflags: u64,
}

impl CpuContext {
    pub const fn empty() -> Self {
        Self {
            r15: 0, r14: 0, r13: 0, r12: 0, r11: 0, r10: 0,
            r9: 0, r8: 0, rdi: 0, rsi: 0, rbp: 0, rdx: 0,
            rcx: 0, rbx: 0, rax: 0, rip: 0, rflags: 0,
        }
    }
}

/// Switch from `old_rsp` (current thread's saved RSP) to `new_rsp`
/// (next thread's saved RSP).  Implemented in inline asm: save old regs
/// to old stack, load new RSP, pop new regs, ret.
///
/// # Safety
/// Both RSP values must point to valid, mapped kernel stacks with at
/// least `size_of::<CpuContext>()` bytes of space.
#[inline(never)]
pub unsafe fn context_switch(old_rsp: *mut u64, new_rsp: *const u64) {
    // We use a simpler approach: save callee-saved regs + switch RSP.
    // The CpuContext is stored on each thread's stack.
    core::arch::asm!(
        // Save callee-saved registers to old stack.
        "push rbp",
        "push rbx",
        "push r12",
        "push r13",
        "push r14",
        "push r15",
        // Save old RSP.
        "mov [{old_rsp}], rsp",
        // Load new RSP.
        "mov rsp, [{new_rsp}]",
        // Restore callee-saved registers from new stack.
        "pop r15",
        "pop r14",
        "pop r13",
        "pop r12",
        "pop rbx",
        "pop rbp",
        // Return to the new thread's saved RIP.
        "ret",
        old_rsp = in(reg) old_rsp,
        new_rsp = in(reg) new_rsp,
        options(nostack),
    );
}
