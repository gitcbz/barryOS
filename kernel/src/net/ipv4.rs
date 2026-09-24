//! IPv4: the layer that turns a datagram into a frame and back.
//!
//! Everything above this — ICMP, UDP, TCP — hands `send` a protocol number and
//! a payload and gets a frame on the wire; everything below it is `arp` and
//! the adapter.  The three things this layer actually owns are the header, the
//! checksum, and the routing decision that is really just "is the destination
//! on our subnet, and if not, who is the next hop".
//!
//! Fragmentation is not implemented in either direction: nothing we send is
//! near 1500 bytes (a DNS query is 40, a DHCP packet 300, an HTTP request a
//! few hundred), and a reassembly buffer would be dead code.  Incoming
//! fragments are counted and dropped rather than silently reassembled wrong.

use crate::net::{arp, e1000, icmp, tcp, udp};
use crate::serial;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

pub const ETHERTYPE_IPV4: u16 = 0x0800;

pub const PROTO_ICMP: u8 = 1;
pub const PROTO_TCP: u8 = 6;
pub const PROTO_UDP: u8 = 17;

/// IPv4 broadcast, as an address.
pub const BROADCAST: [u8; 4] = [255, 255, 255, 255];

/// The interface address itself lives in `arp`, which was the first layer to
/// need it.  These are the rest of what a DHCP lease hands out.
static MASK: AtomicU32 = AtomicU32::new(0xFFFF_FF00);   // 255.255.255.0
static GATEWAY: AtomicU32 = AtomicU32::new(0);

/// Ours.
pub fn our_ip() -> [u8; 4] {
    arp::our_ip()
}

pub fn set_mask(m: [u8; 4]) {
    MASK.store(arp::ip_to_u32(m), Ordering::Relaxed);
}

pub fn mask() -> [u8; 4] {
    arp::u32_to_ip(MASK.load(Ordering::Relaxed))
}

pub fn set_gateway(g: [u8; 4]) {
    GATEWAY.store(arp::ip_to_u32(g), Ordering::Relaxed);
}

pub fn gateway() -> [u8; 4] {
    arp::u32_to_ip(GATEWAY.load(Ordering::Relaxed))
}

/// Is `ip` reachable without a router?
pub fn on_link(ip: [u8; 4]) -> bool {
    let a = arp::ip_to_u32(our_ip());
    let b = arp::ip_to_u32(ip);
    let m = MASK.load(Ordering::Relaxed);
    (a & m) == (b & m)
}

/// The address a frame for `ip` actually goes to.
pub fn next_hop(ip: [u8; 4]) -> [u8; 4] {
    let g = gateway();
    if on_link(ip) || g == [0, 0, 0, 0] {
        ip
    } else {
        g
    }
}

/// Internet checksum (RFC 1071) over `data`.
///
/// Sum 16-bit big-endian words with end-around carry, then complement.  An odd
/// length is padded with a zero byte.
pub fn checksum(data: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    let mut i = 0;
    while i + 1 < data.len() {
        sum += u16::from_be_bytes([data[i], data[i + 1]]) as u32;
        i += 2;
    }
    if i < data.len() {
        sum += (data[i] as u32) << 8;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16)
}

/// Diagnostic counters.
pub static SENT: AtomicU64 = AtomicU64::new(0);
pub static RECEIVED: AtomicU64 = AtomicU64::new(0);
pub static BAD_CHECKSUM: AtomicU64 = AtomicU64::new(0);
pub static FRAGMENTS: AtomicU64 = AtomicU64::new(0);

/// Send one datagram.  Returns false when the next hop's MAC is not in the
/// ARP cache yet — in which case an ARP request has just gone out and the
/// caller should come back and try again.
pub fn send(dst: [u8; 4], proto: u8, payload: &[u8]) -> bool {
    let Some(our_mac) = e1000::mac() else {
        return false;
    };
    let total = 20 + payload.len();
    if total > 1500 {
        return false;                       // would need fragmentation
    }

    // Resolve the link-layer address first: there is no point building a
    // header for a frame we cannot address.
    let hop = next_hop(dst);
    let Some(hop_mac) = arp::lookup(hop) else {
        arp::request(hop);
        return false;
    };

    let mut frame = [0u8; 1514];
    frame[0..6].copy_from_slice(&hop_mac);
    frame[6..12].copy_from_slice(&our_mac);
    frame[12..14].copy_from_slice(&ETHERTYPE_IPV4.to_be_bytes());

    let h = &mut frame[14..34];
    h[0] = 0x45;                            // version 4, IHL 5 (20-byte header)
    h[1] = 0;                               // DSCP/ECN
    h[2..4].copy_from_slice(&(total as u16).to_be_bytes());
    h[4..6].copy_from_slice(&crate::net::ip_id().to_be_bytes());
    h[6..8].copy_from_slice(&0u16.to_be_bytes());   // flags + fragment offset
    h[8] = 64;                              // TTL
    h[9] = proto;
    h[10..12].copy_from_slice(&0u16.to_be_bytes()); // checksum placeholder
    h[12..16].copy_from_slice(&our_ip());
    h[16..20].copy_from_slice(&dst);

    let ck = checksum(h);
    h[10..12].copy_from_slice(&ck.to_be_bytes());

    frame[34..34 + payload.len()].copy_from_slice(payload);
    let n = 34 + payload.len();

    if e1000::send_frame(&frame[..n]) {
        SENT.fetch_add(1, Ordering::Relaxed);
        true
    } else {
        false
    }
}

/// Send a datagram as a link-layer broadcast, with an explicit source address.
///
/// This is the one packet there is nothing to resolve for: DHCP has to talk to
/// a server before it has an address (so ARP, which is *about* addresses, is
/// useless) and before it has a gateway to send through.
pub fn send_broadcast(src: [u8; 4], dst: [u8; 4], proto: u8, payload: &[u8]) -> bool {
    let Some(our_mac) = e1000::mac() else {
        return false;
    };
    let total = 20 + payload.len();
    if total > 1500 {
        return false;
    }

    let mut frame = [0u8; 1514];
    for b in frame[..6].iter_mut() {
        *b = 0xFF;
    }
    frame[6..12].copy_from_slice(&our_mac);
    frame[12..14].copy_from_slice(&ETHERTYPE_IPV4.to_be_bytes());

    let h = &mut frame[14..34];
    h[0] = 0x45;
    h[2..4].copy_from_slice(&(total as u16).to_be_bytes());
    h[4..6].copy_from_slice(&crate::net::ip_id().to_be_bytes());
    h[8] = 64;
    h[9] = proto;
    h[12..16].copy_from_slice(&src);
    h[16..20].copy_from_slice(&dst);
    let ck = checksum(h);
    h[10..12].copy_from_slice(&ck.to_be_bytes());

    frame[34..34 + payload.len()].copy_from_slice(payload);
    let n = 34 + payload.len();

    if e1000::send_frame(&frame[..n]) {
        SENT.fetch_add(1, Ordering::Relaxed);
        true
    } else {
        false
    }
}

/// Handle one IPv4 frame (the whole Ethernet frame, 14-byte header included).
pub fn handle_frame(frame: &[u8]) -> bool {
    if frame.len() < 34 {
        return false;
    }
    let ethertype = u16::from_be_bytes([frame[12], frame[13]]);
    if ethertype != ETHERTYPE_IPV4 {
        return false;
    }
    let pkt = &frame[14..];

    let ihl = ((pkt[0] & 0x0F) as usize) * 4;
    if pkt[0] >> 4 != 4 || ihl < 20 || pkt.len() < ihl {
        return false;
    }
    let total = u16::from_be_bytes([pkt[2], pkt[3]]) as usize;
    if total < ihl || total > pkt.len() {
        return false;
    }
    if checksum(&pkt[..ihl]) != 0 {
        BAD_CHECKSUM.fetch_add(1, Ordering::Relaxed);
        return false;
    }

    // Fragment offset or MF set: we have no reassembly buffer, so say so by
    // counting rather than pretending.
    let flags_frag = u16::from_be_bytes([pkt[6], pkt[7]]);
    if flags_frag & 0x3FFF != 0 {
        FRAGMENTS.fetch_add(1, Ordering::Relaxed);
        return false;
    }

    let src: [u8; 4] = pkt[12..16].try_into().unwrap_or([0; 4]);
    let dst: [u8; 4] = pkt[16..20].try_into().unwrap_or([0; 4]);
    let proto = pkt[9];
    let body = &pkt[ihl..total];

    // Accept anything addressed to us, plus broadcasts: DHCP replies arrive
    // before the address is configured, so "to us" cannot be the only test.
    let ours = arp::ip_to_u32(dst) == arp::ip_to_u32(our_ip());
    if !ours && dst != BROADCAST {
        return false;
    }

    RECEIVED.fetch_add(1, Ordering::Relaxed);

    match proto {
        PROTO_ICMP => icmp::handle(src, body),
        PROTO_UDP => udp::handle(src, body),
        PROTO_TCP => tcp::handle(src, body),
        _ => {}
    }
    true
}

/// Print one address as a.b.c.d to the serial log.
pub fn log_ip(ip: [u8; 4]) {
    arp::print_ip(ip);
}

/// One line describing where we are on the network, for the boot log.
pub fn print_config() {
    serial::print_str("[ip] address ");
    arp::print_ip(our_ip());
    serial::print_str(" mask ");
    arp::print_ip(mask());
    serial::print_str(" gateway ");
    arp::print_ip(gateway());
    serial::print_str("\n");
}
