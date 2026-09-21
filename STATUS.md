# barryOS — STATUS

**Current round:** Round 1 COMPLETE — Stage 0 + Stage 1 (Dual-Boot MVP) ✅
**Last updated:** 2026-09-21 06:01 (Asia/Shanghai)
**Mode:** AUTONOMOUS (no human confirmation; self-verify with QEMU)

## Verification result (this round)
**PASS=16  FAIL=0  SKIP=0** — see CHECK_REPORT.md

| Gate | Result |
|------|--------|
| L0  make all                            | ✅ PASS |
| L1  8 artifacts exist + format-correct | ✅ PASS (8/8) |
| L1b MBR magic 0x55AA                    | ✅ PASS |
| L1c kernel ELF x86-64                   | ✅ PASS |
| L1d BOOTX64.EFI = PE32+ EFI app         | ✅ PASS |
| L1e stage2 = 15872 bytes                | ✅ PASS |
| L2  BIOS QEMU → "barryOS booted"        | ✅ PASS |
| L3  UEFI QEMU → "barryOS booted"        | ✅ PASS |

Both boot paths converge on the same 64-bit kernel entry at 0x100000 and
print `barryOS booted` to COM1 serial + VGA text. Stage 1 is done.

## Environment
- Host: Linux 5.10 x86_64, user `z`, workdir `/home/z/my-project`
- Toolchain (all rootless, see BOOTSTRAP.md):
  - rustc 1.100.0-nightly (x86_64-unknown-none + llvm-tools + rust-src)
  - nasm 2.16.01 (built from source → ~/.local/bin)
  - clang-19 19.1.7 + gcc 14.2.0 + ld 2.44
  - qemu-system-x86_64 10.0.13, xorriso 1.5.6, mtools 4.0.48, mkfs.fat 4.2
  - OVMF: Fedora edk2-ovmf 20240813 (includes FatDxe — Debian's does NOT)
  - SeaBIOS: bundled with qemu, exposed via unified firmware dir

## Key fixes this round
1. Rootless toolchain install (apt .deb extract + source builds).
2. BIOS chain: MBR(512B) → stage2(16K) → long mode → kernel @ 0x100000.
3. UEFI loader in C with hand-written EFI types (verified against EDK2 UefiSpec.h).
4. clang `-target x86_64-unknown-windows` produces MS-ABI PE OVMF accepts.
5. EFI app loads kernel.bin via LocateProtocol(SimpleFS), allocates buffer,
   ExitBootServices, memcpy to 0x100000, jumps with RDI=&BootInfo.
6. FAT16 ESP (16 MiB) — OVMF binds its FatDxe driver reliably.
7. Hybrid ISO: El Torito BIOS entry + UEFI boot image + appended partition.

## Next round (Stage 2 — Memory Management)
- [ ] Physical page frame allocator (bitmap) over UEFI memmap / E820.
- [ ] x86_64 4-level page tables, higher-half kernel remap.
- [ ] Buddy/slab heap allocator with `#[global_allocator]`.
- [ ] Consume BootInfo (framebuffer + memmap) in kernel.

## Gates status
- L0 make ok — ✅ PASS
- L1 artifacts exist + format-correct — ✅ PASS
- L2 BIOS QEMU prints "barryOS booted" — ✅ PASS
- L3 UEFI QEMU prints "barryOS booted" — ✅ PASS
- L4 kernel unit tests — deferred (Stage 2+)
- L5 userland hello world — deferred (Stage 4)
- L6 fs read/write — deferred (Stage 5)
- L7 graphics frame — deferred (Stage 6)
- L8 compat layer samples — deferred (Stage 9-10)
- L9 VMware BIOS/UEFI boot to desktop — VMware-PENDING (Stage 7+)
