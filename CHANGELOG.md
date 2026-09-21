# barryOS — CHANGELOG

Reverse-chronological.

## 2026-09-21 — Round 9 — Stage 9 (Compatibility Layers) — ✅ COMPLETE
- Implemented compatibility layer parsers in `kernel/src/compat/`:
  - `deb.rs` — .deb parser (ar archive format). Reads AR magic,
    iterates 60-byte headers, extracts entry names + sizes.
  - `rpm.rs` — .rpm parser (RPM v3 lead). Reads magic, version,
    type, archnum, 66-byte name.
  - `appimage.rs` — .AppImage Type 2 detector. Checks ELF + AppImage
    magic at offset 8, extracts payload offset.
  - `pe.rs` — PE32+ header parser. Reads DOS header, e_lfanew, PE sig,
    COFF header (machine, sections), optional header (entry, image base).
- All byte comparisons use byte-by-byte (memcmp causes #UD in no_std).
- Updated `kernel/src/main.rs`: `mod compat`, calls `compat::init()`.
- Updated `scripts/check.sh`: L11 gate (6 Stage 9 compat checks).
- Verification: `bash scripts/check.sh` → **PASS=55 FAIL=0 SKIP=0**.
  BIOS boots to compat layers online + 4 parsers initialized + 3 packages
  parsed (rpm + appimage + pe).

## 2026-09-21 — Rounds 1-8 — Stages 0-8 — ✅ COMPLETE
- (see previous entries — dual-boot through desktop apps, 49/49 checks)
