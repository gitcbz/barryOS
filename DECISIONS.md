# barryOS — DECISIONS LOG

## [D18] IDT handler address via `lea [rip + sym]` macro — 2026-09-21
- **Problem**: Rust nightly 1.100 has a codegen bug where `naked_fn as u64`
  returns 0 for functions marked `#[naked]`. The IDT entries all pointed
  at 0x0, so interrupts crashed.
- **Fix**: `fn_addr!` macro that emits `lea {out}, [rip + {sym}]` inline
  asm, capturing the function symbol directly. This gives the correct
  runtime address. The `set_entry!` macro wraps this + IDT field writes.
- **Trade-off**: Can't pass function pointers as parameters (the `sym`
  operand requires a path, not a value), so we use a macro per IDT entry.

## [D19] KEEP_HANDLERS static (force linker retention) — 2026-09-21
- **Problem**: `#[used]` attribute is incompatible with `#[unsafe(naked)]`.
  Without it, `--gc-sections` removes the handler stubs (they're only
  referenced via the IDT, which the compiler can't see).
- **Fix**: A `pub static KEEP_HANDLERS: [unsafe extern "C" fn() -> (); 5]`
  array referencing 5 representative handlers. The `#[used]` on this
  static keeps it, and the function references keep all stubs alive.

## [D20] Separate exception macros (with/without error code) — 2026-09-21
- **Problem**: Using `test {has_err}, {has_err}` + conditional jump in a
  single macro generated `test 0, 0` which is invalid x86.
- **Fix**: Two macros: `exception_stub_no_err!` (pushes fake 0 error code)
  and `exception_stub_err!` (CPU already pushed error code). Exceptions
  with error codes: #DF, #TS, #NP, #SS, #GP, #PF.

## [D21] Binary asm labels use `2:`/`2b` (not `1:`/`1b`) — 2026-09-21
- **Problem**: Rust 1.100 warns "avoid using labels containing only the
  digits 0 and 1" (binary asm labels feature). Using `1:` caused a deny
  warning.
- **Fix**: Used digit `2` for local labels (`2:`, `2f`, `2b`). Digit 2+
  is allowed.

## [D22] PIC io_wait + pre-mask — 2026-09-21
- **Problem**: PIC remap wasn't taking effect — QEMU still delivered
  INT 0x08 (old IRQ0 vector) instead of INT 0x20.
- **Fix**: Added `io_wait()` (writes 0 to port 0x80, the diagnostic
  port) between each ICW write. Also masked all IRQs (0xFF) before
  starting init to prevent spurious interrupts during reconfig.

## [D23] TSS load (ltr) deferred to Stage 3b — 2026-09-21
- **Problem**: `ltr` with our TSS selector caused a #GP (triple fault
  since no IDT was loaded yet at that point in the init sequence).
- **Decision**: Skip the TSS load for now. The IDT still works for
  exceptions and IRQs; only #DF (double fault) won't use the IST1 safe
  stack. Stage 3b will debug the TSS (likely an access byte or limit
  field issue in the 16-byte TSS descriptor).

## [D24] port_out/port_in without `nomem` option — 2026-09-21
- **Problem**: `options(nomem)` on the `out dx, al` inline asm told the
  compiler the write has no memory effect, so it could optimize away
  consecutive PIC writes.
- **Fix**: Removed `nomem` from `port_out`/`port_in` asm options. The
  compiler now treats each I/O port write as a memory operation.

(Previous decisions D01–D17 from Rounds 1-2 are unchanged.)
