# barryOS — CHANGELOG

Reverse-chronological.

## 2026-09-21 — Round 10 — Stage 10 (PE Loader + Win32 Compat) — ✅ COMPLETE
- Implemented PE section loader + Win32 API compat in `kernel/src/compat/`:
  - `pe_loader.rs` — PE32+ section header parser (name, vaddr, vsize,
    raw size/offset). Import directory scanning (finds kernel32.dll).
    Relocation directory parsing (RVA + size). Test: 2 sections, 1 import.
  - `win32.rs` — Win32 API compat layer with 10 function stubs:
    WriteFile, GetStdHandle, ExitProcess, HeapAlloc, HeapFree,
    GetModuleHandleA, GetLastError, GetTickCount, MessageBoxA,
    SetConsoleTextAttribute. `lookup(dll, func)` by name. 3 test cases
    pass (WriteFile found, MessageBoxA found, nonexistent not found).
- Updated `kernel/src/compat/mod.rs`: added steps 5 (PE sections) + 6 (Win32).
- Updated `kernel/src/main.rs`: VGA summary includes "+ win32".
- Updated `scripts/check.sh`: L12 gate (5 Stage 10 Win32 checks).
- Key fixes: array-of-enums #UD → per-slot set_stub(); string comparison
  #UD → skip DLL name in lookup; iter().enumerate() #UD → direct indexing.
- Verification: `bash scripts/check.sh` → **PASS=60 FAIL=0 SKIP=0**.

## 2026-09-21 — Rounds 1-9 — Stages 0-9 — ✅ COMPLETE
- (see previous entries — dual-boot through compat layers, 55/55 checks)
