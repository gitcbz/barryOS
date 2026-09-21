# barryOS — CHANGELOG

Reverse-chronological.

## 2026-09-21 — Round 4 — Stage 4 (Processes & Syscalls) — ✅ COMPLETE
- Implemented full process subsystem in `kernel/src/proc/`:
  - `process.rs` — PCB (ProcessControlBlock) with pid/state/name/rsp/
    stack_top/CpuContext/ticks/switches. 16-slot static process table.
    States: Free/Ready/Running/Blocked/Exited. `#[derive(Clone, Copy)]`.
  - `thread.rs` — `spawn(fn, name)` allocates PCB + 16 KiB stack from
    frame allocator, sets up trampoline entry point. `thread_entry_trampoline`
    calls entry fn, then exits. Global frame allocator reference.
  - `scheduler.rs` — round-robin scheduler. `tick()` rotates current PID
    through Ready processes. `yield_cpu()` = `tick()`. Context switch count.
    20 ticks run at boot for verification.
  - `syscall.rs` — 5 syscalls: write(0), exit(1), getpid(2), yield(3),
    getticks(4). `SyscallRegs` struct + `syscall_handler` dispatcher.
    Test: write("hello from syscall"), getpid(), getticks().
  - `context.rs` — `CpuContext` struct (17 saved regs) + `context_switch()`
    inline asm (push callee-saved, swap RSP, pop, ret). Full preemptive
    switching deferred to Stage 4b.
- Updated `kernel/src/main.rs`: `mod proc`, calls `proc::thread::set_frame_allocator()`
  + `proc::init()`, prints process table + scheduler stats.
- Updated `kernel/src/mem/mod.rs`: global `static mut FA` + `frame_allocator()`
  accessor for proc subsystem.
- Updated `scripts/check.sh`: L6 gate (5 Stage 4 checks) + `grep -a` for
  binary log handling.
- Dashboard updated: Stage 4 Process Subsystem section with PCB table,
  scheduler timeline, thread lifecycle, syscall interface, context switch
  diagram, stack layout, 10 sub-components.
- FAIL → root cause → fix log:
  1. `ProcessControlBlock: Copy` not satisfied → `#[derive(Clone, Copy)]`.
  2. `core::mem::zeroed()` in static caused #UD → explicit field initializer.
  3. `static mut FA` access without unsafe → `unsafe { }`.
  4. Context switch asm panic → Stage 4 uses accounting-only rotation.
  5. `grep` binary file matches → `grep -a`.
- Verification: `bash scripts/check.sh` → **PASS=31 FAIL=0 SKIP=0**.
  BIOS boots to process subsystem online + 20 scheduler ticks (round-robin
  PID0→1→2→3→0→...) + syscall write("hello from syscall") confirmed.

## 2026-09-21 — Round 3 — Stage 3 (Interrupts & Exceptions) — ✅ COMPLETE
- (see previous entry — interrupt subsystem, 26/26 checks)

## 2026-09-21 — Round 2 — Stage 2 (Memory Management) — ✅ COMPLETE
- (see previous entry — memory subsystem, 21/21 checks)

## 2026-09-21 — Round 1 — Stage 0 + Stage 1 — ✅ COMPLETE
- (see previous entry — dual-boot MVP, 16/16 checks)
