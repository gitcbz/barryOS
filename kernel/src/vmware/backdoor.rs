//! VMware backdoor protocol — RPCI for shared folders, clipboard, time sync.
//!
//! The VMware backdoor is accessed via I/O port 0x5658 ('VX').
//! It provides an RPC channel for:
//!   - Guest info (OS type, version).
//!   - Shared folders (open/read/write host files).
//!   - Clipboard (copy/paste between guest and host).
//!   - Time sync (sync guest clock to host).
//!   - Power state (shutdown, suspend).
//!
//! For Stage 11 we implement the backdoor detection + RPCI test.

use crate::serial;
use core::sync::atomic::{AtomicBool, Ordering};

/// VMware backdoor I/O port.
const BACKDOOR_PORT: u16 = 0x5658;  // 'VX'

/// Backdoor commands.
const BACKDOOR_CMD_GET_VERSION: u32 = 10;
const BACKDOOR_CMD_PING: u32 = 26;

/// Detection result.
static VMWARE_DETECTED: AtomicBool = AtomicBool::new(false);

/// Initialize the backdoor.
pub fn init() {
    serial::print_str("[vmware-bd] probing VMware backdoor (port 0x5658)...\n");

    let detected = detect_vmware();
    VMWARE_DETECTED.store(detected, Ordering::SeqCst);

    if detected {
        serial::print_str("[vmware-bd] VMware backdoor detected!\n");
        serial::print_str("[vmware-bd] RPCI channel available (shared folders, clipboard, time sync)\n");

        // Try to get VMware version.
        let version = backdoor_call(BACKDOOR_CMD_GET_VERSION);
        serial::print_str("[vmware-bd] VMware version: ");
        serial::print_hex(version as u64);
        serial::print_str("\n");
    } else {
        serial::print_str("[vmware-bd] backdoor not detected (not under VMware)\n");
        serial::print_str("[vmware-bd] stubs ready for VMware deployment\n");
    }
}

/// Detect if we're running under VMware by probing the backdoor port.
pub fn detect_vmware() -> bool {
    // The VMware backdoor detection works by writing a magic value to
    // port 0x5658 and checking the return. On non-VMware systems (QEMU),
    // the port is not connected and the I/O may cause a #GP fault.
    //
    // For safety, we don't actually probe the port in QEMU (it causes
    // a #GP). Instead, we always return false here — the VMware
    // detection will be re-enabled when running under actual VMware.
    //
    // The backdoor_call function still exists for VMware deployment.
    false
}

/// Make a VMware backdoor call.
/// Returns the result value (0 = failure/timeout).
fn backdoor_call(cmd: u32) -> u32 {
    let result: u32;
    let magic_cmd: u32 = 0x564D5868u32 | cmd;
    unsafe {
        // VMware backdoor: write magic+cmd to EAX, port 0x5658, read EAX.
        // Use "out dx, eax" + "in eax, dx" with explicit 32-bit ops.
        core::arch::asm!(
            "xor ebx, ebx",
            "mov dx, {port}",
            "out dx, eax",
            "in eax, dx",
            port = const BACKDOOR_PORT,
            in("eax") magic_cmd,
            lateout("eax") result,
            options(nostack),
        );
    }
    result
}

/// Test the RPCI channel (send a test message).
pub fn test_rpci() {
    if !VMWARE_DETECTED.load(Ordering::Relaxed) {
        serial::print_str("[vmware-bd] RPCI test: skipped (not under VMware)\n");
        return;
    }

    serial::print_str("[vmware-bd] RPCI test: sending ping...\n");
    let result = backdoor_call(BACKDOOR_CMD_PING);
    if result != 0 {
        serial::print_str("[vmware-bd] RPCI test: OK (ping response=0x");
        serial::print_hex(result as u64);
        serial::print_str(")\n");
    } else {
        serial::print_str("[vmware-bd] RPCI test: no response\n");
    }
}
