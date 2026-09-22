//! barryOS kernel — VMware integration.
//!
//! Modules:
//! - `backdoor`: the port-0x5658 register protocol — detection, direct
//!   commands (host clock, version, UUID) and the RPCI tools channel.
//! - `svga`:     the SVGA-II graphics adapter.
//! - `vmballoon`: the memory balloon driver.
//!
//! Everything here is gated on a CPUID check first: the backdoor port is not
//! wired up on other hypervisors, and an `in` from it can raise #GP, which this
//! kernel has no way to resume from.

pub mod backdoor;
pub mod svga;
pub mod vmballoon;

use core::sync::atomic::{AtomicBool, Ordering};
use crate::serial;

pub static INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Did we find a backdoor we can talk to?
pub fn present() -> bool {
    backdoor::present()
}

/// Bring up the VMware-specific pieces.  Safe to call on any machine.
pub fn init() {
    serial::print_str("[vmware] step 1: SVGA-II graphics adapter\n");
    svga::init();

    serial::print_str("[vmware] step 2: backdoor + tools channel\n");
    backdoor::init();

    serial::print_str("[vmware] step 3: memory balloon\n");
    vmballoon::init();

    INITIALIZED.store(true, Ordering::Release);
    if present() {
        serial::print_str("[vmware] running under VMware\n");
    } else {
        serial::print_str("[vmware] not running under VMware; nothing to do\n");
    }
}

// ---------------------------------------------------------------------------
//  Tools-ish services
// ---------------------------------------------------------------------------

/// The host's wall clock, if the backdoor will tell us.
///
/// This is the whole reason a guest agent exists for time: a VM restored from
/// a saved state resumes with whatever clock it was saved with, and only the
/// host knows what time it really is.
pub fn host_time() -> Option<(u64, u32)> {
    backdoor::host_time()
}

/// Ask the host a question over the tools channel.
/// Returns the reply payload, or None if there is no channel or no answer.
pub fn rpci(cmd: &str) -> Option<&'static str> {
    backdoor::rpci_command(cmd)?;
    Some(backdoor::rpci_reply())
}

/// A compact status line for the terminal.
pub fn status_line() -> &'static str {
    backdoor::status_str()
}
