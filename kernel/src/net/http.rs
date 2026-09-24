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

use crate::crypto::tls;
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

/// Is this fetch going through TLS?  Set by the scheme in the URL, and read
/// by every step that has to choose between the record layer and the raw
/// one.
static USE_TLS: AtomicBool = AtomicBool::new(false);

/// The Location header of the response, if it had one.
static mut LOCATION: [u8; 256] = [0; 256];
static LOCATION_LEN: AtomicU32 = AtomicU32::new(0);

/// Redirects followed so far, and the most we will follow.  A page that
/// redirects to itself would otherwise loop forever, and each hop costs a DNS
/// lookup, a connection and a request.
static HOPS: AtomicU8 = AtomicU8::new(0);
const MAX_HOPS: u8 = 5;

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
        8 => "too many redirects",
        _ => "",
    }
}

pub fn is_redirect() -> bool {
    REDIRECT.load(Ordering::Relaxed)
}

/// Bytes received on whichever transport this fetch is using.
fn rx_len() -> usize {
    if USE_TLS.load(Ordering::Relaxed) { tls::rx_len() } else { tcp::rx_len() }
}

fn rx_read_at(offset: usize, out: &mut [u8]) -> usize {
    if USE_TLS.load(Ordering::Relaxed) {
        tls::rx_read_at(offset, out)
    } else {
        tcp::rx_read_at(offset, out)
    }
}

/// Number of body bytes actually received.
pub fn body_len() -> usize {
    rx_len().saturating_sub(body_offset())
}

/// Copy part of the body out of the receive buffer.
pub fn read_body(offset: usize, out: &mut [u8]) -> usize {
    rx_read_at(body_offset() + offset, out)
}

/// Begin fetching `url`, following no redirects.
pub fn start(url: &str) -> bool {
    HOPS.store(0, Ordering::Relaxed);
    begin(url)
}

/// Begin fetching `url`, keeping the redirect count — a hop must not reset the
/// budget, which is exactly how a redirect loop would get in.
fn begin(url: &str) -> bool {
    tcp::reset();
    PHASE.store(phase_to_u8(Phase::Idle), Ordering::Relaxed);
    STATUS.store(0, Ordering::Relaxed);
    BODY_OFFSET.store(0, Ordering::Relaxed);
    BODY_DECLARED.store(0, Ordering::Relaxed);
    HEADERS_PARSED.store(false, Ordering::Relaxed);
    REQUEST_SENT.store(false, Ordering::Relaxed);
    ERROR.store(0, Ordering::Relaxed);
    REDIRECT.store(false, Ordering::Relaxed);
    LOCATION_LEN.store(0, Ordering::Relaxed);
    DEADLINE_TICK.store(0, Ordering::Relaxed);

    // The scheme decides the port and whether the record layer is TLS.  There
    // is no fallback from https to http: a client that quietly downgrades is
    // worse than one that refuses.
    //
    // A bare host means https.  Typing "example.com" and sending it in clear
    // is a decision the user did not make and cannot see, and every site this
    // browser is meant to reach answers on 443 anyway — the ones that do not
    // would have answered with a redirect to it.
    let (rest, tls_on) = if let Some(r) = url.strip_prefix("https://") {
        (r, true)
    } else if let Some(r) = url.strip_prefix("http://") {
        (r, false)
    } else {
        (url, true)
    };
    USE_TLS.store(tls_on, Ordering::Relaxed);

    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    if authority.is_empty() {
        return fail(1, "[http] empty host");
    }

    // host[:port]
    let default_port = if tls_on { 443 } else { 80 };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => match p.parse::<u16>() {
            Ok(v) => (h, v),
            Err(_) => (authority, default_port),
        },
        None => (authority, default_port),
    };
    if host.is_empty() || host.len() > 127 {
        return fail(1, "[http] host name too long");
    }

    PORT.store(port, Ordering::Relaxed);
    copy_into(core::ptr::addr_of_mut!(TARGET) as *mut u8, 128, &TARGET_LEN, host);
    build_request(host, path);

    serial::print_str("[http] GET ");
    serial::print_str(if tls_on { "https://" } else { "http://" });
    serial::print_str(host);
    serial::print_str(path);
    serial::print_str("\n");

    // An https fetch is handed to the TLS client whole, and not just because
    // that is tidier: TLS has to see the connection from the SYN up, since its
    // handshake keys are bound to the name it resolves and the socket it opens
    // underneath.  Opening one here as well would leave two SYNs and a client
    // whose record layer belongs to neither.
    if tls_on {
        PHASE.store(phase_to_u8(Phase::Connecting), Ordering::Relaxed);
        if !tls::start(host, port) {
            serial::print_str("[http] ");
            serial::print_str(tls::error());
            serial::print_str("\n");
            return fail(4, "[http] could not start the TLS handshake");
        }
        return true;
    }

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
            if USE_TLS.load(Ordering::Relaxed) {
                // TLS owns the socket: it drives DNS, TCP and then its own
                // handshake, and reports when it is ready to carry bytes.
                tls::tick();
                match tls::phase() {
                    tls::Phase::Established => {
                        PHASE.store(phase_to_u8(Phase::Sending), Ordering::Relaxed);
                    }
                    tls::Phase::Failed | tls::Phase::Closed => {
                        serial::print_str("[http] ");
                        serial::print_str(tls::error());
                        serial::print_str("\n");
                        ERROR.store(4, Ordering::Relaxed);
                        PHASE.store(phase_to_u8(Phase::Failed), Ordering::Relaxed);
                    }
                    _ => {}
                }
                return;
            }
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
            if USE_TLS.load(Ordering::Relaxed) { tls::tick(); } else { tcp::tick(); }
            if !REQUEST_SENT.load(Ordering::Relaxed) {
                let n = REQ_LEN.load(Ordering::Relaxed) as usize;
                let mut req = [0u8; 512];
                let base = core::ptr::addr_of!(REQ_BUF) as *const u8;
                for (i, slot) in req.iter_mut().enumerate().take(n.min(512)) {
                    *slot = unsafe { core::ptr::read_volatile(base.add(i)) };
                }
                let sent = if USE_TLS.load(Ordering::Relaxed) {
                    tls::send(&req[..n.min(512)])
                } else {
                    tcp::send(&req[..n.min(512)])
                };
                if sent {
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
            if USE_TLS.load(Ordering::Relaxed) { tls::tick(); } else { tcp::tick(); }
            if !HEADERS_PARSED.load(Ordering::Relaxed) {
                parse_headers();
                if HEADERS_PARSED.load(Ordering::Relaxed) {
                    log_receiving();
                }
            }
            // A redirect is decided as soon as the status and Location are in:
            // there is no point reading the body of a 301, and on a long one
            // that is most of the fetch.  The status alone is not enough —
            // Content-Length being present does not survive a reconnect.
            if is_redirect() && LOCATION_LEN.load(Ordering::Relaxed) != 0 {
                follow_redirect();
                return;
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
            let transport_done = if USE_TLS.load(Ordering::Relaxed) {
                tls::phase() == tls::Phase::Closed
            } else {
                matches!(tcp::state(), tcp::State::Closed | tcp::State::FinSent)
                    && tcp::peer_closed()
            };
            if transport_done {
                PHASE.store(phase_to_u8(Phase::Done), Ordering::Relaxed);
                log_done();
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
            log_receiving();
            serial::print_str("[http] timed out\n");
        }
    }
}

/// One line about where a fetch has got to: the status, what length the
/// headers promised, and how much has arrived.  Printed when the headers are
/// parsed and again if the whole thing times out — which is the case where it
/// matters, because "20 seconds, no body" and "20 seconds, body complete but
/// the connection never closed" are different bugs.
fn log_receiving() {
    serial::print_str("[http] status ");
    serial::print_dec(STATUS.load(Ordering::Relaxed) as u64);
    serial::print_str(", declared ");
    serial::print_dec(BODY_DECLARED.load(Ordering::Relaxed) as u64);
    serial::print_str(", body so far ");
    serial::print_dec(body_len() as u64);
    serial::print_str(", wire ");
    serial::print_dec(rx_len() as u64);
    if USE_TLS.load(Ordering::Relaxed) {
        serial::print_str(", tls ");
        serial::print_str(match crate::crypto::tls::phase() {
            crate::crypto::tls::Phase::Established => "established",
            crate::crypto::tls::Phase::Closed => "closed",
            crate::crypto::tls::Phase::Failed => "failed",
            crate::crypto::tls::Phase::Handshaking => "handshaking",
            crate::crypto::tls::Phase::Connecting => "connecting",
            crate::crypto::tls::Phase::Idle => "idle",
        });
    }
    serial::print_str("\n");
}

fn log_done() {    serial::print_str("[http] done: status ");
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
    let n = rx_read_at(0, &mut all);
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
        } else if let Some(v) = header_value(line, b"location:") {
            REDIRECT.store(true, Ordering::Relaxed);
            let dst = core::ptr::addr_of_mut!(LOCATION) as *mut u8;
            let n = v.len().min(256);
            for i in 0..n {
                unsafe { core::ptr::write_volatile(dst.add(i), v[i]) };
            }
            LOCATION_LEN.store(n as u32, Ordering::Relaxed);
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

/// Follow the Location the response carried.
///
/// Location comes in three shapes and all three turn up in the wild: a full
/// URL, a root-relative path, and a bare relative one.  The last two have to
/// be resolved against the host we asked for — and against the scheme we asked
/// with, which is why the scheme is not hardcoded here: a relative redirect
/// from an https page is an https URL, and resolving it to http would take a
/// page the user reached safely and fetch its next request in clear.
///
/// An absolute redirect is followed in either direction except one: https to
/// http is refused.  Upgrading is what half the web does on the first request;
/// downgrading is a page deciding to expose the next one.
fn follow_redirect() {
    let hops = HOPS.fetch_add(1, Ordering::Relaxed) + 1;
    if hops > MAX_HOPS {
        ERROR.store(8, Ordering::Relaxed);
        PHASE.store(phase_to_u8(Phase::Failed), Ordering::Relaxed);
        tcp::reset();
        serial::print_str("[http] gave up after ");
        serial::print_dec(hops as u64);
        serial::print_str(" redirects\n");
        return;
    }

    let mut loc = [0u8; 256];
    let n = (LOCATION_LEN.load(Ordering::Relaxed) as usize).min(256);
    let src = core::ptr::addr_of!(LOCATION) as *const u8;
    for (i, slot) in loc.iter_mut().enumerate().take(n) {
        *slot = unsafe { core::ptr::read_volatile(src.add(i)) };
    }
    let loc = core::str::from_utf8(&loc[..n]).unwrap_or("");

    let was_tls = USE_TLS.load(Ordering::Relaxed);
    let mut url = [0u8; 384];
    let mut len = 0usize;
    {
        let mut push = |s: &str, len: &mut usize| {
            for &b in s.as_bytes() {
                if *len >= url.len() {
                    return;
                }
                url[*len] = b;
                *len += 1;
            }
        };
        if loc.starts_with("https://") {
            push(loc, &mut len);
        } else if loc.starts_with("http://") {
            if was_tls {
                ERROR.store(1, Ordering::Relaxed);
                PHASE.store(phase_to_u8(Phase::Failed), Ordering::Relaxed);
                tcp::reset();
                serial::print_str("[http] refusing to downgrade to http\n");
                return;
            }
            push(loc, &mut len);
        } else {
            push(if was_tls { "https://" } else { "http://" }, &mut len);
            let mut host = [0u8; 128];
            let hn = target_into(&mut host);
            push(core::str::from_utf8(&host[..hn]).unwrap_or(""), &mut len);
            if !loc.starts_with('/') {
                push("/", &mut len);
            }
            push(loc, &mut len);
        }
    }
    let target = core::str::from_utf8(&url[..len]).unwrap_or("");

    serial::print_str("[http] redirect ");
    serial::print_dec(STATUS.load(Ordering::Relaxed) as u64);
    serial::print_str(" -> ");
    serial::print_str(target);
    serial::print_str("\n");

    begin(target);
}

/// How many redirects the last fetch followed — shown in the status line.
pub fn hops() -> u8 {
    HOPS.load(Ordering::Relaxed)
}

/// Is we in the middle of a fetch?
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
