//! DNS: turn a name into an address.
//!
//! One query type (A), one question, one answer — which is what a browser
//! needs to fetch a page and nothing more.  The two parts worth getting right
//! are name encoding on the way out (length-prefixed labels, not a dotted
//! string) and name *compression* on the way back, where a pointer to an
//! earlier name is marked by the top two bits of the first byte being set.

use crate::interrupts;
use crate::net::{arp, ipv4, udp};
use crate::serial;
use core::sync::atomic::{AtomicU16, AtomicU32, AtomicU64, AtomicU8, Ordering};

/// The server to ask.  DHCP fills this in; before that it is the VMware NAT
/// gateway, which is a guess but a harmless one — a failed query just retries.
static SERVER: AtomicU32 = AtomicU32::new(0);

/// Local port the last query went out from.
static LOCAL_PORT: AtomicU16 = AtomicU16::new(0);
static QUERY_ID: AtomicU16 = AtomicU16::new(0);
static LAST_SEND_TICK: AtomicU64 = AtomicU64::new(0);
static ATTEMPTS: AtomicU32 = AtomicU32::new(0);

/// 0 idle, 1 waiting, 2 answered, 3 failed.
static STATE: AtomicU8 = AtomicU8::new(0);
static RESULT: AtomicU32 = AtomicU32::new(0);
static NAME_LEN: AtomicU32 = AtomicU32::new(0);
static NAME: [AtomicU8; 64] = [const { AtomicU8::new(0) }; 64];

pub static QUERIES: AtomicU64 = AtomicU64::new(0);
pub static ANSWERS: AtomicU64 = AtomicU64::new(0);

const RETRY_TICKS: u64 = 200;           // two seconds
const MAX_ATTEMPTS: u32 = 3;

pub fn set_server(ip: [u8; 4]) {
    SERVER.store(arp::ip_to_u32(ip), Ordering::Relaxed);
}

pub fn server() -> [u8; 4] {
    arp::u32_to_ip(SERVER.load(Ordering::Relaxed))
}

pub fn init() {
    let p = udp::alloc_port();
    LOCAL_PORT.store(p, Ordering::Relaxed);
    udp::listen(p, handle);
}

/// The name the outstanding query is for.
pub fn current_name(out: &mut [u8]) -> usize {
    let n = NAME_LEN.load(Ordering::Relaxed) as usize;
    let n = n.min(out.len());
    for i in 0..n {
        out[i] = NAME[i].load(Ordering::Relaxed);
    }
    n
}

/// 0 idle, 1 waiting for an answer, 2 answered, 3 failed.
pub fn state() -> u8 {
    STATE.load(Ordering::Relaxed)
}

/// Take the resolved address, if one has arrived.  Clears the state, so a
/// second call returns None.
pub fn take_result() -> Option<[u8; 4]> {
    if STATE.load(Ordering::Relaxed) != 2 {
        return None;
    }
    STATE.store(0, Ordering::Relaxed);
    Some(arp::u32_to_ip(RESULT.load(Ordering::Relaxed)))
}

/// Begin resolving `name`.  Returns false if the name cannot be encoded.
pub fn start(name: &str) -> bool {
    if name.len() > 63 || name.is_empty() {
        return false;
    }
    if SERVER.load(Ordering::Relaxed) == 0 {
        return false;
    }

    NAME_LEN.store(name.len() as u32, Ordering::Relaxed);
    for (i, b) in name.bytes().enumerate().take(64) {
        NAME[i].store(b.to_ascii_lowercase(), Ordering::Relaxed);
    }
    // A fresh id per query, so a late answer to the previous one is ignored.
    QUERY_ID.store(
        QUERY_ID.load(Ordering::Relaxed).wrapping_add(1) | 0x0100,
        Ordering::Relaxed,
    );
    ATTEMPTS.store(0, Ordering::Relaxed);
    STATE.store(1, Ordering::Relaxed);
    send_query();
    true
}

fn send_query() {
    let id = QUERY_ID.load(Ordering::Relaxed);
    let mut buf = [0u8; 256];

    buf[0..2].copy_from_slice(&id.to_be_bytes());
    buf[2..4].copy_from_slice(&0x0100u16.to_be_bytes());   // standard query, RD
    buf[4..6].copy_from_slice(&1u16.to_be_bytes());        // one question
    // ANCOUNT/NSCOUNT/ARCOUNT stay zero.

    let mut at = 12usize;
    let mut name = [0u8; 64];
    let n = current_name(&mut name);
    let text = core::str::from_utf8(&name[..n]).unwrap_or("");
    for label in text.split('.') {
        if label.is_empty() {
            continue;
        }
        if at + 1 + label.len() >= buf.len() {
            STATE.store(3, Ordering::Relaxed);
            return;
        }
        buf[at] = label.len() as u8;
        at += 1;
        buf[at..at + label.len()].copy_from_slice(label.as_bytes());
        at += label.len();
    }
    buf[at] = 0;                                    // root label
    at += 1;
    buf[at..at + 2].copy_from_slice(&1u16.to_be_bytes());   // QTYPE = A
    at += 2;
    buf[at..at + 2].copy_from_slice(&1u16.to_be_bytes());   // QCLASS = IN
    at += 2;

    let port = LOCAL_PORT.load(Ordering::Relaxed);
    LAST_SEND_TICK.store(interrupts::TIMER_TICKS.load(Ordering::Relaxed), Ordering::Relaxed);
    let _ = udp::send(server(), 53, port, &buf[..at]);
    QUERIES.fetch_add(1, Ordering::Relaxed);

    serial::print_str("[dns] query ");
    serial::print_str(text);
    serial::print_str(" -> ");
    ipv4::log_ip(server());
    serial::print_str("\n");
}

/// Retransmit, and give up eventually.
pub fn tick() {
    if STATE.load(Ordering::Relaxed) != 1 {
        return;
    }
    let now = interrupts::TIMER_TICKS.load(Ordering::Relaxed);
    if now.saturating_sub(LAST_SEND_TICK.load(Ordering::Relaxed)) < RETRY_TICKS {
        return;
    }
    let n = ATTEMPTS.fetch_add(1, Ordering::Relaxed) + 1;
    if n >= MAX_ATTEMPTS {
        STATE.store(3, Ordering::Relaxed);
        serial::print_str("[dns] no answer after ");
        serial::print_dec(n as u64);
        serial::print_str(" tries\n");
        return;
    }
    send_query();
}

/// Offset just past the name that starts at `at`.
///
/// A name is a run of length-prefixed labels ending in a zero byte, *or* a
/// two-byte pointer to an earlier name (marked by the top two bits of the
/// first byte being set).  Only the offset past it is needed here; nothing in
/// this module ever has to reconstruct the name a pointer refers to.
fn skip_name(msg: &[u8], mut at: usize) -> Option<usize> {
    loop {
        if at >= msg.len() {
            return None;
        }
        let len = msg[at];
        if len & 0xC0 == 0xC0 {
            return if at + 1 < msg.len() { Some(at + 2) } else { None };
        }
        if len == 0 {
            return Some(at + 1);
        }
        at += 1 + len as usize;
    }
}

fn handle(_src: [u8; 4], _src_port: u16, msg: &[u8]) {
    if msg.len() < 12 || STATE.load(Ordering::Relaxed) != 1 {
        return;
    }
    let id = u16::from_be_bytes([msg[0], msg[1]]);
    if id != QUERY_ID.load(Ordering::Relaxed) {
        return;
    }
    // QR bit: this has to be a response, not somebody's query.
    if msg[2] & 0x80 == 0 {
        return;
    }
    let rcode = msg[3] & 0x0F;
    if rcode != 0 {
        serial::print_str("[dns] server returned rcode ");
        serial::print_dec(rcode as u64);
        serial::print_str("\n");
        STATE.store(3, Ordering::Relaxed);
        return;
    }

    let qd = u16::from_be_bytes([msg[4], msg[5]]) as usize;
    let an = u16::from_be_bytes([msg[6], msg[7]]) as usize;

    // Walk past the questions to reach the answers.
    let mut at = 12usize;
    for _ in 0..qd {
        let Some(next) = skip_name(msg, at) else { return };
        at = next + 4;                              // QTYPE + QCLASS
    }

    for _ in 0..an {
        let Some(next) = skip_name(msg, at) else { return };
        at = next;
        if at + 10 > msg.len() {
            return;
        }
        let rtype = u16::from_be_bytes([msg[at], msg[at + 1]]);
        let rdlen = u16::from_be_bytes([msg[at + 8], msg[at + 9]]) as usize;
        let rd_at = at + 10;
        if rd_at + rdlen > msg.len() {
            return;
        }
        if rtype == 1 && rdlen == 4 {
            let ip: [u8; 4] = msg[rd_at..rd_at + 4].try_into().unwrap_or([0; 4]);
            RESULT.store(arp::ip_to_u32(ip), Ordering::Relaxed);
            STATE.store(2, Ordering::Relaxed);
            ANSWERS.fetch_add(1, Ordering::Relaxed);

            let mut name = [0u8; 64];
            let n = self::current_name(&mut name);
            serial::print_str("[dns] ");
            serial::print_str(core::str::from_utf8(&name[..n]).unwrap_or("?"));
            serial::print_str(" is ");
            ipv4::log_ip(ip);
            serial::print_str("\n");
            return;
        }
        at = rd_at + rdlen;
    }

    STATE.store(3, Ordering::Relaxed);
    serial::print_str("[dns] answer had no A record\n");
}
