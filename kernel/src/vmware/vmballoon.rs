//! VMware memory balloon driver stub.
//!
//! The VMware balloon driver allows the hypervisor to reclaim
//! memory pages from the guest OS.  For Stage 11 we provide
//! a stub that reports memory stats.

use crate::serial;

pub fn init() {
    serial::print_str("[vmware-bal] balloon driver stub initialized\n");
    serial::print_str("[vmware-bal] target: 0 pages (no ballooning)\n");
    serial::print_str("[vmware-bal] balloon stats: ready for VMware\n");
}
