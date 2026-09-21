# barryOS — CHANGELOG

Reverse-chronological.

## 2026-09-21 — Round 7 — Stage 7 (Window Manager + GUI) — ✅ COMPLETE
- Implemented window manager + GUI in `kernel/src/wm/`:
  - `font.rs` — 8x16 bitmap font (95 ASCII glyphs). `draw_char`,
    `draw_str`, `draw_str_bg`. 16 bytes per glyph, MSB = leftmost pixel.
  - `window.rs` — Window struct + 16-slot table. `create(x,y,w,h,title)`,
    `render_window` (shadow + body + emerald title bar + border + close
    button + title text). `render_all`. Raw pointer access (no `&mut` UB).
  - `widgets.rs` — `draw_button` (border + centered text), `draw_label`,
    `draw_dock` (bottom bar with emerald icon buttons), `draw_status_bar`
    (top bar with "barryOS" + "Stage 7").
  - `desktop.rs` — `create_desktop` creates 2 windows (Terminal + Files).
    `render` draws: background + status bar + 2 windows with content +
    dock bar.
- Updated `kernel/src/main.rs`: `mod wm`, calls `wm::init()`.
- Updated `scripts/check.sh`: L9 gate (4 Stage 7 WM checks).
- Dashboard updated: Stage 7 WM section with desktop preview mockup,
  window table, bitmap font visualization, widget gallery, compositor
  pipeline, 10 sub-components.
- Screenshot: QEMU screendump 720×400 PPM (download/barryos-screen-stage7.ppm).
- Verification: `bash scripts/check.sh` → **PASS=44 FAIL=0 SKIP=0**.
  BIOS boots to WM online + bitmap font + 2 windows + desktop rendered.

## 2026-09-21 — Rounds 1-6 — Stages 0-6 — ✅ COMPLETE
- (see previous entries — dual-boot MVP through device drivers, 40/40 checks)
