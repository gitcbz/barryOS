//! barryOS network stack.
//!
//! Layered bottom-up, and deliberately staged — each layer is only useful once
//! the one below it works:
//!
//!   pci    (in `dev`)  find the adapter
//!   e1000              reset, link, RX/TX descriptor rings, frame send/recv
//!   arp                address resolution
//!   ipv4               datagrams, checksum, next-hop decision
//!   icmp               ping — the smallest end-to-end proof
//!   udp                datagrams by port
//!   dhcp               an address that is not a guess
//!   dns                name to address
//!   tcp                a connection
//!   http               a page
//!
//! `poll` drains the receive ring and walks the layers up; `tick` walks the
//! timers back down.  Both are called once per pass of the input loop, so a
//! fetch in progress never blocks the compositor.

pub mod arp;
pub mod dhcp;
pub mod dns;
pub mod e1000;
pub mod http;
pub mod icmp;
pub mod ipv4;
pub mod tcp;
pub mod udp;

use crate::serial;
use core::sync::atomic::{AtomicBool, AtomicU16, Ordering};

pub static INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Address the interface starts with, before DHCP has had a say.  Zero means
/// "no address", which is the honest state to begin in — the previous static
/// guess of 192.168.1.100 is not where VMware's NAT lives, and an address that
/// is wrong in a plausible-looking way is worse than none.
const UNCONFIGURED: [u8; 4] = [0, 0, 0, 0];

/// Where we fall back to if no DHCP server answers.  Only used to keep the
/// interface in a defined state; the log says plainly that it is a guess.
const FALLBACK_IP: [u8; 4] = [192, 168, 1, 100];

/// IP header identification field.  Only ever read by whoever reassembles,
/// and we do not fragment, so a counter is as good as anything.
static IP_ID: AtomicU16 = AtomicU16::new(0x0B05);

pub fn ip_id() -> u16 {
    IP_ID.fetch_add(1, Ordering::Relaxed)
}

static FALLBACK_APPLIED: AtomicBool = AtomicBool::new(false);

/// Bring the network stack up.  Requires `dev::pci::init()` to have run.
pub fn init() {
    serial::print_str("[net] step 1: Intel e1000 Ethernet adapter\n");
    e1000::init();

    if !e1000::present() {
        INITIALIZED.store(true, Ordering::Release);
        serial::print_str("[net] stack initialized (no adapter)\n");
        return;
    }

    arp::set_our_ip(UNCONFIGURED);
    serial::print_str("[net] step 2: DHCP\n");
    dhcp::init();
    dhcp::start();

    serial::print_str("[net] step 3: DNS\n");
    dns::init();

    serial::print_str("[net] step 4: HTTP client\n");
    serial::print_str("[http] ready (one request at a time)\n");

    INITIALIZED.store(true, Ordering::Release);
    serial::print_str("[net] stack initialized\n");
}

/// Did we end up with a usable adapter?
pub fn link_present() -> bool {
    e1000::present()
}

/// Do we have an address?
pub fn configured() -> bool {
    arp::our_ip() != UNCONFIGURED
}

/// Drain the receive ring and hand each frame to the layer that owns it.
/// Called from the input loop; polling rather than interrupts because the
/// adapter's interrupt source is masked.
pub fn poll() {
    if e1000::present() {
        let mut buf = [0u8; e1000::BUF_SIZE];
        // Bounded: one pass over the ring's worth of descriptors.
        for _ in 0..e1000::RX_DESC {
            match e1000::recv_frame(&mut buf) {
                Some(n) => dispatch(&buf[..n]),
                None => break,
            }
        }
    }
    tick();
}

/// One Ethernet frame, routed by ethertype.
fn dispatch(frame: &[u8]) {
    if frame.len() < 14 {
        return;
    }
    match u16::from_be_bytes([frame[12], frame[13]]) {
        0x0806 => arp::handle_frame(frame),
        0x0800 => {
            ipv4::handle_frame(frame);
        }
        _ => {}
    }
}

/// Advance every state machine that has a timer.
pub fn tick() {
    if !e1000::present() {
        return;
    }
    dhcp::tick();
    dns::tick();
    tcp::tick();
    http::tick();

    // If DHCP never answered, put *something* on the interface and say so.
    if !FALLBACK_APPLIED.load(Ordering::Relaxed) && dhcp::failed() {
        FALLBACK_APPLIED.store(true, Ordering::Relaxed);
        arp::set_our_ip(FALLBACK_IP);
        serial::print_str("[net] falling back to the static guess ");
        ipv4::log_ip(FALLBACK_IP);
        serial::print_str(" — DHCP got no answer, so this is probably wrong\n");
    }
}

/// Drive the stack until the lease is in hand, or until there is clearly not
/// going to be one.
///
/// The network is otherwise only advanced from the input loop, and that loop
/// starts *after* the login screen — which waits for a human.  A machine
/// sitting at a login prompt would therefore never finish DHCP, and would come
/// up with no address until someone typed.  Waiting here instead costs about
/// ten milliseconds when a server is there and about four seconds when it is
/// not, which is the same four seconds the client would have spent retrying.
pub fn settle() {
    if !e1000::present() {
        return;
    }
    // The client gives up after its own retransmission budget, so this loop is
    // bounded by the state machine rather than by a second timer.
    let mut guard = 0u32;
    while dhcp::in_progress() {
        poll();
        guard += 1;
        if guard > 50_000_000 {
            break;                          // a wedged tick, not a slow server
        }
    }
    if dhcp::is_bound() {
        serial::print_str("[net] address acquired before the desktop\n");
        selftest();
    }
    print_status();
}

/// Exercise the stack once, at boot, where the serial log can show it.
///
/// Every layer above the adapter is invisible until something asks it to do
/// work, and the only other place that happens is a terminal command or the
/// browser — both of which need a human.  This is the same idea as the kernel's
/// other boot-time self-tests: prove the path runs, and say so where a log can
/// capture it.
///
/// The ping is to the gateway, so it only depends on the LAN.  DNS and HTTP
/// are attempted only if the gateway answers, which keeps a machine with no
/// route out from spending its boot timing out on things that cannot work.
fn selftest() {
    serial::print_str("[net] self-test: pinging the gateway\n");
    let gw = ipv4::gateway();
    let mut answered = false;
    for seq in 1..=3u16 {
        for _ in 0..4 {
            if icmp::request(gw, seq) {
                let start = crate::interrupts::TIMER_TICKS.load(Ordering::Relaxed);
                while crate::interrupts::TIMER_TICKS.load(Ordering::Relaxed)
                    .saturating_sub(start) < 100
                {
                    poll();
                    if icmp::take_reply().is_some() {
                        answered = true;
                        break;
                    }
                }
                if answered {
                    break;
                }
            } else {
                spin(20);                   // ARP still resolving
            }
        }
        if answered {
            break;
        }
    }
    if !answered {
        serial::print_str(
            "[net] self-test: the gateway did not answer; skipping the rest\n");
        return;
    }

    serial::print_str("[net] self-test: resolving a name\n");
    if !dns::start(SELFTEST_NAME) {
        serial::print_str("[net] self-test: could not send the query\n");
        return;
    }
    let start = crate::interrupts::TIMER_TICKS.load(Ordering::Relaxed);
    while dns::state() == 1
        && crate::interrupts::TIMER_TICKS.load(Ordering::Relaxed)
            .saturating_sub(start) < 600
    {
        poll();
    }
    if dns::take_result().is_none() {
        serial::print_str("[net] self-test: no answer from DNS\n");
        return;
    }
    serial::print_str("[net] self-test: ok (ICMP, ARP, UDP, DHCP, DNS)\n");
}

/// The name the boot self-test resolves.  Small, plain, and reliably there.
/// It is a test target, not a "home page" — the browser's default is whatever
/// the user types.
const SELFTEST_NAME: &str = "example.com";

/// Poll the stack for `ticks` PIT ticks.
fn spin(ticks: u64) {
    let start = crate::interrupts::TIMER_TICKS.load(Ordering::Relaxed);
    while crate::interrupts::TIMER_TICKS.load(Ordering::Relaxed)
        .saturating_sub(start) < ticks
    {
        poll();
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
    serial::print_str(", dhcp ");
    serial::print_str(dhcp::status_str());
    // Transmits the adapter never came back on.  Should always be zero; if it
    // is not, the card stopped accepting descriptors and nothing else here is
    // worth reading.
    let stalls = e1000::TX_STALLS.load(Ordering::Relaxed);
    if stalls != 0 {
        serial::print_str(", TX STALLS ");
        serial::print_dec(stalls as u64);
    }
    serial::print_str("\n");
}

/// Count of frames that arrived and were handled, for the system panel.
pub fn frames_in() -> u32 {
    e1000::RX_FRAMES.load(Ordering::Relaxed)
}

pub fn frames_out() -> u32 {
    e1000::TX_FRAMES.load(Ordering::Relaxed)
}
