# barryOS — CHANGELOG

Reverse-chronological.

## 2026-09-21 — Round 3 — Stage 3 (Interrupts & Exceptions) — ✅ COMPLETE
- Implemented full interrupt subsystem in `kernel/src/interrupts/`:
  - `idt.rs` — 256-entry IDT, 16-byte interrupt gates, `lidt` load.
    Macro-based `set_entry!` using `fn_addr!` (inline asm `lea [rip + sym]`)
    to work around Rust nightly `naked_fn as u64 → 0` codegen bug.
  - `gdt.rs` — kernel GDT (null + code + data + TSS), TSS with IST1 for #DF.
    TSS load (`ltr`) deferred — caused #GP (Stage 3b will revisit).
  - `exceptions.rs` — all 21 CPU exception handlers. Prints vector + error
    code + register dump + CR2 (for #PF), then halts.
  - `pic.rs` — 8259 PIC remap (IRQ 0-15 → INT 32-47). io_wait between ICWs.
    EOI for master/slave. Mask enables IRQ0/1/2.
  - `irq.rs` — PIT 8253 at 100 Hz. Timer handler (counts ticks), keyboard
    handler (scancode→ASCII), cascade, spurious, unhandled logging.
  - `handlers.rs` — `naked_asm` stub macros: save 15 GPRs, call Rust
    `exception_handler`/`irq_handler`, restore, `iretq`. Separate macros
    for exceptions with/without CPU-pushed error codes.
    `KEEP_HANDLERS` static forces linker to keep stubs (avoids gc-sections).
- Updated `kernel/src/main.rs`: added `mod interrupts`, calls
  `interrupts::irq::init_pit()` + `interrupts::init()` after memory subsystem.
- Updated `scripts/check.sh`: added L5 gate (5 Stage 3 interrupt checks).
- Dashboard updated: Stage 3 Interrupt Subsystem section with IDT grid,
  PIC diagram, PIT timer, exception table, IRQ handlers, flow diagram.
- FAIL → root cause → fix log:
  1. `naked_fn as u64 → 0` (Rust codegen bug) → `lea [rip + sym]` macro.
  2. `#[used]` incompatible with `#[unsafe(naked)]` → `KEEP_HANDLERS` static.
  3. Binary asm labels `1:`/`2:` → used digit 2 (allowed).
  4. PIC remap didn't take → added `io_wait()` + masked all before init.
  5. `options(nomem)` on port_out optimized away I/O → removed `nomem`.
  6. `static mut` reference UB → `addr_of_mut!`.
  7. TSS `ltr` caused #GP → deferred (no IST1 for #DF yet).
- Verification: `bash scripts/check.sh` → **PASS=26 FAIL=0 SKIP=0**.
  Both BIOS and UEFI boot to "barryOS booted" + interrupt subsystem online
  + timer interrupts confirmed (QEMU -d int shows INT=0x20 fires 12×).

## 2026-09-21 — Round 2 — Stage 2 (Memory Management) — ✅ COMPLETE
- (see previous entry — memory subsystem, 21/21 checks)

## 2026-09-21 — Round 1 — Stage 0 + Stage 1 — ✅ COMPLETE
- (see previous entry — dual-boot MVP, 16/16 checks)
