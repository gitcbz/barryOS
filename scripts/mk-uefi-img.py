#!/usr/bin/env python3
"""Build an MBR-partitioned UEFI disk image for barryOS using mkfs.fat.

Layout (4 MiB / 8192 sectors of 512 B):
  LBA 0           : MBR (partition 1 = ESP, type 0xEF, bootable)
  LBA 2048..8191  : partition 1 = EFI System Partition (FAT32 via mkfs.fat)

mkfs.fat (dosfstools) produces a strictly-correct FAT32 that OVMF accepts,
unlike mtools which leaves an inconsistent BPB.

Usage: mk-uefi-img.py <out.img> <BOOTX64.EFI> <kernel.bin>
"""
import struct, subprocess, sys, os

IMG_SECTORS  = 32768               # 16 MiB image
PART_LBA     = 2048
PART_SECTORS = IMG_SECTORS - PART_LBA     # 30720 sectors = 15 MiB
PART_OFFSET  = PART_LBA * 512

# Tool locations.  The Makefile passes these through from scripts/env.mk so the
# same script works in the Linux sandbox and on the Windows/msys2 host.
MKFS_FAT     = os.environ.get("MKFS_FAT", "/home/z/.opt/usr/sbin/mkfs.fat")
MMD          = os.environ.get("MMD", "mmd")
MCOPY        = os.environ.get("MCOPY", "mcopy")

def run(cmd):
    r = subprocess.run(cmd, capture_output=True, text=True)
    if r.returncode != 0:
        sys.stderr.write(f"FAIL: {' '.join(cmd)}\n{r.stderr}\n")
        sys.exit(r.returncode)

def main():
    if len(sys.argv) != 4:
        sys.exit("usage: mk-uefi-img.py <out.img> <BOOTX64.EFI> <kernel.bin>")
    out_img, efi_app, kern_bin = sys.argv[1:]

    # 1. standalone FAT16 image sized to the partition.
    #    FAT16 is valid for 4085-65524 clusters; with 1 sec/cluster our
    #    6144-sector partition yields 6144 clusters -> solid FAT16 range.
    #    OVMF's FAT driver accepts FAT12/16/32; FAT16 here avoids the
    #    "too few clusters" invalid-FAT32 trap.
    # Scratch FAT image.  Kept next to the output rather than in /tmp so it
    # works on Windows too (there is no /tmp there); removed at the end.
    fat_img = out_img + ".esp.fat"
    with open(fat_img, "wb") as f:
        f.truncate(PART_SECTORS * 512)
    run([MKFS_FAT, "-F", "16", "-n", "BARRYOS", fat_img])
    run([MMD,   "-i", fat_img, "::/EFI"])
    run([MMD,   "-i", fat_img, "::/EFI/BOOT"])
    run([MCOPY, "-i", fat_img, efi_app,  "::/EFI/BOOT/BOOTX64.EFI"])
    run([MCOPY, "-i", fat_img, kern_bin, "::/kernel.bin"])

    # 2. zeroed disk image
    with open(out_img, "wb") as f:
        f.truncate(IMG_SECTORS * 512)

    # 3. MBR with partition 1 = ESP (type 0xEF, bootable)
    mbr = bytearray(512)
    struct.pack_into("<B3sB3sII", mbr, 446,
                     0x80, b"\xFE\xFF\xFF", 0xEF, b"\xFE\xFF\xFF",
                     PART_LBA, PART_SECTORS)
    mbr[510] = 0x55; mbr[511] = 0xAA
    with open(out_img, "r+b") as f:
        f.seek(0); f.write(mbr)

    # 4. dd the FAT image into the partition area
    with open(fat_img, "rb") as src, open(out_img, "r+b") as dst:
        dst.seek(PART_OFFSET)
        while True:
            buf = src.read(1 << 20)
            if not buf:
                break
            dst.write(buf)
    os.unlink(fat_img)

    print(f"[uefi] {out_img} ready (MBR + ESP @ LBA {PART_LBA}, "
          f"{PART_SECTORS} sectors, FAT32 via mkfs.fat)")

if __name__ == "__main__":
    main()
