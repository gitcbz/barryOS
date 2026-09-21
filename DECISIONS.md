# barryOS — DECISIONS LOG

## [D11] EFI memory types — self-developed enum matching UEFI spec — 2026-09-21
- `MemoryType` enum in `mem/efi.rs` reproduces UEFI 2.10 spec constants.
- `is_usable()` returns true only for `ConventionalMemory` (free RAM).
- `EfiMemoryDescriptor` is `#[repr(C)]` matching the spec (40 bytes).

## [D12] Static MemMap + parse_into (avoid struct return-by-value) — 2026-09-21
- **Problem**: `memmap::parse() -> MemMap` hung in the BIOS boot path.
  The MemMap (768+ bytes) was returned by value (sret), which triggered
  a hang — likely a code generation issue with large struct returns in
  no_std kernel mode.
- **Fix**: Made MemMap a `static mut BI` in `mem/mod.rs`. Changed
  `parse()` to `parse_into(boot_info, &mut MemMap)`. The parse function
  writes directly into the static via a mutable reference.
- **Trade-off**: Stage 2 is single-threaded (no preemption), so a static
  is safe. Stage 4+ (processes) will need per-address-space memmaps.

## [D13] Raw pointer access for static mut (Rust 2024 safety) — 2026-09-21
- **Problem**: `BITMAP.as_mut_ptr()` and `&PML4` on `static mut` create
  temporary references, which is UB in Rust 2024.
- **Fix**: Use `core::ptr::addr_of_mut!(BITMAP)` and `addr_of!(PML4)` to
  get raw pointers without creating references. All reads/writes via
  `read_volatile`/`write_volatile`. This is the recommended Rust 2024
  pattern for `static mut` access.

## [D14] Bump allocator for Stage 2 (free-list deferred) — 2026-09-21
- **Problem**: Free-list allocator's `dealloc` caused Vec reallocation
  to hang (likely `__rust_dealloc` linkage issue with compiler-builtins).
- **Fix**: Bump allocator (alloc bumps pointer, dealloc is no-op).
  Uses `AtomicU64` for state (no `UnsafeCell`/`static mut` issues).
  Enough for Box, Vec::with_capacity, raw alloc. Vec reallocation
  (grow_amortized) is deferred to Stage 2b.
- **Trade-off**: Bump allocator never frees memory. For Stage 2 testing
  this is fine. Stage 2b will implement a proper free-list or slab.

## [D15] Bitmap reduced from 128 KiB to 8 KiB — 2026-09-21
- 128 KiB bitmap (4 GiB coverage) worked but was excessive for QEMU's
  default 128 MiB. Reduced to 8 KiB (256 MiB coverage) to keep .bss
  small and avoid potential large-allocation issues.
- Can be increased for VMware VMs with more RAM.

## [D16] Identity mapping with 1 GiB pages (not higher-half) — 2026-09-21
- **Decision**: Stage 2 uses 1 GiB pages for identity mapping (0..4 GiB),
  NOT higher-half remap. PML4[0] and PML4[511] both point to PDPT0.
- **Reason**: Higher-half remap requires changing the linker script LMA,
  code model, and carefully handling the address space switch. Too risky
  for Stage 2 — would break the working boot. Identity mapping is safe.
- **Stage 2b**: Will attempt higher-half (link at 0xFFFFFFFF80100000,
  map to physical 0x100000, jump to high address, unmap identity).

## [D17] BIOS fallback memory map (no E820 yet) — 2026-09-21
- BIOS stage2 bootloader doesn't pass E820. Kernel synthesizes:
  - Region 0: [1 MiB, 2 MiB) as LoaderData (kernel image, not allocatable).
  - Region 1: [2 MiB, 64 MiB) as ConventionalMemory (free RAM).
- This is a simplification — real E820 querying in stage2 is Stage 2b.
- UEFI path uses the real UEFI memory map (20+ regions).

(Previous decisions D01–D10 from Round 1 are unchanged.)
