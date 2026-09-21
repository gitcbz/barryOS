# barryOS — CHANGELOG

Reverse-chronological.

## 2026-09-21 — Round 2 — Stage 2 (Memory Management) — ✅ COMPLETE
- Implemented full memory subsystem in `kernel/src/mem/`:
  - `efi.rs` — MemoryType enum + EfiMemoryDescriptor (UEFI spec constants).
  - `memmap.rs` — parse UEFI memmap from BootInfo, or BIOS fallback (2 regions).
    Uses `parse_into(&mut MemMap)` to avoid large struct return-by-value hang.
    Static MemMap in `mem/mod.rs` (avoids sret issue in BIOS path).
  - `frame_alloc.rs` — bitmap physical frame allocator (8 KiB bitmap, 256 MiB).
    Raw pointer access via `addr_of_mut!` (avoids Rust 2024 `static mut` UB).
    Supports `alloc()`, `alloc_contig(n)`, `free()`.
  - `paging.rs` — x86_64 4-level page tables. Kernel owns PML4+PDPT (statics).
    CR3 switched from bootloader's tables (0x70000) to ours (0x106000).
    4 GiB identity-mapped with 1 GiB pages. PML4[0]+[511] → PDPT0.
  - `heap.rs` — bump allocator registered as `#[global_allocator]`.
    1 MiB heap (256 contiguous frames). AtomicU64 state (no UnsafeCell issues).
    Supports raw alloc, Box, Vec::with_capacity, copy_nonoverlapping.
    Vec reallocation (grow_amortized) deferred to Stage 2b.
- Updated `kernel/src/main.rs`: `extern crate alloc`, `mod mem`, calls `mem::init()`.
- Updated `kernel/.cargo/config.toml`: `build-std` now includes `"alloc"`.
- Updated `Makefile`: kernel target depends on `find src -name '*.rs'` (was `*.rs`).
- Updated `scripts/check.sh`: added L4 gate (5 Stage 2 memory checks).
- Dashboard updated: Stage 2 Memory Management section with memmap viz,
  frame allocator stats, CR3 switch, page table hierarchy, heap smoke tests.
- FAIL → root cause → fix log:
  1. `memmap::parse()` return-by-value hung in BIOS path → static MemMap + `parse_into()`.
  2. `static mut BITMAP` reference UB (Rust 2024) → raw pointer via `addr_of_mut!`.
  3. `static mut PML4` assignment UB → `static` + `addr_of!` + volatile writes.
  4. Free-list allocator's `dealloc` caused Vec reallocation hang → bump allocator.
  5. `#[global_allocator]` on `static mut` → changed to plain `static` with atomics.
  6. Makefile didn't track `src/mem/*.rs` → `find` instead of `wildcard`.
  7. Vec reallocation still hangs → `__rust_dealloc` linkage issue, deferred to 2b.
- Verification: `bash scripts/check.sh` → **PASS=21 FAIL=0 SKIP=0**.
  Both BIOS and UEFI boot to "barryOS booted" + "memory subsystem online".

## 2026-09-21 — Round 1 — Stage 0 + Stage 1 — ✅ COMPLETE
- (see previous entry — dual-boot MVP, 16/16 checks)
