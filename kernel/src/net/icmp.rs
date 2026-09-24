//! ICMP echo — ping.
//!
//! The smallest thing that proves the stack end to end: it needs ARP to
//! resolve the next hop, IPv4 to build a header, and the receive path to work
//! in the other direction.  A reply also means a real machine on the other end
//! looked at our packet and chose to answer it.
//!
//! One echo is in flight at a time.  A ping is a diagnostic, not a service;
//! a table of outstanding requests would be more code than it is worth.

use crate::interrupts;
use crate::net::{arp, ipv4};
use crate::serial;
use core::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicU64, Ordering};

pub const TYPE_ECHO_REPLY: u8 = 0;
pub const TYPE_ECHO_REQUEST: u8 = 8;

/// Identifier we put in our requests, so we can tell our own replies from
/// anything else that happens to be on the wire.
const OUR_ID: u16 = 0x0B05;             // "barryOS", near enough

static PENDING: AtomicBool = AtomicBool::new(false);
static PENDING_SEQ: AtomicU16 = AtomicU16::new(0);
static PENDING_TARGET: AtomicU32 = AtomicU32::new(0);
static PENDING_SINCE: AtomicU64 = AtomicU64::new(0);

/// Set when a reply lands, and read by whoever is waiting.
static REPLY_SEEN: AtomicBool = AtomicBool::new(false);
static REPLY_RTT_MS: AtomicU32 = AtomicU32::new(0);
static REPLY_FROM: AtomicU32 = AtomicU32::new(0);

pub static REQUESTS_SENT: AtomicU64 = AtomicU64::new(0);
pub static REPLIES_RECEIVED: AtomicU64 = AtomicU64::new(0);

/// Send one echo request.  Returns false if the next hop is unresolved, which
/// happens on the first packet to a new destination: an ARP request has gone
/// out and a retry a moment later will succeed.
pub fn request(dst: [u8; 4], seq: u16) -> bool {
    let mut pkt = [0u8; 8 + 32];
    pkt[0] = TYPE_ECHO_REQUEST;
    pkt[1] = 0;                                     // code
    // Checksum at 2..4 stays zero until the body is in place.
    pkt[4..6].copy_from_slice(&OUR_ID.to_be_bytes());
    pkt[6..8].copy_from_slice(&seq.to_be_bytes());
    // A recognisable payload, so a reply that comes back mangled is obvious.
    for (i, b) in pkt[8..].iter_mut().enumerate() {
        *b = b'a' + (i % 26) as u8;
    }
    let ck = ipv4::checksum(&pkt);
    pkt[2..4].copy_from_slice(&ck.to_be_bytes());

    if !ipv4::send(dst, ipv4::PROTO_ICMP, &pkt) {
        return false;
    }

    PENDING.store(true, Ordering::Relaxed);
    PENDING_SEQ.store(seq, Ordering::Relaxed);
    PENDING_TARGET.store(arp::ip_to_u32(dst), Ordering::Relaxed);
    PENDING_SINCE.store(interrupts::TIMER_TICKS.load(Ordering::Relaxed), Ordering::Relaxed);
    REPLY_SEEN.store(false, Ordering::Relaxed);
    REQUESTS_SENT.fetch_add(1, Ordering::Relaxed);
    true
}

/// Is an echo still waiting for an answer?
pub fn pending() -> bool {
    PENDING.load(Ordering::Relaxed)
}

/// How long the outstanding echo has been waiting, in milliseconds.
pub fn pending_age_ms() -> u32 {
    let now = interrupts::TIMER_TICKS.load(Ordering::Relaxed);
    let since = PENDING_SINCE.load(Ordering::Relaxed);
    ticks_to_ms(now.saturating_sub(since))
}

/// The reply that arrived, if one has: (from, rtt in ms).
pub fn take_reply() -> Option<([u8; 4], u32)> {
    if !REPLY_SEEN.load(Ordering::Relaxed) {
        return None;
    }
    REPLY_SEEN.store(false, Ordering::Relaxed);
    let from = arp::u32_to_ip(REPLY_FROM.load(Ordering::Relaxed));
    Some((from, REPLY_RTT_MS.load(Ordering::Relaxed)))
}

fn ticks_to_ms(ticks: u64) -> u32 {
    let ms = ticks * 10;                            // the PIT runs at 100 Hz
    if ms > u32::MAX as u64 { u32::MAX } else { ms as u32 }
}

/// Handle one ICMP message (the IPv4 payload).
pub fn handle(src: [u8; 4], body: &[u8]) {
    if body.len() < 8 {
        return;
    }
    // The checksum covers the whole ICMP message, so verify before trusting
    // anything in it.
    if ipv4::checksum(&body[..body.len()]) != 0 {
        return;
    }

    match body[0] {
        TYPE_ECHO_REPLY => {
            let id = u16::from_be_bytes([body[4], body[5]]);
            let seq = u16::from_be_bytes([body[6], body[7]]);
            if id != OUR_ID {
                return;                         // somebody else's ping
            }
            REPLIES_RECEIVED.fetch_add(1, Ordering::Relaxed);

            let rtt = {
                let now = interrupts::TIMER_TICKS.load(Ordering::Relaxed);
                let since = PENDING_SINCE.load(Ordering::Relaxed);
                ticks_to_ms(now.saturating_sub(since))
            };
            if PENDING.load(Ordering::Relaxed) && PENDING_SEQ.load(Ordering::Relaxed) == seq {
                PENDING.store(false, Ordering::Relaxed);
            }
            REPLY_FROM.store(arp::ip_to_u32(src), Ordering::Relaxed);
            REPLY_RTT_MS.store(rtt, Ordering::Relaxed);
            REPLY_SEEN.store(true, Ordering::Relaxed);

            serial::print_str("[icmp] echo reply from ");
            ipv4::log_ip(src);
            serial::print_str(" seq ");
            serial::print_hex(seq as u64);
            serial::print_str(" rtt ");
            serial::print_hex(rtt as u64);
            serial::print_str(" ms\n");
        }
        TYPE_ECHO_REQUEST => {
            // Answer pings aimed at us.  Not needed for anything we do, but a
            // stack that ignores them looks broken from the outside.
            let mut reply = [0u8; 1500];
            let n = body.len().min(reply.len());
            reply[..n].copy_from_slice(&body[..n]);
            reply[0] = TYPE_ECHO_REPLY;
            reply[2..4].copy_from_slice(&0u16.to_be_bytes());
            let ck = ipv4::checksum(&reply[..n]);
            reply[2..4].copy_from_slice(&ck.to_be_bytes());
            ipv4::send(src, ipv4::PROTO_ICMP, &reply[..n]);
        }
        _ => {}
    }
}
