//! barryOS network stack.
//!
//! Layered bottom-up, and deliberately staged — each layer is only useful once
//! the one below it works:
//!
//!   pci    (in `dev`)  find the adapter
//!   e1000              reset, link, RX/TX descriptor rings, frame send/recv
//!   arp                address resolution — the first thing that proves the
//!                      card can actually talk to the network
//!   (next)             IPv4, ICMP, UDP, DHCP, TCP

pub mod arp;
pub mod e1000;

use crate::serial;
use core::sync::atomic::{AtomicBool, Ordering};

pub static INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Default address.  There is no DHCP client yet, so this is a guess at the
/// VMware NAT subnet and can be changed at runtime with the `ip` command.
const DEFAULT_IP: [u8; 4] = [192, 168, 1, 100];

/// Bring the network stack up.  Requires `dev::pci::init()` to have run.
pub fn init() {
    serial::print_str("[net] step 1: Intel e1000 Ethernet adapter\n");
    e1000::init();

    if e1000::present() {
        arp::set_our_ip(DEFAULT_IP);
        serial::print_str("[net] step 2: IPv4 address ");
        arp::print_ip(arp::our_ip());
        serial::print_str(" (static; `ip` command changes it)\n");
    }

    INITIALIZED.store(true, Ordering::Release);
    serial::print_str("[net] stack initialized\n");
}

/// Did we end up with a usable adapter?
pub fn link_present() -> bool {
    e1000::present()
}

/// Drain the receive ring.  Called from the input loop; polling rather than
/// interrupts because the adapter's interrupt source is masked.
pub fn poll() {
    if e1000::present() {
        arp::poll();
    }
}

/// Human-readable one-line status.
pub fn print_status() {
    serial::print_str("[net] adapter ");
    if !e1000::present() {
        serial::print_str("absent\n");
        return;
    }
    serial::print_str("present, MAC ");
    if let Some(m) = e1000::mac() {
        arp::print_mac(&m);
    }
    serial::print_str(", link ");
    serial::print_str(if e1000::link_up() { "up" } else { "down" });
    serial::print_str(", ip ");
    arp::print_ip(arp::our_ip());
    serial::print_str("\n");
}
