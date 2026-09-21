# barryOS — CHANGELOG

Reverse-chronological.

## 2026-09-21 — Round 6 — Stage 6 (Device Drivers) — ✅ COMPLETE
- Implemented device drivers in `kernel/src/dev/`:
  - `framebuffer.rs` — GOP/VBE framebuffer. `put_pixel`, `fill_rect`,
    `draw_test_pattern` (8 color bars + emerald box + dot grid).
    AtomicU64/U32 state (FB_ADDR/WIDTH/HEIGHT/PITCH/BPP). BIOS fallback
    0xE0000000, UEFI uses BootInfo GOP values.
  - `keyboard.rs` — PS/2 keyboard. 59-entry scancode→ASCII map. 256-byte
    line buffer. Enter submits, backspace edits. Echo to serial.
    KEYS_TOTAL counter. Raw pointer volatile access.
  - `mod.rs` — init sequence: framebuffer → keyboard → test pattern.
- Updated `kernel/src/main.rs`: `mod dev`, calls `dev::init(boot_info)`.
- Updated `kernel/src/interrupts/irq.rs`: `handle_keyboard()` now calls
  `dev::keyboard::handle_scancode()` (clean driver separation).
- Updated `scripts/check.sh`: L8 gate (4 Stage 6 device checks).
- Dashboard updated: Stage 6 Device Drivers section with framebuffer
  visualization (color bars + emerald box mockup), color palette,
  pixel API, memory layout, PS/2 keyboard scancode table, line buffer,
  IRQ1 flow, 10 sub-components.
- Screenshot captured: QEMU screendump 720×400 PPM (download/barryos-screen-stage6.ppm).
- Verification: `bash scripts/check.sh` → **PASS=40 FAIL=0 SKIP=0**.
  BIOS boots to device drivers online + framebuffer 640×480×32 @ 0xE0000000
  + PS/2 keyboard initialized + test pattern drawn.

## 2026-09-21 — Round 5 — Stage 5 (VFS + Filesystem) — ✅ COMPLETE
- (see previous entry — VFS + RAM filesystem, 36/36 checks)

## 2026-09-21 — Round 4 — Stage 4 (Processes & Syscalls) — ✅ COMPLETE
- (see previous entry — process subsystem, 31/31 checks)

## 2026-09-21 — Round 3 — Stage 3 (Interrupts & Exceptions) — ✅ COMPLETE
- (see previous entry — interrupt subsystem, 26/26 checks)

## 2026-09-21 — Round 2 — Stage 2 (Memory Management) — ✅ COMPLETE
- (see previous entry — memory subsystem, 21/21 checks)

## 2026-09-21 — Round 1 — Stage 0 + Stage 1 — ✅ COMPLETE
- (see previous entry — dual-boot MVP, 16/16 checks)
