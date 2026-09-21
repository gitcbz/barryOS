# barryOS — CHANGELOG

Reverse-chronological.

## 2026-09-21 — Round 8 — Stage 8 (Desktop Environment + Apps) — ✅ COMPLETE
- Implemented desktop applications in `kernel/src/apps/`:
  - `terminal.rs` — Terminal with 7 commands (help, ver, ls, mem, ps, echo,
    cat). Renders to framebuffer. Command processing verified via serial.
  - `filemgr.rs` — File manager listing 4 VFS files with icons + sizes.
  - `sysinfo.rs` — System info display (kernel version, memory, processes,
    timer ticks).
  - `mod.rs` — App init sequence: terminal → file manager → system info →
    render all → command test.
- Updated `kernel/src/wm/desktop.rs`: Apps create their own windows;
    desktop only renders background + status bar + dock.
- Updated `kernel/src/main.rs`: `mod apps`, calls `apps::init()`.
- Updated `scripts/check.sh`: L10 gate (5 Stage 8 app checks).
- Screenshot: QEMU screendump 720×400 PPM (download/barryos-screen-stage8.ppm).
- Verification: `bash scripts/check.sh` → **PASS=49 FAIL=0 SKIP=0**.
  BIOS boots to desktop apps online + terminal + file manager + system info
  rendered + all terminal commands processed.

## 2026-09-21 — Rounds 1-7 — Stages 0-7 — ✅ COMPLETE
- (see previous entries — dual-boot through window manager, 44/44 checks)
