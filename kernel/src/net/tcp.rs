//! TCP: a client, for one connection at a time.
//!
//! One connection, because that is what the thing above it does — the browser
//! resolves a name, connects, sends a request, reads the answer and closes,
//! and it is doing one of those at a time.  A table of control blocks would be
//! a lot of code that nothing would exercise, and untested code in a protocol
//! with sequence numbers is worse than a documented limit.
//!
//! What is here is a real handshake and a real close: SYN, SYN|ACK, ACK to get
//! up; FIN and the ACK for it to get down.  Data goes out one segment at a
//! time and is retransmitted on a timer; data comes in only in order, and a
//! segment that arrives early is dropped rather than queued, which costs a
//! retransmission and nothing else.

use crate::interrupts;
use crate::net::{arp, ipv4};
use crate::serial;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};

/// Outgoing segment payload cap: the Ethernet MTU minus 20 bytes of IPv4 and
/// 20 of TCP, minus a little for options we do not send anyway.
const MAX_SEG: usize = 1460;

/// Receive buffer.  A page that does not fit is truncated and the truncation
/// is reported rather than silently swallowed.
pub const RX_CAP: usize = 32 * 1024;

const RETRY_TICKS: u64 = 100;           // one second at 100 Hz
const MAX_RETRIES: u32 = 5;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    Closed,
    SynSent,
    Established,
    FinWait,
    FinSent,
    Failed,
}

fn state_from_u8(v: u8) -> State {
    match v {
        1 => State::SynSent,
        2 => State::Established,
        3 => State::FinWait,
        4 => State::FinSent,
        5 => State::Failed,
        _ => State::Closed,
    }
}

fn state_to_u8(s: State) -> u8 {
    match s {
        State::Closed => 0,
        State::SynSent => 1,
        State::Established => 2,
        State::FinWait => 3,
        State::FinSent => 4,
        State::Failed => 5,
    }
}

// --- control block ---------------------------------------------------------

static ACTIVE: AtomicBool = AtomicBool::new(false);
static STATE: AtomicU8 = AtomicU8::new(0);
static REMOTE_IP: AtomicU32 = AtomicU32::new(0);
static REMOTE_PORT: AtomicU32 = AtomicU32::new(0);
static LOCAL_PORT: AtomicU32 = AtomicU32::new(0);
static SND_UNA: AtomicU32 = AtomicU32::new(0);   // oldest byte not yet acked
static SND_NXT: AtomicU32 = AtomicU32::new(0);   // next byte we will send
static RCV_NXT: AtomicU32 = AtomicU32::new(0);   // next byte we expect
static LAST_SEND: AtomicU64 = AtomicU64::new(0);
static RETRIES: AtomicU32 = AtomicU32::new(0);
static ISS: AtomicU32 = AtomicU32::new(0);

static mut TX_SEG: [u8; MAX_SEG] = [0; MAX_SEG];
static TX_LEN: AtomicU32 = AtomicU32::new(0);
/// Set when the segment in TX_SEG carries FIN as well as data.
static TX_FIN: AtomicBool = AtomicBool::new(false);

static mut RX_BUF: [u8; RX_CAP] = [0; RX_CAP];
static RX_LEN: AtomicU32 = AtomicU32::new(0);
static RX_TRUNCATED: AtomicBool = AtomicBool::new(false);
/// Set once the peer has sent FIN — everything received is then final.
static PEER_CLOSED: AtomicBool = AtomicBool::new(false);

pub static CONNECTS: AtomicU64 = AtomicU64::new(0);
pub static SEGMENTS_IN: AtomicU64 = AtomicU64::new(0);
pub static RETRANSMITS: AtomicU64 = AtomicU64::new(0);

/// Diagnostic log switch.  The receive path runs on every segment and the log
/// is slow, so it is off once a page is flowing and on while a connection is
/// being set up, which is when a failure is interesting.
static VERBOSE: AtomicBool = AtomicBool::new(true);

pub fn state() -> State {
    state_from_u8(STATE.load(Ordering::Relaxed))
}

pub fn rx_len() -> usize {
    RX_LEN.load(Ordering::Relaxed) as usize
}

pub fn rx_truncated() -> bool {
    RX_TRUNCATED.load(Ordering::Relaxed)
}

pub fn peer_closed() -> bool {
    PEER_CLOSED.load(Ordering::Relaxed)
}

/// Copy out what has arrived so far, without consuming it.
pub fn rx_peek(out: &mut [u8]) -> usize {
    let n = rx_len().min(out.len());
    let base = core::ptr::addr_of!(RX_BUF) as *const u8;
    for i in 0..n {
        out[i] = unsafe { core::ptr::read_volatile(base.add(i)) };
    }
    n
}

/// Copy `out.len()` bytes starting at `offset`, without consuming anything.
///
/// This exists so a caller that only wants a window of the response — the
/// browser painting the part that is on screen — does not have to copy the
/// whole 32 KiB buffer to get at it.
pub fn rx_read_at(offset: usize, out: &mut [u8]) -> usize {
    let total = rx_len();
    if offset >= total {
        return 0;
    }
    let n = (total - offset).min(out.len());
    let base = core::ptr::addr_of!(RX_BUF) as *const u8;
    for i in 0..n {
        out[i] = unsafe { core::ptr::read_volatile(base.add(offset + i)) };
    }
    n
}

/// Forget the connection.  Does not send anything: a client that wants to say
/// goodbye calls `close` first.
pub fn reset() {
    ACTIVE.store(false, Ordering::Relaxed);
    STATE.store(0, Ordering::Relaxed);
    RX_LEN.store(0, Ordering::Relaxed);
    RX_TRUNCATED.store(false, Ordering::Relaxed);
    PEER_CLOSED.store(false, Ordering::Relaxed);
    TX_LEN.store(0, Ordering::Relaxed);
    TX_FIN.store(false, Ordering::Relaxed);
    RETRIES.store(0, Ordering::Relaxed);
    VERBOSE.store(true, Ordering::Relaxed);
}

/// Is a connection up, or on its way up?
pub fn busy() -> bool {
    matches!(state(), State::SynSent | State::Established | State::FinWait)
}

// --- sending ---------------------------------------------------------------

/// Start a connection.  Returns false when the next hop still needs resolving.
pub fn connect(ip: [u8; 4], port: u16) -> bool {
    reset();
    REMOTE_IP.store(arp::ip_to_u32(ip), Ordering::Relaxed);
    REMOTE_PORT.store(port as u32, Ordering::Relaxed);
    LOCAL_PORT.store(crate::net::udp::alloc_port() as u32, Ordering::Relaxed);

    // The initial sequence number has to differ between connections, and in
    // particular between *boots*.  This used to be a fixed base plus the tick
    // counter, which meant a boot that reached the same point at the same time
    // picked the same ISN as the boot before it — and any half-open connection
    // still held by a NAT or a server then swallowed the new SYN, answering
    // with an acknowledgement of the old one.  The cycle counter costs nothing
    // and is different every time.
    let iss = {
        let tsc = unsafe { core::arch::x86_64::_rdtsc() } as u32;
        let ticks = interrupts::TIMER_TICKS.load(Ordering::Relaxed) as u32;
        tsc.rotate_left(13) ^ ticks.wrapping_mul(0x9E37_79B9)
    };
    ISS.store(iss, Ordering::Relaxed);
    SND_UNA.store(iss, Ordering::Relaxed);
    SND_NXT.store(iss.wrapping_add(1), Ordering::Relaxed);   // SYN takes one
    RCV_NXT.store(0, Ordering::Relaxed);

    STATE.store(state_to_u8(State::SynSent), Ordering::Relaxed);
    ACTIVE.store(true, Ordering::Relaxed);
    VERBOSE.store(true, Ordering::Relaxed);
    CONNECTS.fetch_add(1, Ordering::Relaxed);

    if !send_segment(&[], true, false, false) {
        // The only reason a send fails here is an unresolved next hop: `send`
        // fires an ARP request and returns false, and the caller is expected
        // to come back once it is answered.  That is not a failure of the
        // connection, so leave the state as "not started" rather than "dead".
        STATE.store(state_to_u8(State::Closed), Ordering::Relaxed);
        return false;
    }
    serial::print_str("[tcp] SYN -> ");
    ipv4::log_ip(ip);
    serial::print_str(":");
    serial::print_dec(port as u64);
    serial::print_str("\n");
    true
}

/// Queue and send `data` on the established connection.
pub fn send(data: &[u8]) -> bool {
    if state() != State::Established {
        return false;
    }
    if data.len() > MAX_SEG {
        return false;
    }
    let base = core::ptr::addr_of_mut!(TX_SEG) as *mut u8;
    for (i, &b) in data.iter().enumerate() {
        unsafe { core::ptr::write_volatile(base.add(i), b) };
    }
    TX_LEN.store(data.len() as u32, Ordering::Relaxed);
    TX_FIN.store(false, Ordering::Relaxed);
    RETRIES.store(0, Ordering::Relaxed);

    if !send_segment(&[], false, false, true) {
        return false;
    }
    SND_NXT.fetch_add(data.len() as u32, Ordering::Relaxed);
    true
}

/// Say goodbye.  The connection is not gone until the peer's FIN arrives.
pub fn close() {
    if state() != State::Established {
        return;
    }
    TX_LEN.store(0, Ordering::Relaxed);
    TX_FIN.store(true, Ordering::Relaxed);
    STATE.store(state_to_u8(State::FinWait), Ordering::Relaxed);
    SND_NXT.fetch_add(1, Ordering::Relaxed);        // FIN takes one
    RETRIES.store(0, Ordering::Relaxed);
    send_segment(&[], false, true, true);
}

/// Build and emit one segment.
///
/// `payload` is used when the caller already has the bytes in hand (the SYN,
/// where there are none); `from_tx_buf` sends what is in TX_SEG, which is the
/// retransmittable path.
fn send_segment(payload: &[u8], syn: bool, fin: bool, from_tx_buf: bool) -> bool {
    let src_port = LOCAL_PORT.load(Ordering::Relaxed) as u16;
    let dst_port = REMOTE_PORT.load(Ordering::Relaxed) as u16;
    let dst = arp::u32_to_ip(REMOTE_IP.load(Ordering::Relaxed));

    let body_len = if from_tx_buf && !syn {
        TX_LEN.load(Ordering::Relaxed) as usize
    } else {
        payload.len()
    };

    let mut seg = [0u8; 20 + MAX_SEG];
    seg[0..2].copy_from_slice(&src_port.to_be_bytes());
    seg[2..4].copy_from_slice(&dst_port.to_be_bytes());

    let seq = if syn { ISS.load(Ordering::Relaxed) } else { SND_UNA.load(Ordering::Relaxed) };
    seg[4..8].copy_from_slice(&seq.to_be_bytes());
    seg[8..12].copy_from_slice(&RCV_NXT.load(Ordering::Relaxed).to_be_bytes());

    // Data offset 5 (20-byte header), then the flags.
    seg[12] = 5 << 4;
    let mut flags = 0x10u8;                          // ACK is on everything but the first SYN
    if syn { flags = 0x02; }
    if fin { flags |= 0x01; }
    seg[13] = flags;
    seg[14..16].copy_from_slice(&0x4000u16.to_be_bytes());   // window
    // Checksum at 16..18; urgent pointer zero.

    if body_len > 0 {
        let base = core::ptr::addr_of!(TX_SEG) as *const u8;
        for i in 0..body_len {
            seg[20 + i] = if from_tx_buf {
                unsafe { core::ptr::read_volatile(base.add(i)) }
            } else {
                payload[i]
            };
        }
    }

    let n = 20 + body_len;
    let ck = checksum(ipv4::our_ip(), dst, &seg[..n]);
    seg[16..18].copy_from_slice(&(if ck == 0 { 0xFFFF } else { ck }).to_be_bytes());

    if VERBOSE.load(Ordering::Relaxed) {
        serial::print_str("[tcp] out: seq ");
        serial::print_hex(seq as u64);
        serial::print_str(" ack ");
        serial::print_hex(RCV_NXT.load(Ordering::Relaxed) as u64);
        serial::print_str(" flags 0x");
        serial::print_hex(flags as u64);
        serial::print_str(" len ");
        serial::print_dec(body_len as u64);
        serial::print_str(" from port ");
        serial::print_dec(src_port as u64);
        serial::print_str(" to ");
        serial::print_dec(dst_port as u64);
        serial::print_str("\n");
    }

    LAST_SEND.store(interrupts::TIMER_TICKS.load(Ordering::Relaxed), Ordering::Relaxed);
    let sent = ipv4::send(dst, ipv4::PROTO_TCP, &seg[..n]);
    if VERBOSE.load(Ordering::Relaxed) && !sent {
        serial::print_str("[tcp]   (not sent: next hop unresolved)\n");
    }
    sent
}

/// TCP checksum: pseudo-header, then the segment.
fn checksum(src: [u8; 4], dst: [u8; 4], seg: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    for pair in src.chunks(2).chain(dst.chunks(2)) {
        sum += u16::from_be_bytes([pair[0], pair[1]]) as u32;
    }
    sum += ipv4::PROTO_TCP as u32;
    sum += seg.len() as u32;

    let mut i = 0;
    while i + 1 < seg.len() {
        sum += u16::from_be_bytes([seg[i], seg[i + 1]]) as u32;
        i += 2;
    }
    if i < seg.len() {
        sum += (seg[i] as u32) << 8;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16)
}

/// Send a bare ACK for the current receive position.
fn send_ack() {
    send_segment(&[], false, false, false);
}

// --- receiving -------------------------------------------------------------

/// Handle one TCP segment (the IPv4 payload).
pub fn handle(src: [u8; 4], body: &[u8]) {
    if !ACTIVE.load(Ordering::Relaxed) || body.len() < 20 {
        return;
    }
    if arp::ip_to_u32(src) != REMOTE_IP.load(Ordering::Relaxed) {
        return;
    }
    let dst_port = u16::from_be_bytes([body[2], body[3]]);
    if dst_port as u32 != LOCAL_PORT.load(Ordering::Relaxed) {
        return;
    }
    if u16::from_be_bytes([body[0], body[1]]) as u32
        != REMOTE_PORT.load(Ordering::Relaxed)
    {
        return;
    }

    let data_off = ((body[12] >> 4) as usize) * 4;
    if data_off < 20 || body.len() < data_off {
        return;
    }
    let flags = body[13];
    let seq = u32::from_be_bytes(body[4..8].try_into().unwrap_or([0; 4]));
    let ack = u32::from_be_bytes(body[8..12].try_into().unwrap_or([0; 4]));
    let payload = &body[data_off..];
    SEGMENTS_IN.fetch_add(1, Ordering::Relaxed);

    let syn = flags & 0x02 != 0;
    let fin = flags & 0x01 != 0;
    let rst = flags & 0x04 != 0;

    if VERBOSE.load(Ordering::Relaxed) {
        serial::print_str("[tcp] in: seq ");
        serial::print_hex(seq as u64);
        serial::print_str(" ack ");
        serial::print_hex(ack as u64);
        serial::print_str(" flags 0x");
        serial::print_hex(flags as u64);
        serial::print_str(" len ");
        serial::print_dec(payload.len() as u64);
        serial::print_str("\n[tcp] raw: ");
        for i in 0..20.min(body.len()) {
            let b = body[i];
            let hex = b"0123456789ABCDEF";
            let s = [hex[(b >> 4) as usize], hex[(b & 0xF) as usize], b' '];
            serial::print_str(core::str::from_utf8(&s).unwrap_or("??"));
        }
        serial::print_str("\n");
    }

    if rst {
        serial::print_str("[tcp] connection reset by peer\n");
        STATE.store(state_to_u8(State::Failed), Ordering::Relaxed);
        return;
    }

    // Anything the peer has acknowledged is no longer outstanding.
    if flags & 0x10 != 0 && ack == SND_NXT.load(Ordering::Relaxed) {
        SND_UNA.store(ack, Ordering::Relaxed);
        TX_LEN.store(0, Ordering::Relaxed);
        RETRIES.store(0, Ordering::Relaxed);
    }

    match state() {
        State::SynSent => {
            if syn && flags & 0x10 != 0 {
                // Their SYN consumes one sequence number.
                RCV_NXT.store(seq.wrapping_add(1), Ordering::Relaxed);
                SND_UNA.store(ack, Ordering::Relaxed);
                SND_NXT.store(ack, Ordering::Relaxed);
                STATE.store(state_to_u8(State::Established), Ordering::Relaxed);
                send_ack();
                serial::print_str("[tcp] established\n");
                // The handshake is the part worth reading in a log; a page
                // arriving is dozens of segments and none of them are news.
                VERBOSE.store(false, Ordering::Relaxed);
            }
        }
        State::Established | State::FinWait | State::FinSent => {
            let mut consumed = 0usize;
            if !payload.is_empty() {
                if seq == RCV_NXT.load(Ordering::Relaxed) {
                    let n = append_rx(payload);
                    consumed = payload.len();
                    RCV_NXT.fetch_add(n as u32, Ordering::Relaxed);
                    // Everything beyond the buffer is dropped, but the
                    // sequence number still advances so the transfer can end.
                    if n < payload.len() {
                        RCV_NXT.fetch_add((payload.len() - n) as u32, Ordering::Relaxed);
                        RX_TRUNCATED.store(true, Ordering::Relaxed);
                    }
                } else {
                    // Out of order.  Dropping it costs a retransmission from
                    // the peer; there is no reassembly queue to put it in.
                    serial::print_str("[tcp] out-of-order segment dropped\n");
                }
            }

            if fin {
                PEER_CLOSED.store(true, Ordering::Relaxed);
                RCV_NXT.fetch_add(1, Ordering::Relaxed);
                send_ack();
                // Both directions are done once we have sent our own FIN.
                if state() == State::FinWait || state() == State::FinSent {
                    STATE.store(state_to_u8(State::Closed), Ordering::Relaxed);
                    ACTIVE.store(false, Ordering::Relaxed);
                    serial::print_str("[tcp] closed\n");
                    return;
                }
                STATE.store(state_to_u8(State::FinSent), Ordering::Relaxed);
                return;
            }

            if consumed > 0 || !payload.is_empty() {
                send_ack();
            }
            // Wait for the server's FIN even after our FIN has been acked.
            if state() == State::FinWait && TX_FIN.load(Ordering::Relaxed)
                && SND_UNA.load(Ordering::Relaxed)
                    == SND_NXT.load(Ordering::Relaxed)
            {
                STATE.store(state_to_u8(State::FinSent), Ordering::Relaxed);
            }
        }
        _ => {}
    }
}

fn append_rx(data: &[u8]) -> usize {
    let used = rx_len();
    if used >= RX_CAP {
        return 0;
    }
    let n = data.len().min(RX_CAP - used);
    let base = core::ptr::addr_of_mut!(RX_BUF) as *mut u8;
    for i in 0..n {
        unsafe { core::ptr::write_volatile(base.add(used + i), data[i]) };
    }
    RX_LEN.store((used + n) as u32, Ordering::Relaxed);
    n
}

// --- timers ----------------------------------------------------------------

/// Retransmit unacknowledged data, and give up on a connection that has gone
/// quiet.
pub fn tick() {
    if !ACTIVE.load(Ordering::Relaxed) {
        return;
    }
    let now = interrupts::TIMER_TICKS.load(Ordering::Relaxed);
    if now.saturating_sub(LAST_SEND.load(Ordering::Relaxed)) < RETRY_TICKS {
        return;
    }
    let outstanding = SND_NXT.load(Ordering::Relaxed) != SND_UNA.load(Ordering::Relaxed);
    let waiting_fin = TX_FIN.load(Ordering::Relaxed) && state() == State::FinWait;
    if !outstanding && !waiting_fin && state() != State::SynSent {
        return;
    }

    let n = RETRIES.fetch_add(1, Ordering::Relaxed) + 1;
    if n > MAX_RETRIES {
        serial::print_str("[tcp] gave up after ");
        serial::print_dec(n as u64);
        serial::print_str(" retransmissions\n");
        STATE.store(state_to_u8(State::Failed), Ordering::Relaxed);
        // Tear the connection down rather than leaving it to be retried: the
        // previous version cleared only the state, so the next tick saw a
        // still-active connection and announced giving up again, once per
        // retry interval, for as long as the machine was up.
        ACTIVE.store(false, Ordering::Relaxed);
        return;
    }
    RETRANSMITS.fetch_add(1, Ordering::Relaxed);

    if state() == State::SynSent {
        send_segment(&[], true, false, false);
    } else if waiting_fin {
        send_segment(&[], false, true, false);
    } else {
        send_segment(&[], false, false, true);
    }
}

/// One line for a status bar or a log.
pub fn describe_state() -> &'static str {
    match state() {
        State::Closed => "closed",
        State::SynSent => "connecting",
        State::Established => "open",
        State::FinWait => "closing",
        State::FinSent => "closing",
        State::Failed => "failed",
    }
}
