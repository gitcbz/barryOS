#!/usr/bin/env python3
"""Write the kernel's byte size into a copy of stage2's header.

stage2 streams the kernel to its final address in batches instead of reading a
fixed number of sectors, so it has to be told how many bytes there are.  The
Makefile patches the value in at offset 4 (little-endian u32); if the header is
left at zero the loader refuses to run rather than guess.

It writes to a *separate* output file on purpose.  `build/stage2.bin` has to
stay pristine, because it is also embedded into the kernel image for the
installer (see gen-bootimg.py).  Patching it in place would change the embedded
bytes on every build, which would rebuild the kernel on every build, forever.

Layout at the start of a stage2 image:
    0..1   jmp short real_start
    2..3   padding
    4..7   kernel size in bytes   <- patched here
    8..    real_start
"""
import os
import struct
import sys


def main():
    if len(sys.argv) != 4:
        sys.exit("usage: patch-stage2.py <stage2.bin> <kernel.bin> <out.bin>")

    stage2, kernel, out = sys.argv[1], sys.argv[2], sys.argv[3]

    with open(stage2, "rb") as f:
        image = bytearray(f.read())
    if len(image) < 8 or image[0] != 0xEB:
        sys.exit("patch-stage2: %s does not look like a stage2 image" % stage2)

    size = os.path.getsize(kernel)
    # u32: absurd for any kernel we could load, and it catches a build that
    # somehow produced a 4 GiB file.
    if size <= 0 or size >= 0xFFFF_FFFF:
        sys.exit("patch-stage2: implausible kernel size %d" % size)

    struct.pack_into("<I", image, 4, size)
    with open(out, "wb") as f:
        f.write(image)

    print("[bios] stage2 patched: kernel %d bytes (%d sectors)"
          % (size, (size + 511) // 512))


if __name__ == "__main__":
    main()
