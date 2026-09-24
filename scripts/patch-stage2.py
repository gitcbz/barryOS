#!/usr/bin/env python3
"""Write the kernel's sizes into a copy of stage2's header.

stage2 streams the kernel's compressed image to a staging buffer and then
decompresses it to 0x100000, so it has to be told two numbers: how many bytes
to read, and how many it should end up with.  The Makefile patches both in as
little-endian u32s; if the first is left at zero the loader refuses to run
rather than guess, and if the second is wrong the loader stops rather than
jumping into a half-written kernel.

It writes to a *separate* output file on purpose.  `build/stage2.bin` has to
stay pristine, because it is also embedded into the kernel image for the
installer (see gen-bootimg.py).  Patching it in place would change the embedded
bytes on every build, which would rebuild the kernel on every build, forever.

Layout at the start of a stage2 image:
    0..1   jmp short real_start
    2..3   padding
    4..7   compressed image size  <- patched here
    8..11  decompressed size      <- and here
    12..   real_start
"""
import os
import struct
import sys


def main():
    if len(sys.argv) != 5:
        sys.exit("usage: patch-stage2.py <stage2.bin> <kernel.lz> <kernel.bin> <out.bin>")

    stage2, packed, kernel, out = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]

    with open(stage2, "rb") as f:
        image = bytearray(f.read())
    if len(image) < 12 or image[0] != 0xEB:
        sys.exit("patch-stage2: %s does not look like a stage2 image" % stage2)

    read_size = os.path.getsize(packed)
    final_size = os.path.getsize(kernel)
    for what, size in (("compressed", read_size), ("decompressed", final_size)):
        # u32: absurd for any kernel we could load, and it catches a build
        # that somehow produced a 4 GiB file.
        if size <= 0 or size >= 0xFFFF_FFFF:
            sys.exit("patch-stage2: implausible %s size %d" % (what, size))

    struct.pack_into("<I", image, 4, read_size)
    struct.pack_into("<I", image, 8, final_size)
    with open(out, "wb") as f:
        f.write(image)

    print("[bios] stage2 patched: %d compressed bytes (%d sectors) -> %d bytes"
          % (read_size, (read_size + 511) // 512, final_size))


if __name__ == "__main__":
    main()
