//! HTTP/1.0 client.
//!
//! One request per connection, and `Connection: close`, which is the whole
//! reason this is short: the server tells us the response is over by closing
//! the connection, so there is no chunked transfer encoding to decode and no
//! persistent-connection state to keep.  The response is read into TCP's
//! receive buffer, and what is parsed is the status line, the headers we care
//! about (Content-Type, Content-Length, Location) and where the body starts.
//!
//! Everything here is a state machine advanced by `tick()`, because a fetch
//! takes seconds and the desktop has to keep drawing while it happens.

use crate::net::{dns, tcp};
use crate::serial;
use core::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicU8, Ordering};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Idle,
    Resolving,
    Connecting,
    Sending,
    Receiving,
    Done,
    Failed,
}

fn phase_from_u8(v: u8) -> Phase {
    match v {
        1 => Phase::Resolving,
        2 => Phase::Connecting,
        3 => Phase::Sending,
        4 => Phase::Receiving,
        5 => Phase::Done,
        6 => Phase::Failed,
        _ => Phase::Idle,
    }
}

fn phase_to_u8(p: Phase) -> u8 {
    match p {
        Phase::Idle => 0,
        Phase::Resolving => 1,
        Phase::Connecting => 2,
        Phase::Sending => 3,
        Phase::Receiving => 4,
        Phase::Done => 5,
        Phase::Failed => 6,
    }
}

static PHASE: AtomicU8 = AtomicU8::new(0);
static STATUS: AtomicU16 = AtomicU16::new(0);
/// Byte offset in the receive buffer where the body starts.
static BODY_OFFSET: AtomicU32 = AtomicU32::new(0);
static BODY_DECLARED: AtomicU32 = AtomicU32::new(0);
static HEADERS_PARSED: AtomicBool = AtomicBool::new(false);
static REQUEST_SENT: AtomicBool = AtomicBool::new(false);
static DEADLINE_TICK: AtomicU32 = AtomicU32::new(0);

/// The request line and headers, built once when the fetch starts.  Keeping
/// the bytes rather than the parts means the send path never has to
/// reassemble a string from mutable statics, and a retransmission resends
/// exactly what the first attempt sent.
static mut REQ_BUF: [u8; 512] = [0; 512];
static REQ_LEN: AtomicU32 = AtomicU32::new(0);

/// The host as typed, for the status line.  Not used to build anything.
static mut TARGET: [u8; 128] = [0; 128];
static TARGET_LEN: AtomicU32 = AtomicU32::new(0);
static PORT: AtomicU16 = AtomicU16::new(80);

static ERROR: AtomicU8 = AtomicU8::new(0);
static REDIRECT: AtomicBool = AtomicBool::new(false);
/// Resolved address of the target, so the connect can be retried after an
/// ARP exchange without re-running DNS.
static REMOTE: AtomicU32 = AtomicU32::new(0);

/// How long a whole fetch may take before it is declared failed.  20 seconds
/// is well past a LAN fetch of anything a text browser can show.
const DEADLINE_SECS: u32 = 20;

pub fn phase() -> Phase {
    phase_from_u8(PHASE.load(Ordering::Relaxed))
}

pub fn status_code() -> u16 {
    STATUS.load(Ordering::Relaxed)
}

pub fn body_offset() -> usize {
    BODY_OFFSET.load(Ordering::Relaxed) as usize
}

pub fn body_declared() -> usize {
    BODY_DECLARED.load(Ordering::Relaxed) as usize
}

/// The host that was requested, copied out for a status line.
pub fn target_into(out: &mut [u8]) -> usize {
    let n = TARGET_LEN.load(Ordering::Relaxed) as usize;
    let base = core::ptr::addr_of!(TARGET) as *const u8;
    let n = n.min(out.len()).min(128);
    for (i, slot) in out.iter_mut().enumerate().take(n) {
        *slot = unsafe { core::ptr::read_volatile(base.add(i)) };
    }
    n
}

/// Why the last fetch failed, or "" when it did not.
pub fn error() -> &'static str {
    match ERROR.load(Ordering::Relaxed) {
        1 => "could not parse that address",
        2 => "no DNS server configured",
        3 => "name did not resolve",
        4 => "could not reach the server",
        5 => "the request was lost",
        6 => "the server closed without a response",
        7 => "timed out",
        _ => "",
    }
}

pub fn is_redirect() -> bool {
    REDIRECT.load(Ordering::Relaxed)
}

/// Number of body bytes actually received.
pub fn body_len() -> usize {
    let total = tcp::rx_len();
    total.saturating_sub(body_offset())
}

/// Copy part of the body out of the receive buffer.
pub fn read_body(offset: usize, out: &mut [u8]) -> usize {
    tcp::rx_read_at(body_offset() + offset, out)
}

/// Begin fetching `url`.  Returns false if it cannot be parsed.
pub fn start(url: &str) -> bool {
    tcp::reset();
    PHASE.store(phase_to_u8(Phase::Idle), Ordering::Relaxed);
    STATUS.store(0, Ordering::Relaxed);
    BODY_OFFSET.store(0, Ordering::Relaxed);
    BODY_DECLARED.store(0, Ordering::Relaxed);
    HEADERS_PARSED.store(false, Ordering::Relaxed);
    REQUEST_SENT.store(false, Ordering::Relaxed);
    ERROR.store(0, Ordering::Relaxed);
    REDIRECT.store(false, Ordering::Relaxed);
    DEADLINE_TICK.store(0, Ordering::Relaxed);

    // Strip a scheme if there is one.  https is not implemented and saying so
    // beats silently trying port 80.
    let rest = if let Some(r) = url.strip_prefix("http://") {
        r
    } else if url.starts_with("https://") {
        return fail(1, "[http] https is not implemented");
    } else {
        url
    };

    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    if authority.is_empty() {
        return fail(1, "[http] empty host");
    }

    // host[:port]
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => match p.parse::<u16>() {
            Ok(v) => (h, v),
            Err(_) => (authority, 80),
        },
        None => (authority, 80),
    };
    if host.is_empty() || host.len() > 127 {
        return fail(1, "[http] host name too long");
    }

    PORT.store(port, Ordering::Relaxed);
    copy_into(core::ptr::addr_of_mut!(TARGET) as *mut u8, 128, &TARGET_LEN, host);
    build_request(host, path);

    serial::print_str("[http] GET http://");
    serial::print_str(host);
    serial::print_str(path);
    serial::print_str("\n");

    // An address literal needs no lookup.
    if let Some(ip) = crate::net::arp::parse_ip(host) {
        PHASE.store(phase_to_u8(Phase::Connecting), Ordering::Relaxed);
        begin_connect(ip);
        return true;
    }

    if dns::server() == [0, 0, 0, 0] {
        return fail(2, "[http] no DNS server configured");
    }
    PHASE.store(phase_to_u8(Phase::Resolving), Ordering::Relaxed);
    if !dns::start(host) {
        return fail(3, "[http] could not send the DNS query");
    }
    true
}

fn fail(code: u8, msg: &str) -> bool {
    ERROR.store(code, Ordering::Relaxed);
    PHASE.store(phase_to_u8(Phase::Failed), Ordering::Relaxed);
    serial::print_str(msg);
    serial::print_str("\n");
    false
}

/// Kick off a connection, retrying once if the next hop still needs an ARP
/// exchange — the frame cannot be built without a destination MAC.
fn begin_connect(ip: [u8; 4]) {
    if !tcp::connect(ip, PORT.load(Ordering::Relaxed)) {
        serial::print_str("[http] waiting for ARP\n");
    }
}

fn copy_into(dst: *mut u8, cap: usize, len: &AtomicU32, s: &str) {
    for (i, b) in s.bytes().enumerate() {
        if i >= cap {
            break;
        }
        unsafe { core::ptr::write_volatile(dst.add(i), b) };
    }
    len.store(s.len().min(cap) as u32, Ordering::Relaxed);
}

/// Build the request once, so a retransmission sends the same bytes.
fn build_request(host: &str, path: &str) {
    let mut n = 0usize;
    let base = core::ptr::addr_of_mut!(REQ_BUF) as *mut u8;
    let mut push = |s: &str, n: &mut usize| {
        for &b in s.as_bytes() {
            if *n >= 512 {
                return;
            }
            unsafe { core::ptr::write_volatile(base.add(*n), b) };
            *n += 1;
        }
    };
    push("GET ", &mut n);
    push(path, &mut n);
    push(" HTTP/1.0\r\nHost: ", &mut n);
    push(host, &mut n);
    push("\r\nUser-Agent: barryOS/0.13\r\n", &mut n);
    push("Accept: text/html, text/plain, */*\r\n", &mut n);
    // No keep-alive: the server closing is how we know the body is complete.
    push("Connection: close\r\n\r\n", &mut n);
    REQ_LEN.store(n as u32, Ordering::Relaxed);
}


/// Advance the fetch.  Called once per input-loop pass.
pub fn tick() {
    match phase() {
        Phase::Resolving => {
            dns::tick();
            match dns::state() {
                2 => {
                    if let Some(ip) = dns::take_result() {
                        PHASE.store(phase_to_u8(Phase::Connecting), Ordering::Relaxed);
                        REMOTE.store(crate::net::arp::ip_to_u32(ip), Ordering::Relaxed);
                        begin_connect(ip);
                    }
                }
                3 => {
                    ERROR.store(3, Ordering::Relaxed);
                    PHASE.store(phase_to_u8(Phase::Failed), Ordering::Relaxed);
                }
                _ => {}
            }
        }
        Phase::Connecting => {
            tcp::tick();
            match tcp::state() {
                tcp::State::Established => {
                    PHASE.store(phase_to_u8(Phase::Sending), Ordering::Relaxed);
                }
                tcp::State::Failed => {
                    ERROR.store(4, Ordering::Relaxed);
                    PHASE.store(phase_to_u8(Phase::Failed), Ordering::Relaxed);
                }
                tcp::State::Closed => {
                    // The SYN never went out because the next hop was still
                    // being resolved.  Now that it is, try again.  The address
                    // to look up is the *next hop*, not the destination: for
                    // anything off our subnet the frame goes to the gateway,
                    // and the destination's own MAC never appears in the cache.
                    let ip = crate::net::arp::u32_to_ip(REMOTE.load(Ordering::Relaxed));
                    if ip != [0, 0, 0, 0]
                        && crate::net::arp::lookup(crate::net::ipv4::next_hop(ip)).is_some()
                    {
                        begin_connect(ip);
                    }
                }
                _ => {}
            }
        }
        Phase::Sending => {
            tcp::tick();
            if !REQUEST_SENT.load(Ordering::Relaxed) {
                let n = REQ_LEN.load(Ordering::Relaxed) as usize;
                let mut req = [0u8; 512];
                let base = core::ptr::addr_of!(REQ_BUF) as *const u8;
                for (i, slot) in req.iter_mut().enumerate().take(n.min(512)) {
                    *slot = unsafe { core::ptr::read_volatile(base.add(i)) };
                }
                if tcp::send(&req[..n.min(512)]) {
                    REQUEST_SENT.store(true, Ordering::Relaxed);
                    PHASE.store(phase_to_u8(Phase::Receiving), Ordering::Relaxed);
                    serial::print_str("[http] request sent (");
                    serial::print_dec(n as u64);
                    serial::print_str(" bytes)\n");
                }
            }
            if tcp::state() == tcp::State::Failed {
                ERROR.store(5, Ordering::Relaxed);
                PHASE.store(phase_to_u8(Phase::Failed), Ordering::Relaxed);
            }
        }
        Phase::Receiving => {
            tcp::tick();
            if !HEADERS_PARSED.load(Ordering::Relaxed) {
                parse_headers();
            }
            // HTTP/1.0 with Connection: close means the peer's FIN is the end
            // of the body.  A declared Content-Length lets us finish sooner.
            let declared = body_declared();
            if declared != 0 && body_len() >= declared {
                tcp::close();
                PHASE.store(phase_to_u8(Phase::Done), Ordering::Relaxed);
                log_done();
                return;
            }
            match tcp::state() {
                tcp::State::Failed => {
                    ERROR.store(6, Ordering::Relaxed);
                    PHASE.store(phase_to_u8(Phase::Failed), Ordering::Relaxed);
                }
                tcp::State::Closed | tcp::State::FinSent if tcp::peer_closed() => {
                    PHASE.store(phase_to_u8(Phase::Done), Ordering::Relaxed);
                    log_done();
                }
                _ => {}
            }
        }
        _ => {}
    }

    // One deadline for the whole fetch, so a peer that goes silent in the
    // middle does not leave the browser spinning forever.
    if !matches!(phase(), Phase::Idle | Phase::Done | Phase::Failed) {
        let now = crate::interrupts::TIMER_TICKS.load(Ordering::Relaxed) as u32;
        let d = DEADLINE_TICK.load(Ordering::Relaxed);
        if d == 0 {
            DEADLINE_TICK.store(now, Ordering::Relaxed);
        } else if now.wrapping_sub(d) > DEADLINE_SECS * 100 {
            ERROR.store(7, Ordering::Relaxed);
            PHASE.store(phase_to_u8(Phase::Failed), Ordering::Relaxed);
            tcp::reset();
            serial::print_str("[http] timed out\n");
        }
    }
}

fn log_done() {
    serial::print_str("[http] done: status ");
    serial::print_dec(status_code() as u64);
    serial::print_str(", ");
    serial::print_dec(body_len() as u64);
    serial::print_str(" bytes of body");
    if tcp::rx_truncated() {
        serial::print_str(" (truncated: receive buffer full)");
    }
    serial::print_str("\n");
}


/// Find the end of the headers and pick out the fields worth keeping.
fn parse_headers() {
    let mut all = [0u8; tcp::RX_CAP];
    let n = tcp::rx_peek(&mut all);
    if n < 12 {
        return;                                 // wait for more
    }

    // Status line: HTTP/1.x NNN ...
    if !all.starts_with(b"HTTP/") {
        // Could still be mid-arrival; give up only on a clearly wrong byte.
        if all[0] != b'H' {
            ERROR.store(6, Ordering::Relaxed);
            PHASE.store(phase_to_u8(Phase::Failed), Ordering::Relaxed);
        }
        return;
    }
    let mut i = 0usize;
    while i < n && all[i] != b' ' {
        i += 1;
    }
    if i + 3 < n {
        let code = ((all[i + 1] - b'0') as u16) * 100
            + ((all[i + 2] - b'0') as u16) * 10
            + (all[i + 3] - b'0') as u16;
        STATUS.store(code, Ordering::Relaxed);
    }

    // Header block ends at CRLFCRLF.
    let Some(end) = find_headers_end(&all[..n]) else { return };
    HEADERS_PARSED.store(true, Ordering::Relaxed);
    BODY_OFFSET.store((end + 4) as u32, Ordering::Relaxed);

    // Walk the header lines.
    let mut line_start = 0usize;
    while line_start < end {
        let line_end = match all[line_start..end].iter().position(|&b| b == b'\r') {
            Some(k) => line_start + k,
            None => break,
        };
        let line = &all[line_start..line_end];
        if let Some(v) = header_value(line, b"content-length:") {
            BODY_DECLARED.store(parse_dec(v) as u32, Ordering::Relaxed);
        } else if header_value(line, b"location:").is_some() {
            REDIRECT.store(true, Ordering::Relaxed);
        }
        line_start = line_end + 2;
        if line_start >= end {
            break;
        }
    }

    serial::print_str("[http] headers parsed, body starts at ");
    serial::print_dec(end as u64 + 4);
    if body_declared() != 0 {
        serial::print_str(", Content-Length ");
        serial::print_dec(body_declared() as u64);
    }
    serial::print_str("\n");
}

fn find_headers_end(buf: &[u8]) -> Option<usize> {
    for i in 0..buf.len().saturating_sub(3) {
        if &buf[i..i + 4] == b"\r\n\r\n" {
            return Some(i);
        }
    }
    None
}

/// Case-insensitive header lookup, returning the value with leading spaces
/// trimmed.
fn header_value<'a>(line: &'a [u8], name: &[u8]) -> Option<&'a [u8]> {
    if line.len() < name.len() {
        return None;
    }
    for i in 0..name.len() {
        if line[i].to_ascii_lowercase() != name[i] {
            return None;
        }
    }
    let mut v = &line[name.len()..];
    while !v.is_empty() && (v[0] == b' ' || v[0] == b'\t') {
        v = &v[1..];
    }
    Some(v)
}

fn parse_dec(v: &[u8]) -> u64 {
    let mut n = 0u64;
    for &b in v {
        if !b.is_ascii_digit() {
            break;
        }
        n = n * 10 + (b - b'0') as u64;
    }
    n
}

/// Are we in the middle of a fetch?
pub fn in_progress() -> bool {
    matches!(phase(),
             Phase::Resolving | Phase::Connecting | Phase::Sending | Phase::Receiving)
}

/// One-word state for a status bar.
pub fn phase_str() -> &'static str {
    match phase() {
        Phase::Idle => "idle",
        Phase::Resolving => "resolving",
        Phase::Connecting => "connecting",
        Phase::Sending => "sending",
        Phase::Receiving => "receiving",
        Phase::Done => "done",
        Phase::Failed => "failed",
    }
}

/// Where the fetch has got to, for the browser's status line.
pub fn phase_name() -> &'static str {
    phase_str()
}
