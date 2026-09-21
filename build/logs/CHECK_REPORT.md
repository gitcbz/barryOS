# barryOS — CHECK_REPORT

**Generated:** 2026-09-21 06:00 (Asia/Shanghai)
**Round:** 1 — Stage 0 + Stage 1 (Dual-Boot MVP)
**Mode:** Autonomous, rootless sandbox (no sudo)

## 1. Environment
| Tool      | Version          | Source                  | Verified |
|-----------|------------------|-------------------------|----------|
| rustc     | 1.100.0-nightly  | rustup (no sudo)        | ✓        |
| nasm      | 2.16.01          | built from source       | ✓        |
| qemu      | 10.0.13          | apt .deb extract         | ✓        |
| xorriso   | 1.5.6            | apt .deb extract         | ✓        |
| mtools    | 4.0.48           | apt .deb extract         | ✓        |
| mkfs.fat  | 4.2              | dosfstools .deb          | ✓        |
| gcc       | 14.2.0           | system                   | ✓        |
| clang-19  | 19.1.7           | apt .deb extract         | ✓        |
| ld (GNU)  | 2.44             | system                   | ✓        |
| OVMF      | Fedora edk2 20240813 | RPM extract (has FatDxe) | ✓     |
| SeaBIOS   | (qemu bundled)  | firmware dir             | ✓        |

## 2. Verification gates

| Gate | Description                          | Result |
|------|--------------------------------------|--------|
| L0   | `make all` exit 0                    | **PASS** |
| L1   | 8 artifacts exist + format-correct   | **PASS** (8/8) |
| L1b  | MBR magic 0x55AA at offset 510       | **PASS** |
| L1c  | kernel ELF machine = x86-64          | **PASS** |
| L1d  | BOOTX64.EFI = PE32+ EFI app          | **PASS** |
| L1e  | stage2 size = 15872 bytes (31 sec)  | **PASS** |
| L2   | BIOS QEMU boot → "barryOS booted"   | **PASS** |
| L3   | UEFI QEMU boot → "barryOS booted"   | **PASS** |

**Summary: PASS=16  FAIL=0  SKIP=0**

## 3. Commands executed (exact)

```
# toolchain
source /home/z/my-project/barryOS/scripts/env.sh
export PATH=/home/z/.opt/usr/bin:/home/z/.opt/usr/sbin:$PATH

# build
make clean && make all                      # exit 0

# static checks
od -An -tx1 -j 510 -N 2 build/mbr.bin       # → 55 aa
readelf -h build/kernel.elf | grep Machine  # → Advanced Micro Devices X86-64
file build/BOOTX64.EFI                      # → PE32+ executable for EFI
stat -c%s build/stage2.bin                  # → 15872

# BIOS boot
qemu-system-x86_64 -L $QEMU_BIOS_DIR \
  -drive format=raw,file=build/barryOS-bios.img \
  -serial stdio -display none -no-reboot
# → serial: "barryOS booted"

# UEFI boot
cp $OVMF_VARS build/v.fd
qemu-system-x86_64 -L $QEMU_BIOS_DIR \
  -drive if=pflash,format=raw,readonly=on,file=$OVMF_CODE \
  -drive if=pflash,format=raw,file=build/v.fd \
  -drive format=raw,file=build/barryOS-uefi.img \
  -serial stdio -display none -no-reboot
# → serial: "barryOS booted"

# full harness
bash scripts/check.sh                       # exit 0, PASS=16 FAIL=0
```

## 4. Key serial log excerpts

### BIOS boot (build/logs/qemu-bios.log)
```
========================================
  barryOS - self-developed x86_64 kernel
========================================
[boot] path: BIOS
[boot] kernel entry @ 0x100000
barryOS booted
[ok] Stage 1 complete; halting.
```

### UEFI boot (build/logs/qemu-uefi.log)
```
[barryOS] UEFI loader starting
[barryOS] kernel.bin size: 4584 bytes
[barryOS] kernel read into buffer, will copy to 0x100000 after exit
========================================
  barryOS - self-developed x86_64 kernel
========================================
[boot] path: UEFI
[boot] kernel entry @ 0x100000
barryOS booted
[ok] Stage 1 complete; halting.
```

## 5. Build artifacts
| File                       | Size      | Format                     |
|----------------------------|-----------|----------------------------|
| build/kernel.elf           | 13,816 B  | ELF64 x86-64, entry 0x100000 |
| build/kernel.bin           | 4,584 B   | flat binary @ 0x100000     |
| build/mbr.bin              | 512 B     | MBR, magic 0x55AA          |
| build/stage2.bin           | 15,872 B  | 31 sectors, real→long mode  |
| build/barryOS-bios.img     | 4 MiB     | BIOS boot disk             |
| build/BOOTX64.EFI          | 6,144 B   | PE32+ EFI application      |
| build/barryOS-uefi.img     | 16 MiB    | MBR + FAT16 ESP            |
| build/barryOS.iso          | 38 MiB    | hybrid El Torito (BIOS+UEFI)|

## 6. Issues encountered & resolved (this round)
1. **No sudo in sandbox** → `apt-get download` + `dpkg-deb -x` into `~/.opt`.
2. **NASM missing** → built from source to `~/.local/bin`.
3. **QEMU missing SeaBIOS** → unified firmware dir with symlinks + `-L`.
4. **Debian OVMF lacks FatDxe** → extracted Fedora edk2-ovmf RPM (zstd payload parsed in Python), which includes the FAT driver.
5. **mtools FAT32 BPB invalid** → created ESP with `mkfs.fat -F 16` (valid FAT16 for 16 MiB partition); OVMF binds its FAT driver.
6. **EFI PE "Unsupported"/"Load Error"** → gcc's non-PIC PE had runtime pseudo-relocs OVMF rejects. Switched to `clang-19 -target x86_64-unknown-windows` (MS-ABI code) + GNU `ld -m i386pep --subsystem 10` → OVMF loads and runs the app.
7. **EFI app at 0x100000 conflicted with kernel load addr** → moved EFI app to `--image-base 0x1000000`.
8. **AllocateAddress @0x100000 = EFI_NOT_FOUND** → allocate anywhere, then `memcpy` to 0x100000 after ExitBootServices.
9. **GOP struct missing QueryMode/SetMode/Blt** → added them; `Mode` now at offset 24 per spec.

## 7. Next actions (Stage 2)
- Physical page frame allocator (bitmap over UEFI memmap / E820).
- x86_64 4-level page tables, higher-half kernel remap.
- Buddy/slab heap allocator with `#[global_allocator]`.
- Consume BootInfo (framebuffer + memmap) in kernel.

## 8. VMware acceptance
Marked **VMware-PENDING** (Stage 7+ requires desktop). QEMU BIOS+UEFI
acceptance is the proxy and is **PASS** for Stage 1.
