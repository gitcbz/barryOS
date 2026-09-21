# barryOS — DECISIONS LOG

Every autonomous decision is recorded here with rationale so the work is
auditable and reversible. Format: `[Dxx] Title — decision — reason — date`.

## [D01] Kernel load address = 0x00100000 (1 MiB), identity-mapped — 2026-09-21
- Decision: Kernel is a flat binary loaded at physical 0x100000, identity-mapped
  for Stage 1. Entry point at the very start of the image.
- Reason: Simplest reliable MVP. BIOS bootloader loads a flat binary (no ELF
  parsing in 16-bit real mode). UEFI app also loads the flat binary to 0x100000.
  Both paths converge on the same `_start`.
- Higher-half remap deferred to Stage 2 (paging subsystem) per the stage plan.

## [D02] Kernel image format = flat binary on disk, ELF kept for inspection — 2026-09-21
- Decision: Build kernel as ELF (for sections/symbols/debug), then `objcopy -O binary`
  to produce kernel.bin which is what the bootloader places on disk & loads.
- Reason: Avoids ELF program-header parsing in the bootloader. The ELF remains
  in `build/` for readelf/objdump inspection. ELF-in-bootloader loading is a
  Stage 5 enhancement.

## [D03] BIOS disk layout — 2026-09-21
- LBA 0         : MBR (512 B, ends 0x55AA) — loads stage2
- LBA 1..31     : stage2.bin (16 KiB max, loaded to 0x7E00)
- LBA 32..N     : kernel.bin (loaded to 0x100000)
- LBA N+1..     : (reserved for filesystem in Stage 5)
- Reason: Fixed offsets make the bootloader trivial; 16 KiB is plenty for a
  real-mode→long-mode stage 2.

## [D04] UEFI boot path: self-written EFI app in C, hand-rolled EFI types — 2026-09-21
- Decision: Write the UEFI bootloader as a freestanding C program that calls
  EFI Boot Services directly, using EFI struct definitions we write ourselves
  (verified against EDK2 `UefiSpec.h`).  Compile with
  `clang-19 -target x86_64-unknown-windows` (MS-ABI) and link to PE32+ with
  GNU `ld -m i386pep --subsystem 10`.
- Reason: Fully self-developed (no uefi-rs dependency). clang's windows target
  emits MS-ABI code that OVMF's PE loader accepts; gcc's non-PIC PE produced
  runtime pseudo-relocations OVMF rejected ("Unsupported"/"Load Error").
  UEFI already runs in 64-bit long mode, so the EFI app only needs:
  LocateProtocol(SimpleFS), read kernel.bin, GOP framebuffer, memmap,
  ExitBootServices, memcpy kernel → 0x100000, jmp with RDI=&BootInfo.
- Resolution: Stage 1 UEFI boots to "barryOS booted" (L3 PASS).

## [D04b] OVMF firmware: Fedora build (FatDxe included) — 2026-09-21
- The Debian `ovmf` package ships a minimal OVMF without the FAT filesystem
  driver (FatDxe). OVMF booted to the shell but never created an `fs0:`
  mapping, so no EFI app could be loaded.
- Fix: extracted Fedora's `edk2-ovmf` RPM (parsed zstd payload in Python,
  no rpm2cpio deps), which includes FatDxe. Installed as the default OVMF.
- CI note: ubuntu-latest runners ship the full OVMF with FatDxe, so this
  sandbox-specific substitution is not needed there.

## [D05] Unified kernel entry contract — 2026-09-21
- Both boot paths jump to `0x00100000` (= `_start`) in 64-bit long mode with a
  valid stack (RSP set by the loader) and interrupts disabled.
- ABI: RDI = pointer to a BootInfo struct (UEFI provides it; BIOS passes 0).
- For Stage 1 the kernel ignores BootInfo and just prints "barryOS booted".
- Reason: Lets us unify the entry now and adopt BootInfo in Stage 2 without
  touching the boot path again.

## [D06] Output drivers: VGA text (0xB8000) + serial COM1 (0x3F8) — 2026-09-21
- Stage 1 prints to both. VGA gives a visible screen; serial gives
  script-checkable output (`-serial stdio` in QEMU → assert on string).
- Reason: QEMU assertion needs deterministic text; VGA alone is not assertable
  headlessly without screen scraping.

## [D07] Output string for Stage 1 acceptance = `barryOS booted` — 2026-09-21
- Exact ASCII string printed once near the start of `kernel_main`.
- Assertion: serial log must contain this substring; boot is otherwise silent.

## [D08] No higher-half, no ASLR, no SMP in Stage 1 — 2026-09-21
- Single CPU, identity-mapped low MiB, no randomization. Stage 4+ adds SMP.

## [D09] Toolchain install without sudo — 2026-09-21
- Used `apt-get download` + `dpkg-deb -x` to install QEMU/xorriso/mtools/OVMF
  into `~/.opt` (no root). Built NASM from source to `~/.local/bin`.
  Rust via rustup to `~/.cargo`.
- Reason: Sandbox has no passwordless sudo. This keeps the env reproducible and
  is mirrored in ci/Dockerfile for CI (which DOES run as root and uses apt-get).

## [D10] ISO format = hybrid El Torito + ISO9660, built with xorriso — 2026-09-21
- BIOS El Torito boot image = barryOS-bios.img (MBR + stage2 + kernel).
- UEFI El Torito boot image = a FAT32 ESP containing /EFI/BOOT/BOOTX64.EFI
  and /kernel.bin.
- xorriso `-eltorito-boot` + `-efi-boot` + `-append_partition` for the
  hybrid MBR so the same ISO boots BIOS and UEFI.
