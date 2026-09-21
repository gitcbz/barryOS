# barryOS — DECISIONS LOG

## [D25] PCB uses `#[derive(Clone, Copy)]` for static array init — 2026-09-21
- **Problem**: `[ProcessControlBlock::empty(); MAX_PROCESSES]` requires
  `Copy` bound, but PCB has a `CpuContext` field (which is Copy) but PCB
  itself wasn't derived Copy.
- **Fix**: Added `#[derive(Clone, Copy)]` to both `ProcessControlBlock`
  and `CpuContext`. PCB is ~200 bytes; copying it is cheap for a 16-slot
  table.

## [D26] Explicit field initializer instead of `core::mem::zeroed()` — 2026-09-21
- **Problem**: `static mut PROC_TABLE = unsafe { core::mem::zeroed() }`
  caused a #UD (invalid opcode) at runtime — the compiler generated a
  `ud2` panic instruction for the zeroed initialization.
- **Fix**: Explicit `ProcessControlBlock { pid: 0, state: Free, ... }`
  initializer with all fields listed. This is const-safe and doesn't
  trigger the `zeroed()` codegen issue.

## [D27] Stage 4 uses accounting-only scheduler rotation — 2026-09-21
- **Problem**: The `context_switch()` inline asm (push callee-saved,
  swap RSP, pop, ret) caused a panic when called — likely because the
  initial thread stack setup was wrong (the trampoline entry point
  wasn't correctly set up for the return address).
- **Decision**: Stage 4 uses an accounting-only rotation: `tick()`
  updates the PCB states + tick/switch counters + current PID pointer
  without actually swapping stacks. This proves the scheduler logic
  works (round-robin PID rotation is verified in serial output).
- **Stage 4b**: Will fix the context switch (debug the stack setup +
  trampoline, implement actual preemptive timer-driven switching).

## [D28] Global frame allocator for proc subsystem — 2026-09-21
- **Problem**: `mem::init()` created a local `BitmapFrameAllocator` that
  was dropped after init. The proc subsystem needs it to allocate thread
  stacks.
- **Fix**: Made the frame allocator a `static mut FA` in `mem/mod.rs`
  with a `frame_allocator()` accessor that returns `&'static mut`.
  `proc::thread::set_frame_allocator()` stores the reference globally.

## [D29] `int 0x80` syscalls (not `syscall`/`sysret`) — 2026-09-21
- **Decision**: Use `int 0x80` (software interrupt) for syscalls
  instead of the `syscall`/`sysret` CPU instructions.
- **Reason**: `syscall`/`sysret` require EFER MSR setup, STAR MSR,
  and a dedicated syscall entry point — complex for Stage 4. `int 0x80`
  uses the existing IDT infrastructure (vector 0x80). Stage 4b may
  upgrade to `syscall`/`sysret` for performance.

## [D30] UEFI timer ticks = 0 (known issue) — 2026-09-21
- **Problem**: On the UEFI boot path, the PIT timer doesn't fire (ticks=0),
  while on BIOS it works (ticks=2).
- **Likely cause**: UEFI firmware may leave the PIC in a state where the
  remap doesn't take effect, or the PIT channel 0 isn't connected the
  same way. OVMF's timer setup differs from SeaBIOS.
- **Workaround**: BIOS is the primary verification path. UEFI timer
  fix is deferred to Stage 4b.

(Previous decisions D01–D24 from Rounds 1-3 are unchanged.)
