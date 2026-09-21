# barryOS — CHANGELOG

Reverse-chronological.

## 2026-09-21 — Round 5 — Stage 5 (VFS + Filesystem) — ✅ COMPLETE
- Implemented VFS + RAM filesystem in `kernel/src/fs/`:
  - `vfs.rs` — Vnode abstraction (64-slot table, raw pointer access).
    `create_file`, `create_dir`, `lookup` (byte-by-byte comparison),
    `print_table`, `ls_root`. NEXT_ID starts at 2 (root=1).
  - `ramfs.rs` — In-memory RAM filesystem (8 KiB data pool).
    `alloc_data`, `write_file`, `read_file` (volatile writes/reads).
    `create_test_files` creates 4 test files (motd, hello, version, hostname).
  - `file.rs` — File handle operations. `open(path)`→fd, `read(fd,buf)`→bytes,
    `close(fd)`. 16-slot handle table. Smoke test: open("/hello")→fd=0,
    read 45 bytes. open("/nonexistent")→-1.
- Updated `boot/bios/stage2.asm`: increased kernel load from 128 to 256
  sectors using 4-chunk int 13h reads (64 sectors each) via NASM macro.
  kernel.bin grew to ~84 KiB with fs module, exceeding old 64 KiB limit.
- Updated `kernel/src/main.rs`: `mod fs`, calls `fs::init()`.
- Updated `scripts/check.sh`: L7 gate (5 Stage 5 filesystem checks).
- Dashboard updated: Stage 5 Filesystem Subsystem section with vnode table,
  root directory browser, file operations flow, RAMfs data pool,
  VFS architecture diagram, file read test results, 11 sub-components.
- FAIL → root cause → fix log:
  1. kernel.bin > 64 KiB stage2 limit → 4-chunk 256-sector BIOS reads.
  2. `&mut` reference UB on static VNODES → raw pointer access throughout.
  3. `slice == slice` (memcmp) caused #UD → byte-by-byte volatile comparison.
  4. Vnode ID collision (root + file both id=1) → NEXT_ID starts at 2.
  5. `#[derive(Clone, Copy)]` needed → added to Vnode + VnodeType.
  6. `implicit autoref` on raw pointer deref → `from_raw_parts` + volatile.
- Verification: `bash scripts/check.sh` → **PASS=36 FAIL=0 SKIP=0**.
  BIOS boots to filesystem subsystem online + VFS initialized (40 slots)
  + RAM filesystem (8 KiB) + 4 test files + root directory listing
  + file read test (open+read 45 bytes).

## 2026-09-21 — Round 4 — Stage 4 (Processes & Syscalls) — ✅ COMPLETE
- (see previous entry — process subsystem, 31/31 checks)

## 2026-09-21 — Round 3 — Stage 3 (Interrupts & Exceptions) — ✅ COMPLETE
- (see previous entry — interrupt subsystem, 26/26 checks)

## 2026-09-21 — Round 2 — Stage 2 (Memory Management) — ✅ COMPLETE
- (see previous entry — memory subsystem, 21/21 checks)

## 2026-09-21 — Round 1 — Stage 0 + Stage 1 — ✅ COMPLETE
- (see previous entry — dual-boot MVP, 16/16 checks)
