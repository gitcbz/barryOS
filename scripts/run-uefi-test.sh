#!/bin/bash
# Drive the OVMF shell: force driver connect, then boot the loader.
source /home/z/my-project/barryOS/scripts/env.sh
cd /home/z/my-project/barryOS
cp "$OVMF_VARS" build/v.fd 2>/dev/null
rm -f /tmp/uefi-boot.out

( sleep 8
  printf 'connect -r\r\n'      # recursively connect all drivers
  sleep 2
  printf 'map -r\r\n'           # refresh mappings
  sleep 2
  printf 'ls fs0:\r\n'          # list ESP root
  sleep 1
  printf 'fs0:\r\n'
  sleep 1
  printf '\\EFI\\BOOT\\BOOTX64.EFI\r\n'
  sleep 6
) | qemu-system-x86_64 -L "$QEMU_BIOS_DIR" \
    -drive if=pflash,format=raw,readonly=on,file="$OVMF_CODE" \
    -drive if=pflash,format=raw,file=build/v.fd \
    -drive format=raw,file=build/barryOS-uefi.img \
    -serial stdio -display none -no-reboot > /tmp/uefi-boot.out 2>&1
rc=$?
echo "=== OUTPUT (filtered) ==="
cat /tmp/uefi-boot.out | tr -d '\000' | sed 's/\x1b\[[0-9;]*[a-zA-Z]//g' | tail -40
echo "=== rc=$rc ==="
