//! UDP: datagrams, and the port table that delivers them.
//!
//! Connectionless, so there is no state machine here — a datagram goes out,
//! a datagram comes in, and the only bookkeeping is which local port each
//! arriving one belongs to.  DHCP and DNS both sit directly on this and both
//! are implemented as state machines of their own, so the table exists to keep
//! this layer from having to know about either of them.

use crate::net::ipv4;
use crate::serial;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

pub static SENT: AtomicU64 = AtomicU64::new(0);
pub static RECEIVED: AtomicU64 = AtomicU64::new(0);
pub static NO_LISTENER: AtomicU64 = AtomicU64::new(0);

/// Ports handed out to outgoing requests, so replies can be matched.
///
/// 0 means "not seeded yet".  The starting point is taken from the cycle
/// counter rather than being the fixed 49152 it used to be, because a fixed
/// one meant every boot asked for the same first port — and a NAT holding a
/// stale mapping for `192.168.91.135:49153` then treats the new connection as
/// a continuation of the old one, acknowledging sequence numbers from a
/// previous boot and never answering the new SYN.
static NEXT_PORT: AtomicU32 = AtomicU32::new(0);

/// Lowest port we will hand out.  Below this is the system range.
const EPHEMERAL_BASE: u32 = 0xC000;
const EPHEMERAL_SPAN: u32 = 0x3F00;

/// Allocate a source port for a request we are about to send.
pub fn alloc_port() -> u16 {
    let mut p = NEXT_PORT.load(Ordering::Relaxed);
    if p == 0 {
        p = EPHEMERAL_BASE
            + (unsafe { core::arch::x86_64::_rdtsc() } as u32) % EPHEMERAL_SPAN;
    }
    let next = if p + 1 >= EPHEMERAL_BASE + EPHEMERAL_SPAN {
        EPHEMERAL_BASE
    } else {
        p + 1
    };
    NEXT_PORT.store(next, Ordering::Relaxed);
    p as u16
}

/// A sink for one local port.  Called with the sender's address and port and
/// the datagram body.
pub type Handler = fn(src: [u8; 4], src_port: u16, payload: &[u8]);

const MAX_LISTENERS: usize = 6;
struct Listener {
    port: u16,
    handler: Handler,
}
static mut LISTENERS: [Option<Listener>; MAX_LISTENERS] = [const { None }; MAX_LISTENERS];
static LISTENER_COUNT: AtomicU32 = AtomicU32::new(0);

/// Register (or re-register) the handler for a local port.  Idempotent, so a
/// protocol can call it from its own `init` without worrying about order.
pub fn listen(port: u16, handler: Handler) {
    let base = core::ptr::addr_of_mut!(LISTENERS) as *mut Option<Listener>;
    let n = LISTENER_COUNT.load(Ordering::Relaxed) as usize;
    for i in 0..n {
        if let Some(l) = unsafe { (*base.add(i)).as_mut() } {
            if l.port == port {
                l.handler = handler;
                return;
            }
        }
    }
    if n < MAX_LISTENERS {
        unsafe { *base.add(n) = Some(Listener { port, handler }); }
        LISTENER_COUNT.store(n as u32 + 1, Ordering::Relaxed);
    }
}

/// Build and send a datagram.  Returns false when the next hop is not yet
/// resolved — the caller should retry after the ARP exchange completes.
pub fn send(dst: [u8; 4], dst_port: u16, src_port: u16, payload: &[u8]) -> bool {
    send_from(ipv4::our_ip(), dst, dst_port, src_port, payload)
}

/// As `send`, but with an explicit source address.  DHCP needs this: its first
/// packets go out from 0.0.0.0, because the whole point is that we do not have
/// an address yet.
pub fn send_from(src: [u8; 4], dst: [u8; 4], dst_port: u16, src_port: u16, payload: &[u8]) -> bool {
    let mut dgram = [0u8; 1472];
    let len = 8 + payload.len();
    if len > dgram.len() {
        return false;
    }
    dgram[0..2].copy_from_slice(&src_port.to_be_bytes());
    dgram[2..4].copy_from_slice(&dst_port.to_be_bytes());
    dgram[4..6].copy_from_slice(&(len as u16).to_be_bytes());
    // Checksum at 6..8 stays zero for now: 0 is a legal "not computed" in
    // IPv4, and it is what most stacks send.
    dgram[8..8 + payload.len()].copy_from_slice(payload);

    let ck = checksum(src, dst, &dgram[..len]);
    // A computed checksum of zero must be transmitted as 0xFFFF, or the
    // receiver reads it as "not computed".
    dgram[6..8].copy_from_slice(&(if ck == 0 { 0xFFFF } else { ck }).to_be_bytes());

    // A datagram from 0.0.0.0, or addressed to the broadcast address, has no
    // next hop to resolve — it goes straight out on the wire.  That is the
    // only reason DHCP can run at all: it speaks before we have an address.
    let ok = if src == [0, 0, 0, 0] || dst == ipv4::BROADCAST {
        ipv4::send_broadcast(src, dst, ipv4::PROTO_UDP, &dgram[..len])
    } else {
        ipv4::send(dst, ipv4::PROTO_UDP, &dgram[..len])
    };

    if ok {
        SENT.fetch_add(1, Ordering::Relaxed);
        true
    } else {
        false
    }
}

/// UDP checksum: the pseudo-header, then the datagram.
fn checksum(src: [u8; 4], dst: [u8; 4], dgram: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    // Pseudo-header: source, destination, zero, protocol, length.
    for pair in src.chunks(2).chain(dst.chunks(2)) {
        sum += u16::from_be_bytes([pair[0], pair[1]]) as u32;
    }
    sum += ipv4::PROTO_UDP as u32;
    sum += dgram.len() as u32;

    let mut i = 0;
    while i + 1 < dgram.len() {
        sum += u16::from_be_bytes([dgram[i], dgram[i + 1]]) as u32;
        i += 2;
    }
    if i < dgram.len() {
        sum += (dgram[i] as u32) << 8;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16)
}

/// Handle one UDP datagram (the IPv4 payload).
pub fn handle(src: [u8; 4], body: &[u8]) {
    if body.len() < 8 {
        return;
    }
    let src_port = u16::from_be_bytes([body[0], body[1]]);
    let dst_port = u16::from_be_bytes([body[2], body[3]]);
    let len = u16::from_be_bytes([body[4], body[5]]) as usize;
    if len < 8 || len > body.len() {
        return;
    }
    RECEIVED.fetch_add(1, Ordering::Relaxed);
    let payload = &body[8..len];

    let base = core::ptr::addr_of!(LISTENERS) as *const Option<Listener>;
    let n = LISTENER_COUNT.load(Ordering::Relaxed) as usize;
    for i in 0..n {
        if let Some(l) = unsafe { (*base.add(i)).as_ref() } {
            if l.port == dst_port {
                (l.handler)(src, src_port, payload);
                return;
            }
        }
    }
    NO_LISTENER.fetch_add(1, Ordering::Relaxed);
    serial::print_str("[udp] no listener for port ");
    serial::print_hex(dst_port as u64);
    serial::print_str(" from ");
    ipv4::log_ip(src);
    serial::print_str("\n");
}
