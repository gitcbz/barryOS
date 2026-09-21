#!/bin/bash
# Test UEFI boot from the hybrid ISO (El Torito UEFI boot entry).
source /home/z/my-project/barryOS/scripts/env.sh
cd /home/z/my-project/barryOS
cp "$OVMF_VARS" build/v.fd 2>/dev/null
rm -f /tmp/uefi-iso.out

( sleep 8
  printf 'connect -r\r\n'
  sleep 2
  printf 'map -r\r\n'
  sleep 2
  printf 'ls fs0:\r\n'
  sleep 1
  printf 'fs0:\r\n'
  sleep 1
  printf '\\EFI\\BOOT\\BOOTX64.EFI\r\n'
  sleep 6
) | qemu-system-x86_64 -L "$QEMU_BIOS_DIR" \
    -drive if=pflash,format=raw,readonly=on,file="$OVMF_CODE" \
    -drive if=pflash,format=raw,file=build/v.fd \
    -cdrom build/barryOS.iso \
    -boot d \
    -serial stdio -display none -no-reboot > /tmp/uefi-iso.out 2>&1
rc=$?
echo "=== OUTPUT (filtered) ==="
cat /tmp/uefi-iso.out | tr -d '\000' | sed 's/\x1b\[[0-9;]*[a-zA-Z]//g' | grep -iE "fs0|barryOS|EFI|boot|connect|map|BLK|loaded|error|fail|Shell>|Not Found" | head -40
echo "=== rc=$rc ==="
