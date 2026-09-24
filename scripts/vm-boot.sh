#!/usr/bin/env bash
# Boot the BIOS image on the host's hypervisor and print the serial log.
#
# There is no QEMU on this machine, and the firmware that matters is the one
# people actually run: VMware's BIOS, a real e1000 on a NAT, a real VBE mode.
# The VM is configured to send COM1 to a file, which is the only view into a
# boot that has no console attached to it.
#
#   scripts/vm-boot.sh          # build, boot, wait, dump the log
#   scripts/vm-boot.sh 60       # wait 60 seconds instead of 40
#
# Override any of these if the VM lives somewhere else:
#   VMRUN=... VMX=... SERIAL=...
set -u

ROOT="$(cd "$(dirname "$0")/.." && pwd)"

VMRUN="${VMRUN:-/e/Program Files (x86)/VMware/VMware Workstation/vmrun.exe}"
VMX="${VMX:-/c/Users/陈炳灼/Documents/Virtual Machines/barryOS/barryOS.vmx}"
SERIAL="${SERIAL:-/c/Users/陈炳灼/Desktop/barryOS/serial.log}"
WAIT="${1:-40}"

if [ ! -x "$VMRUN" ]; then
    echo "vm-boot: no vmrun at $VMRUN" >&2
    exit 2
fi
if [ ! -f "$VMX" ]; then
    echo "vm-boot: no VM at $VMX" >&2
    exit 2
fi

# The VM has no guest tools, so a running instance has to be killed rather than
# asked politely -- and a stale one would hold the disk file open.
"$VMRUN" -T ws stop "$VMX" hard >/dev/null 2>&1
sleep 1

if [ ! -f "$ROOT/build/barryOS-bios.vmdk" ]; then
    echo "vm-boot: no VMDK yet -- run 'make vmdk' first" >&2
    exit 2
fi

: > "$SERIAL" 2>/dev/null || true
rm -f "$SERIAL"

echo "[vm] starting, waiting ${WAIT}s"
"$VMRUN" -T ws start "$VMX" nogui || exit 1
sleep "$WAIT"
"$VMRUN" -T ws stop "$VMX" hard >/dev/null 2>&1

echo "[vm] serial log:"
if [ -f "$SERIAL" ]; then
    # The file is opened in append mode by VMware and carries a BOM; strip the
    # carriage returns so the lines read normally.
    tr -d '\r' < "$SERIAL"
else
    echo "  (no serial log was written)"
fi
