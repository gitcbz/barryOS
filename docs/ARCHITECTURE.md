# barryOS — Architecture

```
barryOS  ── self-developed x86_64 kernel (no Linux code, no POSIX base)
 │
 ├── BOOT (two paths → one entry)
 │    │
 │    ├── BIOS path (self-developed, 16-bit → 64-bit)
 │    │     ┌──────────────┐   ┌────────────────┐   ┌──────────────────┐
 │    │     │ MBR (512 B)  │──▶│ Stage 2 (16K)  │──▶│ Kernel @0x100000 │
 │    │     │ LBA0, 0x55AA │   │ real→prot→long │   │  _start          │
 │    │     └──────────────┘   └────────────────┘   └──────────────────┘
 │    │     loads stage2 from   A20+GDT+paging+      flat binary loaded
 │    │     LBA1..31 to 0x7E00  long-mode jump       at physical 1 MiB
 │    │
 │    └── UEFI path (self-developed PE32+ EFI app)
 │          ┌──────────────────┐   ┌──────────────────┐
 │          │ /EFI/BOOT/BOOTX64│──▶│ Kernel @0x100000 │
 │          │  .EFI (C, self-  │   │  _start          │
 │          │  written types)  │   │  RDI=BootInfo*   │
 │          └──────────────────┘   └──────────────────┘
 │          GOP framebuffer +     loads /kernel.bin from ESP to 1 MiB,
 │          memmap → ExitBoot-    jumps in 64-bit long mode (UEFI is
 │          Services → jump       already there)
 │
 ├── KERNEL  (Rust, #![no_std], target x86_64-unknown-none)
 │    ├── entry: _start (asm) → rust_main(RDI: *const BootInfo)
 │    ├── vga.rs      80x25 text framebuffer @ 0xB8000
 │    ├── serial.rs   COM1 @ 0x3F8, 115200 8N1, port I/O
 │    ├── panic.rs    panic handler → serial + VGA + halt
 │    ├── [Stage 2] mem/   frame allocator, 4-level paging, heap
 │    ├── [Stage 3] idt/   IDT, exceptions, PIC, timer
 │    ├── [Stage 4] task/  PCB, scheduler, syscalls, Ring3
 │    ├── [Stage 5] vfs/   VFS, FAT32, block driver
 │    ├── [Stage 6] dev/   kbd, mouse, framebuffer, serial
 │    └── [Stage 7] wm/    compositor, window manager, widgets
 │
 ├── USERLAND
 │    ├── desktop      Hideo-style compositor + Dock + theme engine
 │    ├── apps         file manager, terminal, editor, screenshot, image view
 │    └── compat       .deb / .rpm / .AppImage / .exe (PE+Win32)
 │
 └── BUILD
      ├── Makefile      bios.img, uefi.img, barryOS.iso (hybrid El Torito)
      ├── scripts/env.sh  activates rootless toolchain
      ├── scripts/check.sh L0..L3 self-verification (make + QEMU BIOS + UEFI)
      ├── ci/Dockerfile   reproducible root container
      └── .github/workflows/ci.yml  CI on every push
```

## Memory map (Stage 1)
| Region            | Address          | Use                         |
|-------------------|------------------|-----------------------------|
| IVT / BDA         | 0x00000–0x00500  | BIOS (real mode only)       |
| MBR               | 0x7C00–0x7E00    | loaded by BIOS              |
| stage2            | 0x7E00–0x0BE00   | loaded by MBR                |
| kernel load buf   | 0x10000–0x1FFFF  | real-mode disk load target  |
| page tables       | 0x70000–0x73000  | PML4/PDPT/PD (Stage 1)      |
| kernel stack      | 0x110000–0x200000| grows down from 0x200000    |
| kernel image      | 0x100000–0x110000| flat binary, entry _start   |

## Disk layout (BIOS img)
| LBA        | Content                 |
|------------|-------------------------|
| 0          | MBR (512 B, magic 55AA) |
| 1..31      | stage2.bin (16 KiB)     |
| 32..N      | kernel.bin              |
| N+1..      | (filesystem in Stage 5)  |

## ISO layout (hybrid)
- ISO9660 root
- El Torito BIOS boot entry  → barryOS-bios.img
- El Torito UEFI boot entry  → efiboot.img (FAT, /EFI/BOOT/BOOTX64.EFI + /kernel.bin)
- Partition 1 (MBR) appended = efiboot.img, so BIOS+UEFI boot from same ISO.

## Verification gates
- L0: `make all` exit 0
- L1: build/*.{img,iso,bin,efi} exist + format-correct (file/readelf)
- L2: `qemu ... -serial stdio` BIOS boot log contains `barryOS booted`
- L3: `qemu ... -bios OVMF ...` UEFI boot log contains `barryOS booted`
- L4+: per-stage (tests, fs, graphics, compat, VMware)
