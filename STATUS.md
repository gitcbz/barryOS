# barryOS — STATUS

**Current round:** Round 3 COMPLETE — Stage 3 (Interrupts & Exceptions) ✅
**Last updated:** 2026-09-21 07:55 (Asia/Shanghai)
**Mode:** AUTONOMOUS

## Verification result
**PASS=26  FAIL=0  SKIP=0** — see CHECK_REPORT.md

| Gate | Result |
|------|--------|
| L0  make all                            | ✅ PASS |
| L1  8/8 artifacts + format              | ✅ PASS |
| L1b-e format checks (4)                | ✅ PASS |
| L2  BIOS QEMU → "barryOS booted"        | ✅ PASS |
| L3  UEFI QEMU → "barryOS booted"        | ✅ PASS |
| L4  Stage 2 memory (5 checks)           | ✅ PASS |
| L5  Stage 3 interrupts online           | ✅ PASS |
| L5  IDT loaded (256 entries)            | ✅ PASS |
| L5  PIC remapped (IRQ0..15 → INT 32..47)| ✅ PASS |
| L5  PIT configured (100 Hz)             | ✅ PASS |
| L5  timer interrupts fired (ticks=2)    | ✅ PASS |

## Stage 3 deliverables
1. **IDT** (`kernel/src/interrupts/idt.rs`): 256-entry Interrupt Descriptor
   Table, 16-byte interrupt gate descriptors, loaded via `lidt`.
   Macro-based handler registration using `lea [rip + sym]` to work around
   Rust nightly `naked_fn as u64 → 0` codegen bug.
2. **GDT + TSS** (`kernel/src/interrupts/gdt.rs`): kernel GDT with null +
   code + data + TSS entries. TSS with IST1 reserved for #DF (double fault
   safe stack). TSS load deferred (caused #GP; Stage 3b will revisit).
3. **CPU exception handlers** (`kernel/src/interrupts/exceptions.rs`):
   All 21 vectors wired: #DE, #DB, #NMI, #BP, #OF, #BR, #UD, #NM, #DF,
   #MX, #TS, #NP, #SS, #GP, #PF, #MF, #AC, #MC, #XM, #VE, reserved 21-31.
   Prints vector + error code + register dump + CR2 (for #PF) then halts.
4. **PIC 8259** (`kernel/src/interrupts/pic.rs`): remap IRQ 0-15 → INT 32-47.
   Masked to enable IRQ0 (timer) + IRQ1 (keyboard) + IRQ2 (cascade).
   EOI (end-of-interrupt) for master/slave.
5. **PIT 8253** (`kernel/src/interrupts/irq.rs`): configured at 100 Hz
   (divisor 0x2E9B = 11931). Channel 0, mode 3 (square wave).
6. **IRQ handlers** (`kernel/src/interrupts/irq.rs`): timer (IRQ0, counts
   ticks), keyboard (IRQ1, scancode→ASCII map), cascade (IRQ2, no-op),
   spurious (IRQ7/IRQ15, no EOI), unhandled IRQ logging.
7. **Raw assembly stubs** (`kernel/src/interrupts/handlers.rs`): `naked_asm`
   macros that save all 15 GPRs, call Rust handler with `&Registers`, then
   restore registers + `iretq`. Separate macros for exceptions with/without
   CPU-pushed error codes.

## Key fixes this round
- **`naked_fn as u64 → 0` codegen bug**: Rust nightly 1.100 returns 0 for
  function pointers of naked functions. Worked around with `lea [rip + sym]`
  inline asm macro (`fn_addr!`).
- **`#[used]` incompatible with `#[unsafe(naked)]`**: used a `static` array
  of function pointers (`KEEP_HANDLERS`) to force the linker to keep stubs.
- **Binary asm labels (`1:`, `2:`)**: Rust 1.100 warns about 0/1-only labels.
  Used `2:`/`2b` pattern (digit 2 is allowed).
- **PIC remap sequence**: added `io_wait()` (out 0x80) between ICW writes
  and masked all IRQs before init.
- **`static mut` reference UB**: used `addr_of_mut!` for IDT access.
- **`options(nomem)` on port I/O**: removed `nomem` from `port_out`/`port_in`
  asm options so the compiler doesn't optimize away I/O writes.

## Known limitations (Stage 3b)
- TSS load (`ltr`) causes #GP — deferred; #DF IST not yet active.
- No syscall/sysenter support (Stage 4).
- No APIC/LAPIC (legacy PIC only; Stage 4b).

## Next round (Stage 4 — Processes & Syscalls)
- [ ] PCB (process control block), kernel threads
- [ ] Ring 0 → Ring 3 privilege transition
- [ ] Round-robin scheduler
- [ ] syscall/sysret ABI, basic calls (write/exit/fork/exec/wait)

## Gates status
- L0-L5: ✅ PASS
- L6 userland hello: deferred (Stage 4)
- L7 graphics frame: deferred (Stage 6)
- L8 compat layer: deferred (Stage 9-10)
- L9 VMware: PENDING (Stage 7+)
