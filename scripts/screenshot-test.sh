#!/bin/bash
# Run QEMU with display to capture a screenshot of the barryOS framebuffer.
source /home/z/my-project/barryOS/scripts/env.sh
cd /home/z/my-project/barryOS
rm -f /tmp/barryos-screen.ppm
# Run QEMU with VBE framebuffer, redirect to a PPM screenshot.
timeout 8 qemu-system-x86_64 -L "$QEMU_BIOS_DIR" \
    -drive format=raw,file=build/barryOS-bios.img \
    -serial stdio -display none -no-reboot -no-shutdown \
    -vga std \
    -device VGA \
    -monitor none \
    2>&1 | tail -5
echo "=== done ==="
