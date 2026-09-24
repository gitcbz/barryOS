//! DHCP client.
//!
//! A static address is a guess about someone else's network, and the first
//! version of this stack made exactly that guess (192.168.1.100, which is not
//! where VMware's NAT lives).  Asking is strictly better: one exchange gets us
//! the address, the subnet mask, the router and the DNS server, and none of
//! them have to be right in advance.
//!
//! Driven from `tick()` — DISCOVER, wait for OFFER, REQUEST, wait for ACK —
//! with retransmission on a one-second timer, so a boot does not stall on a
//! server that never answers.

use crate::interrupts;
use crate::net::{arp, dns, ipv4, udp};
use crate::serial;
use core::sync::atomic::{AtomicU32, AtomicU8, AtomicU64, Ordering};

const SERVER_PORT: u16 = 67;
const CLIENT_PORT: u16 = 68;

const OP_REQUEST: u8 = 1;
const OP_REPLY: u8 = 2;

const MSG_DISCOVER: u8 = 1;
const MSG_OFFER: u8 = 2;
const MSG_REQUEST: u8 = 3;
const MSG_ACK: u8 = 5;

const OPT_SUBNET_MASK: u8 = 1;
const OPT_ROUTER: u8 = 3;
const OPT_DNS: u8 = 6;
const OPT_REQUESTED_IP: u8 = 50;
const OPT_LEASE_TIME: u8 = 51;
const OPT_MESSAGE_TYPE: u8 = 53;
const OPT_SERVER_ID: u8 = 54;
const OPT_PARAM_LIST: u8 = 55;
const OPT_END: u8 = 255;

const MAGIC_COOKIE: [u8; 4] = [99, 130, 83, 99];

#[derive(Clone, Copy, PartialEq)]
enum State {
    /// Nothing sent yet.
    Idle,
    /// DISCOVER sent, waiting for an OFFER.
    Discovering,
    /// REQUEST sent, waiting for an ACK.
    Requesting,
    /// Lease in hand.
    Bound,
    /// Gave up; the interface keeps whatever it had.
    Failed,
}

static STATE: AtomicU8 = AtomicU8::new(0);
static XID: AtomicU32 = AtomicU32::new(0);
static OFFERED_IP: AtomicU32 = AtomicU32::new(0);
static SERVER_ID: AtomicU32 = AtomicU32::new(0);
static LAST_SEND_TICK: AtomicU64 = AtomicU64::new(0);
static ATTEMPTS: AtomicU32 = AtomicU32::new(0);
static LEASE_SECS: AtomicU32 = AtomicU32::new(0);

pub static OFFERS_SEEN: AtomicU64 = AtomicU64::new(0);
pub static ACKS_SEEN: AtomicU64 = AtomicU64::new(0);

fn state() -> State {
    match STATE.load(Ordering::Relaxed) {
        1 => State::Discovering,
        2 => State::Requesting,
        3 => State::Bound,
        4 => State::Failed,
        _ => State::Idle,
    }
}

fn set_state(s: State) {
    STATE.store(match s {
        State::Idle => 0,
        State::Discovering => 1,
        State::Requesting => 2,
        State::Bound => 3,
        State::Failed => 4,
    }, Ordering::Relaxed);
}

/// Seconds left on the lease, or 0 if we never got one.
pub fn lease_seconds() -> u32 {
    LEASE_SECS.load(Ordering::Relaxed)
}

pub fn is_bound() -> bool {
    state() == State::Bound
}

/// Gave up: every retransmission went unanswered.
pub fn failed() -> bool {
    state() == State::Failed
}

/// Still trying.
pub fn in_progress() -> bool {
    matches!(state(), State::Discovering | State::Requesting)
}

/// One line for the boot log.
pub fn status_str() -> &'static str {
    match state() {
        State::Idle => "idle",
        State::Discovering => "discovering",
        State::Requesting => "requesting",
        State::Bound => "bound",
        State::Failed => "failed",
    }
}

/// How long to wait before deciding the server is not coming.  Four
/// retransmissions at one second apart is generous for a NAT device that
/// answers in microseconds.
const MAX_ATTEMPTS: u32 = 4;
const RETRY_TICKS: u64 = 100;           // 100 Hz → one second

pub fn init() {
    udp::listen(CLIENT_PORT, handle);
    // A fixed xid per boot is enough: it only has to match our own replies.
    let seed = interrupts::TIMER_TICKS.load(Ordering::Relaxed) as u32;
    XID.store(seed ^ 0x0B05_0B05, Ordering::Relaxed);
    set_state(State::Idle);
}

/// Start the exchange.  Safe to call more than once; a bound lease is kept.
pub fn start() {
    if state() == State::Bound {
        return;
    }
    ATTEMPTS.store(0, Ordering::Relaxed);
    set_state(State::Discovering);
    send_discover();
}

/// Drive retransmission.  Called once per input-loop pass.
pub fn tick() {
    let s = state();
    if s == State::Bound || s == State::Failed || s == State::Idle {
        return;
    }

    let now = interrupts::TIMER_TICKS.load(Ordering::Relaxed);
    let last = LAST_SEND_TICK.load(Ordering::Relaxed);
    if now.saturating_sub(last) < RETRY_TICKS {
        return;
    }

    let n = ATTEMPTS.fetch_add(1, Ordering::Relaxed) + 1;
    if n >= MAX_ATTEMPTS {
        set_state(State::Failed);
        serial::print_str("[dhcp] no server answered; keeping the static address\n");
        return;
    }
    serial::print_str("[dhcp] retransmitting (attempt ");
    serial::print_dec(n as u64 + 1);
    serial::print_str(")\n");

    if s == State::Discovering {
        send_discover();
    } else {
        send_request();
    }
}

/// Build the fixed part of a DHCP message, leaving the options area to the
/// caller.  Returns the offset the options should start at.
fn base_packet(buf: &mut [u8; 300]) -> usize {
    for b in buf.iter_mut() {
        *b = 0;
    }
    buf[0] = OP_REQUEST;
    buf[1] = 1;                                 // Ethernet
    buf[2] = 6;                                 // MAC length
    buf[3] = 0;                                 // hops
    buf[4..8].copy_from_slice(&XID.load(Ordering::Relaxed).to_be_bytes());
    buf[8..10].copy_from_slice(&0u16.to_be_bytes());    // seconds
    // Broadcast flag: the reply comes to 255.255.255.255, which is what we can
    // receive before the address is configured.
    buf[10..12].copy_from_slice(&0x8000u16.to_be_bytes());
    if let Some(mac) = e1000_mac() {
        buf[28..34].copy_from_slice(&mac);
    }
    buf[236..240].copy_from_slice(&MAGIC_COOKIE);
    240
}

fn e1000_mac() -> Option<[u8; 6]> {
    crate::net::e1000::mac()
}

fn push_option(buf: &mut [u8; 300], at: usize, code: u8, data: &[u8]) -> usize {
    let mut i = at;
    if i + 2 + data.len() >= buf.len() {
        return i;
    }
    buf[i] = code;
    buf[i + 1] = data.len() as u8;
    buf[i + 2..i + 2 + data.len()].copy_from_slice(data);
    i += 2 + data.len();
    i
}

fn send_discover() {
    let mut buf = [0u8; 300];
    let mut at = base_packet(&mut buf);
    at = push_option(&mut buf, at, OPT_MESSAGE_TYPE, &[MSG_DISCOVER]);
    // Ask for everything we will actually use, in the order we want it.
    at = push_option(&mut buf, at, OPT_PARAM_LIST,
                     &[OPT_SUBNET_MASK, OPT_ROUTER, OPT_DNS, OPT_LEASE_TIME]);
    buf[at] = OPT_END;

    LAST_SEND_TICK.store(interrupts::TIMER_TICKS.load(Ordering::Relaxed), Ordering::Relaxed);
    // From 0.0.0.0 to the broadcast address: there is no address to send from
    // and no next hop to resolve.
    udp::send_from([0, 0, 0, 0], ipv4::BROADCAST, SERVER_PORT, CLIENT_PORT, &buf);
    serial::print_str("[dhcp] DISCOVER sent (xid 0x");
    serial::print_hex(XID.load(Ordering::Relaxed) as u64);
    serial::print_str(")\n");
}

fn send_request() {
    let mut buf = [0u8; 300];
    let mut at = base_packet(&mut buf);
    at = push_option(&mut buf, at, OPT_MESSAGE_TYPE, &[MSG_REQUEST]);
    // Echoing the offered address and the server that offered it is what
    // makes this a request for *that* lease rather than a new discovery.
    at = push_option(&mut buf, at, OPT_REQUESTED_IP,
                     &OFFERED_IP.load(Ordering::Relaxed).to_be_bytes());
    at = push_option(&mut buf, at, OPT_SERVER_ID,
                     &SERVER_ID.load(Ordering::Relaxed).to_be_bytes());
    at = push_option(&mut buf, at, OPT_PARAM_LIST,
                     &[OPT_SUBNET_MASK, OPT_ROUTER, OPT_DNS, OPT_LEASE_TIME]);
    buf[at] = OPT_END;

    LAST_SEND_TICK.store(interrupts::TIMER_TICKS.load(Ordering::Relaxed), Ordering::Relaxed);
    udp::send_from([0, 0, 0, 0], ipv4::BROADCAST, SERVER_PORT, CLIENT_PORT, &buf);
    serial::print_str("[dhcp] REQUEST sent for ");
    ipv4::log_ip(arp::u32_to_ip(OFFERED_IP.load(Ordering::Relaxed)));
    serial::print_str("\n");
}

/// A DHCP message arrived on port 68.
fn handle(src: [u8; 4], _src_port: u16, payload: &[u8]) {
    if payload.len() < 240 || payload[0] != OP_REPLY {
        return;
    }
    let xid = u32::from_be_bytes(payload[4..8].try_into().unwrap_or([0; 4]));
    if xid != XID.load(Ordering::Relaxed) {
        return;                             // a reply to some other client
    }
    if payload[236..240] != MAGIC_COOKIE {
        return;
    }

    let yiaddr = u32::from_be_bytes(payload[16..20].try_into().unwrap_or([0; 4]));

    let mut opt = 240usize;
    let mut msg_type = 0u8;
    let mut mask: Option<[u8; 4]> = None;
    let mut router: Option<[u8; 4]> = None;
    let mut dns_server: Option<[u8; 4]> = None;
    let mut server_id: Option<[u8; 4]> = None;
    let mut lease: Option<u32> = None;

    while opt < payload.len() {
        let code = payload[opt];
        if code == OPT_END {
            break;
        }
        if code == 0 {
            opt += 1;                       // pad
            continue;
        }
        if opt + 1 >= payload.len() {
            break;
        }
        let len = payload[opt + 1] as usize;
        if opt + 2 + len > payload.len() {
            break;
        }
        let data = &payload[opt + 2..opt + 2 + len];
        let four = |d: &[u8]| -> Option<[u8; 4]> { d.try_into().ok() };
        match code {
            OPT_MESSAGE_TYPE if len == 1 => msg_type = data[0],
            OPT_SUBNET_MASK => mask = four(data),
            OPT_ROUTER => router = four(data),
            OPT_DNS => dns_server = four(data),
            OPT_SERVER_ID => server_id = four(data),
            OPT_LEASE_TIME if len == 4 => {
                lease = Some(u32::from_be_bytes(data.try_into().unwrap_or([0; 4])));
            }
            _ => {}
        }
        opt += 2 + len;
    }

    match msg_type {
        MSG_OFFER => {
            OFFERS_SEEN.fetch_add(1, Ordering::Relaxed);
            OFFERED_IP.store(yiaddr, Ordering::Relaxed);
            if let Some(s) = server_id {
                SERVER_ID.store(arp::ip_to_u32(s), Ordering::Relaxed);
            }
            serial::print_str("[dhcp] OFFER of ");
            ipv4::log_ip(arp::u32_to_ip(yiaddr));
            serial::print_str(" from server ");
            ipv4::log_ip(server_id.unwrap_or(src));
            serial::print_str("\n");
            set_state(State::Requesting);
            ATTEMPTS.store(0, Ordering::Relaxed);
            send_request();
        }
        MSG_ACK => {
            ACKS_SEEN.fetch_add(1, Ordering::Relaxed);
            arp::set_our_ip(arp::u32_to_ip(yiaddr));
            if let Some(m) = mask {
                ipv4::set_mask(m);
            }
            if let Some(r) = router {
                ipv4::set_gateway(r);
            }
            if let Some(d) = dns_server {
                dns::set_server(d);
            }
            if let Some(l) = lease {
                LEASE_SECS.store(l, Ordering::Relaxed);
            }
            set_state(State::Bound);

            serial::print_str("[dhcp] bound: ");
            ipv4::print_config();
            if let Some(d) = dns_server {
                serial::print_str("[dhcp] dns server ");
                ipv4::log_ip(d);
                serial::print_str("\n");
            }
        }
        _ => {}
    }
}
