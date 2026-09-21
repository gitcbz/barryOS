# barryOS

A self-developed x86_64 operating system. **No Linux kernel code.** BIOS + UEFI
dual-boot, hand-written bootloader, no_std Rust kernel, planned desktop and
compatibility layers for `.deb` / `.rpm` / `.AppImage` / `.exe`.

## Status
- Stage 0 + Stage 1 (Dual-Boot MVP) — see `STATUS.md`
- Both BIOS and UEFI paths boot to a 64-bit kernel that prints
  `barryOS booted` on the serial port and VGA screen.

## Quick start
```bash
source scripts/env.sh        # activate rootless toolchain (Rust/NASM/QEMU/xorriso/mtools/OVMF)
make all                     # build kernel + BIOS img + UEFI img + hybrid ISO
bash scripts/check.sh        # L0-L3 self-verification (build + QEMU BIOS + QEMU UEFI)
```

## Run in QEMU
```bash
make run-bios                # BIOS boot, serial on stdout
make run-uefi                # UEFI boot, serial on stdout
```

## Build artifacts
| File                       | What                                  |
|----------------------------|---------------------------------------|
| `build/kernel.elf`         | kernel ELF (for readelf/objdump)      |
| `build/kernel.bin`         | flat binary loaded at 0x00100000      |
| `build/mbr.bin`            | 512 B BIOS MBR (magic 0x55AA)         |
| `build/stage2.bin`         | 16 KiB BIOS stage 2                    |
| `build/barryOS-bios.img`   | bootable BIOS disk image              |
| `build/BOOTX64.EFI`        | self-developed PE32+ UEFI loader      |
| `build/barryOS-uefi.img`   | FAT ESP image (BOOTX64.EFI+kernel.bin)|
| `build/barryOS.iso`        | hybrid El Torito ISO (BIOS+UEFI)      |

## VMware Workstation 17 Pro — acceptance (Stage 7+)
1. New VM → Linux 64-bit, 2 GB RAM, 20 GB disk.
2. CD/DVD → Use ISO image → `build/barryOS.iso`.
3. Boot — BIOS mode boots via El Torito BIOS entry; UEFI mode (VM settings →
   Firmware → UEFI) boots via the appended ESP partition.
4. Acceptance: serial console shows `barryOS booted`, VGA screen shows the
   barryOS banner.

## Layout
See `docs/ARCHITECTURE.md` and `STATUS.md` / `TODO.md` / `DECISIONS.md`.

## License
Proprietary — self-developed. No Linux kernel or third-party kernel code.
