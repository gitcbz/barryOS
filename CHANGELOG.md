# barryOS — CHANGELOG

Reverse-chronological. Each entry: date, round, stage, change, verification.

## 2026-09-21 — Round 1 — Stage 0 + Stage 1 — ✅ COMPLETE
- Created project tree under `/home/z/my-project/barryOS/`.
- Installed full toolchain rootlessly (see BOOTSTRAP.md, DECISIONS D09):
  Rust nightly 1.100.0, NASM 2.16.01, QEMU 10.0.13, xorriso 1.5.6,
  mtools 4.0.48, mkfs.fat 4.2, clang-19 19.1.7, Fedora OVMF (FatDxe).
- Authored state files: STATUS/TODO/CHANGELOG/DECISIONS/ASSUMPTION/BOOTSTRAP/BLOCKERS.
- Authored docs/ARCHITECTURE.md (full system architecture).
- Implemented Stage 1 dual-boot MVP:
  - boot/bios/mbr.asm — 512 B MBR, magic 0x55AA, loads stage2 from LBA 1.
  - boot/bios/stage2.asm — real mode → A20 → GDT → protected mode →
    PAE paging → long mode → jump to kernel @ 0x100000.
  - boot/uefi/efi_main.c + efi_types.h + efi.ld — self-developed PE32+ EFI
    app: LocateProtocol(SimpleFS), read kernel.bin, GOP framebuffer,
    memmap, ExitBootServices, memcpy kernel → 0x100000, jmp with RDI=&BootInfo.
    Verified struct layouts against EDK2 UefiSpec.h.
  - kernel/Cargo.toml, kernel/.cargo/config.toml, kernel/linker.ld,
    kernel/src/{main.rs,lib.rs(removed),vga.rs,serial.rs,panic.rs,bootinfo.rs}
    — no_std kernel printing "barryOS booted" to VGA + serial.
  - Makefile: builds kernel ELF + flat bin, BIOS img, UEFI img, hybrid ISO.
  - scripts/check.sh: L0-L3 self-verification (make, artifact, BIOS QEMU,
    UEFI QEMU, asserts "barryOS booted" on serial).
  - scripts/mk-uefi-img.py: MBR + FAT16 ESP builder (mkfs.fat + mtools).
  - .github/workflows/ci.yml + ci/Dockerfile: reproducible CI.
- FAIL → root cause → fix log:
  1. No sudo → apt .deb extract trick.
  2. QEMU missing SeaBIOS → unified firmware dir + `-L`.
  3. Debian OVMF lacks FatDxe → Fedora edk2-ovmf RPM (zstd payload parsed in Python).
  4. mtools FAT32 BPB invalid (total_sectors_16 set, _32=0) → mkfs.fat -F 16.
  5. EFI PE "Unsupported"/"Load Error" with gcc → clang -target x86_64-unknown-windows
     (MS-ABI code) + GNU ld.  gcc's non-PIC PE has runtime pseudo-relocs OVMF rejects.
  6. EFI app at 0x100000 collided with kernel load addr → moved to 0x1000000.
  7. AllocateAddress @0x100000 = EFI_NOT_FOUND → AllocateAnyPages + memcpy after exit.
  8. GOP struct missing QueryMode/SetMode/Blt → added; Mode now at offset 24.
- Verification: `bash scripts/check.sh` → **PASS=16 FAIL=0 SKIP=0**.
  Both BIOS and UEFI QEMU boots print "barryOS booted" on serial.
  See CHECK_REPORT.md for exact commands, exit codes, and serial logs.
