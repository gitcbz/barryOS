//! barryOS kernel — Stage 11 VMware optimization.
//!
//! Modules:
//! - `svga`:    VMware SVGA-II driver stubs (PCI device detection, VRAM mapping).
//! - `backdoor`: VMware backdoor protocol (RPCI for shared folders, clipboard, time sync).
//! - `vmballoon`: VMware memory balloon driver stub.

pub mod svga;
pub mod backdoor;
pub mod vmballoon;

use core::sync::atomic::{AtomicBool, Ordering};
use crate::serial;

pub static INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Initialize VMware optimization subsystems:
///   1. SVGA-II driver (PCI detection + VRAM stub).
///   2. VMware backdoor (RPCI detection + shared folders stub).
///   3. Memory balloon driver stub.
pub fn init() {
    serial::print_str("[vmware] step 1: SVGA-II driver\n");
    svga::init();

    serial::print_str("[vmware] step 2: VMware backdoor\n");
    backdoor::init();
    backdoor::test_rpci();

    serial::print_str("[vmware] step 3: memory balloon\n");
    vmballoon::init();

    INITIALIZED.store(true, Ordering::Release);
    serial::print_str("[vmware] VMware optimization online\n");

    // Check if we're actually running under VMware.
    let is_vmware = backdoor::detect_vmware();
    if is_vmware {
        serial::print_str("[vmware] running under VMware (backdoor detected)\n");
    } else {
        serial::print_str("[vmware] not running under VMware (QEMU/other)\n");
        serial::print_str("[vmware] stubs active — ready for VMware deployment\n");
    }
}
