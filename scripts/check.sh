#!/usr/bin/env bash
# =============================================================================
#  barryOS — self-verification harness (L0–L3)
#  L0: make all                       succeeds
#  L1: artifacts exist + format ok    (img/iso/efi/elf/bin)
#  L2: BIOS QEMU boot                serial contains "barryOS booted"
#  L3: UEFI QEMU boot                serial contains "barryOS booted"
# =============================================================================
set -u
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
source "$ROOT/scripts/env.sh"
export PATH="/home/z/.opt/usr/bin:/home/z/.opt/usr/sbin:$PATH"

mkdir -p build/logs
LOG=build/logs/check-$(date +%Y%m%d-%H%M%S).log
PASS=0; FAIL=0; SKIP=0

log() { echo "$@" | tee -a "$LOG"; }
section() { echo "" | tee -a "$LOG"; echo "=== $1 ===" | tee -a "$LOG"; }
pass() { log "[PASS] $1"; PASS=$((PASS+1)); }
fail() { log "[FAIL] $1"; FAIL=$((FAIL+1)); }
skip() { log "[SKIP] $1"; SKIP=$((SKIP+1)); }

section "barryOS self-check $(date -Iseconds)"

# --------------------------------------------------------------- L0: build --
section "L0: make all"
if make all >/tmp/barryos-make.log 2>&1; then
    pass "make all (exit 0)"
else
    fail "make all (exit $?)"; tail -n 20 /tmp/barryos-make.log | tee -a "$LOG"
    { echo ""; echo "SUMMARY: PASS=$PASS FAIL=$FAIL SKIP=$SKIP"; } | tee -a "$LOG"
    cp "$LOG" build/logs/CHECK_REPORT.md; exit 1
fi

# -------------------------------------------------- L1: artifact existence --
section "L1: artifacts + format checks"
check_file() { if [ -f "$1" ]; then pass "exists: $1"; else fail "missing: $1"; fi; }
for f in build/kernel.elf build/kernel.bin build/mbr.bin build/stage2.bin \
         build/barryOS-bios.img build/BOOTX64.EFI build/barryOS-uefi.img build/barryOS.iso; do
    check_file "$f"
done

section "L1b: MBR magic 0x55AA"
mag=$(od -An -tx1 -j 510 -N 2 build/mbr.bin 2>/dev/null | tr -d ' \n')
if [ "$mag" = "55aa" ]; then pass "MBR magic = 0x55AA"; else fail "MBR magic = '$mag' (want 55aa)"; fi

section "L1c: kernel ELF arch"
if readelf -h build/kernel.elf 2>/dev/null | grep -qi "Machine:.*x86-64"; then
    pass "kernel ELF machine = x86-64"
else
    fail "kernel ELF machine not x86-64"
fi

section "L1d: BOOTX64.EFI format"
effmt=$(file -b build/BOOTX64.EFI 2>/dev/null)
if echo "$effmt" | grep -qiE "PE32\+|EFI"; then pass "EFI format: $effmt"; else fail "EFI format: $effmt"; fi

section "L1e: stage2 size = 31 sectors"
s2sz=$(stat -c%s build/stage2.bin 2>/dev/null)
if [ "$s2sz" = "15872" ]; then pass "stage2 size = 15872 bytes"; else fail "stage2 size = '$s2sz'"; fi

# ----------------------------------------------------------- L2: BIOS boot --
section "L2: BIOS QEMU boot (serial)"
cp "$OVMF_VARS" build/OVMF_VARS.check.fd 2>/dev/null  # touch only
timeout 20 qemu-system-x86_64 -L "$QEMU_BIOS_DIR" \
    -drive format=raw,file=build/barryOS-bios.img \
    -serial stdio -display none -no-reboot -no-shutdown \
    >/tmp/barryos-bios.log 2>&1
if grep -q "barryOS booted" /tmp/barryos-bios.log; then
    pass "BIOS: serial contains 'barryOS booted'"
else
    fail "BIOS: serial missing 'barryOS booted'"
fi
if grep -qiE "triple fault|panic|general protection" /tmp/barryos-bios.log; then fail "BIOS: fault detected"; else pass "BIOS: no faults"; fi
cp /tmp/barryos-bios.log build/logs/qemu-bios.log

# ----------------------------------------------------------- L3: UEFI boot --
section "L3: UEFI QEMU boot (serial)"
cp "$OVMF_VARS" build/OVMF_VARS.check.fd
( sleep 8; printf 'fs0:\r\n'; sleep 1; printf '\\EFI\\BOOT\\BOOTX64.EFI\r\n'; sleep 10 ) | \
timeout 25 qemu-system-x86_64 -L "$QEMU_BIOS_DIR" \
    -drive if=pflash,format=raw,readonly=on,file="$OVMF_CODE" \
    -drive if=pflash,format=raw,file=build/OVMF_VARS.check.fd \
    -drive format=raw,file=build/barryOS-uefi.img \
    -serial stdio -display none -no-reboot -no-shutdown \
    >/tmp/barryos-uefi.log 2>&1
if grep -q "barryOS booted" /tmp/barryos-uefi.log; then
    pass "UEFI: serial contains 'barryOS booted'"
else
    fail "UEFI: serial missing 'barryOS booted'"
fi
cp /tmp/barryos-uefi.log build/logs/qemu-uefi.log

# ---------------------------------------------------- L4: Stage 2 memory --
section "L4: Stage 2 memory subsystem"
if grep -q "memory subsystem online" /tmp/barryos-bios.log; then
    pass "BIOS: Stage 2 memory subsystem online"
else
    fail "BIOS: Stage 2 memory subsystem not online"
fi
if grep -q "CR3 0x" /tmp/barryos-bios.log; then
    pass "BIOS: CR3 switched (own page tables)"
else
    fail "BIOS: CR3 switch missing"
fi
if grep -q "heap test 1: val=0x123456789ABCDEF0 OK" /tmp/barryos-bios.log; then
    pass "BIOS: heap alloc+write+read OK"
else
    fail "BIOS: heap smoke test failed"
fi
if grep -q "Vec with_capacity" /tmp/barryos-bios.log; then
    pass "BIOS: Vec::with_capacity works"
else
    fail "BIOS: Vec test missing"
fi
if grep -q "Box=0xDEADBEEF OK" /tmp/barryos-bios.log; then
    pass "BIOS: Box::new works"
else
    fail "BIOS: Box test failed"
fi

# ---------------------------------------------------- L5: Stage 3 interrupts --
section "L5: Stage 3 interrupt subsystem"
if grep -q "interrupt subsystem online" /tmp/barryos-bios.log; then
    pass "BIOS: Stage 3 interrupt subsystem online"
else
    fail "BIOS: Stage 3 interrupt subsystem not online"
fi
if grep -q "IDT loaded (256 entries" /tmp/barryos-bios.log; then
    pass "BIOS: IDT loaded (256 entries)"
else
    fail "BIOS: IDT load missing"
fi
if grep -q "PIC remapped" /tmp/barryos-bios.log; then
    pass "BIOS: PIC remapped (IRQ0..15 → INT 32..47)"
else
    fail "BIOS: PIC remap missing"
fi
if grep -q "PIT configured" /tmp/barryos-bios.log; then
    pass "BIOS: PIT configured (100 Hz)"
else
    fail "BIOS: PIT config missing"
fi
# Verify timer actually ticked (IRQ0 handler ran).
ticks=$(grep "timer ticks:" /tmp/barryos-bios.log | grep -oE "[0-9]+" | head -1)
if [ -n "$ticks" ] && [ "$ticks" -gt 0 ] 2>/dev/null; then
    pass "BIOS: timer interrupts fired (ticks=$ticks)"
else
    fail "BIOS: no timer ticks (irq0 handler didn't run)"
fi

# ---------------------------------------------------------------- summary --
section "SUMMARY"
log "PASS=$PASS  FAIL=$FAIL  SKIP=$SKIP"
log "Full log: $LOG"
cp "$LOG" build/logs/CHECK_REPORT.md
[ "$FAIL" -gt 0 ] && exit 1 || exit 0
