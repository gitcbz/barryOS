#!/usr/bin/env python3
"""Write a VMDK descriptor that points at a raw disk image.

VMware will happily boot a flat extent — a descriptor file naming the raw
image and its geometry is all a `vmfs` VMDK is.  `qemu-img convert` produces
exactly this file, and it is the only thing it was being used for here, so
writing it directly means building the VM images needs no qemu-utils at all:
one less thing a host has to have, and the same bytes on every platform.

Usage: scripts/mk-vmdk.py <image.img> <out.vmdk> [adapter]
"""

import io
import os
import sys

DESCRIPTOR = """# Disk DescriptorFile
version=1
encoding="UTF-8"
CID=fffffffe
parentCID=ffffffff
createType="vmfs"

# Extent description
# {name} = {size} bytes = {sectors} x 512
RW {sectors} FLAT "{name}" 0

# The Disk Data Base
#DDB

ddb.virtualHWVersion = "4"
ddb.geometry.cylinders = "{cylinders}"
ddb.geometry.heads = "16"
ddb.geometry.sectors = "32"
ddb.adapterType = "{adapter}"
"""


def main():
    if len(sys.argv) < 3:
        sys.exit(__doc__)
    img, out = sys.argv[1], sys.argv[2]
    adapter = sys.argv[3] if len(sys.argv) > 3 else "ide"

    # The descriptor names the image by its bare file name, and VMware looks
    # for it beside the descriptor -- so the two have to be in one directory.
    name = os.path.basename(img)
    if os.path.dirname(os.path.abspath(img)) != os.path.dirname(os.path.abspath(out)):
        sys.exit("mk-vmdk: %s and %s must be in the same directory" % (img, out))

    size = os.path.getsize(img)
    if size % 512 != 0:
        sys.exit("mk-vmdk: %s is not a whole number of sectors" % img)
    sectors = size // 512

    # 512 bytes per sector, 32 sectors per track, 16 heads: 256 KiB per
    # cylinder.  The exact geometry does not matter to anyone but the BIOS, and
    # this is the one qemu-img picks.
    cylinders = (sectors + 16 * 32 - 1) // (16 * 32)

    io.open(out, "w", encoding="utf-8", newline="\n").write(
        DESCRIPTOR.format(name=name, size=size, sectors=sectors,
                          cylinders=cylinders, adapter=adapter)
    )
    print("[vmdk] %s -> %s (%d sectors, %d cylinders)" % (img, out, sectors, cylinders))


if __name__ == "__main__":
    main()
