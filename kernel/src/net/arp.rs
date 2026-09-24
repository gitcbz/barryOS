//! ARP: address resolution, and the first protocol that makes the NIC
//! demonstrably useful — a request goes out and a reply comes back.
//!
//! Only the four things needed to be useful: build a request, recognise a
//! reply, answer requests aimed at us, and remember a handful of mappings.

use crate::net::e1000;
use crate::serial;
use core::sync::atomic::{AtomicU32, Ordering};

pub const ETHERTYPE_ARP: u16 = 0x0806;
pub const ARP_REQUEST: u16 = 1;
pub const ARP_REPLY: u16 = 2;

/// Our address.  There is no DHCP yet, so it is set by hand (see `net::init`).
static OUR_IP: AtomicU32 = AtomicU32::new(0);

/// Small cache of resolved mappings.
const CACHE_SIZE: usize = 8;
#[derive(Clone, Copy)]
struct Entry {
    ip: u32,
    mac: [u8; 6],
    valid: bool,
}
static mut CACHE: [Entry; CACHE_SIZE] = [Entry { ip: 0, mac: [0; 6], valid: false }; CACHE_SIZE];

/// Requests sent and replies received, for diagnostics.
pub static REQUESTS_SENT: AtomicU32 = AtomicU32::new(0);
pub static REPLIES_SEEN: AtomicU32 = AtomicU32::new(0);

pub fn our_ip() -> [u8; 4] {
    u32_to_ip(OUR_IP.load(Ordering::Relaxed))
}

pub fn set_our_ip(ip: [u8; 4]) {
    OUR_IP.store(ip_to_u32(ip), Ordering::Relaxed);
}

pub fn ip_to_u32(ip: [u8; 4]) -> u32 {
    ((ip[0] as u32) << 24) | ((ip[1] as u32) << 16) | ((ip[2] as u32) << 8) | ip[3] as u32
}

pub fn u32_to_ip(v: u32) -> [u8; 4] {
    [
        (v >> 24) as u8,
        (v >> 16) as u8,
        (v >> 8) as u8,
        v as u8,
    ]
}

/// Look up a cached mapping.
pub fn lookup(ip: [u8; 4]) -> Option<[u8; 6]> {
    let key = ip_to_u32(ip);
    for i in 0..CACHE_SIZE {
        let e = unsafe { &*(core::ptr::addr_of!(CACHE) as *const Entry).add(i) };
        if e.valid && e.ip == key {
            return Some(e.mac);
        }
    }
    None
}

fn remember(ip: u32, mac: [u8; 6]) {
    // Overwrite a matching entry, else the first free one, else slot 0 — a
    // ring would be nicer but this is a boot-time convenience.
    let base = unsafe { core::ptr::addr_of_mut!(CACHE) as *mut Entry };
    let mut slot = 0usize;
    for i in 0..CACHE_SIZE {
        unsafe {
            let e = &mut *base.add(i);
            if e.valid && e.ip == ip {
                slot = i;
                break;
            }
            if !e.valid && slot == 0 {
                slot = i;
            }
        }
    }
    unsafe {
        let e = &mut *base.add(slot);
        e.ip = ip;
        e.mac = mac;
        e.valid = true;
    }
}

/// Send an ARP request asking who holds `target`.
pub fn request(target: [u8; 4]) -> bool {
    let Some(our_mac) = e1000::mac() else {
        return false;
    };
    let our = OUR_IP.load(Ordering::Relaxed);

    let mut frame = [0u8; 42];
    // Ethernet: broadcast destination.
    for b in frame.iter_mut().take(6) {
        *b = 0xFF;
    }
    frame[6..12].copy_from_slice(&our_mac);
    frame[12..14].copy_from_slice(&ETHERTYPE_ARP.to_be_bytes());

    // ARP payload.
    let p = &mut frame[14..];
    p[0..2].copy_from_slice(&1u16.to_be_bytes());          // htype = Ethernet
    p[2..4].copy_from_slice(&0x0800u16.to_be_bytes());     // ptype = IPv4
    p[4] = 6;                                              // hlen
    p[5] = 4;                                              // plen
    p[6..8].copy_from_slice(&ARP_REQUEST.to_be_bytes());
    p[8..14].copy_from_slice(&our_mac);                    // sender hardware
    p[14..18].copy_from_slice(&our.to_be_bytes());         // sender protocol
    // target hardware left as zeroes
    p[24..28].copy_from_slice(&ip_to_u32(target).to_be_bytes());

    if e1000::send_frame(&frame) {
        REQUESTS_SENT.fetch_add(1, Ordering::Relaxed);
        true
    } else {
        false
    }
}

/// Answer an ARP request aimed at us.
fn reply_to(frame: &[u8]) {
    let Some(our_mac) = e1000::mac() else {
        return;
    };
    let our = OUR_IP.load(Ordering::Relaxed);
    if frame.len() < 42 {
        return;
    }
    let p = &frame[14..];
    let sender_mac: [u8; 6] = p[8..14].try_into().unwrap_or([0; 6]);
    let sender_ip: [u8; 4] = p[14..18].try_into().unwrap_or([0; 4]);
    let target_ip = u32::from_be_bytes(p[24..28].try_into().unwrap_or([0; 4]));

    if target_ip != our {
        return;
    }

    let mut out = [0u8; 42];
    out[0..6].copy_from_slice(&sender_mac);
    out[6..12].copy_from_slice(&our_mac);
    out[12..14].copy_from_slice(&ETHERTYPE_ARP.to_be_bytes());
    let q = &mut out[14..];
    q[0..2].copy_from_slice(&1u16.to_be_bytes());
    q[2..4].copy_from_slice(&0x0800u16.to_be_bytes());
    q[4] = 6;
    q[5] = 4;
    q[6..8].copy_from_slice(&ARP_REPLY.to_be_bytes());
    q[8..14].copy_from_slice(&our_mac);
    q[14..18].copy_from_slice(&our.to_be_bytes());
    q[18..24].copy_from_slice(&sender_mac);
    q[24..28].copy_from_slice(&sender_ip);
    e1000::send_frame(&out);
}

/// Handle one received Ethernet frame.
///
/// The receive ring is drained by `net::poll`, which routes by ethertype and
/// calls this for the ARP ones — this layer does not own the adapter.
pub fn handle_frame(frame: &[u8]) {
    if frame.len() < 14 {
        return;
    }
    let ethertype = u16::from_be_bytes([frame[12], frame[13]]);
    if ethertype != ETHERTYPE_ARP || frame.len() < 42 {
        return;
    }
    let p = &frame[14..];
    let oper = u16::from_be_bytes([p[6], p[7]]);
    let sender_mac: [u8; 6] = p[8..14].try_into().unwrap_or([0; 6]);
    let sender_ip: [u8; 4] = p[14..18].try_into().unwrap_or([0; 4]);

    match oper {
        ARP_REPLY => {
            REPLIES_SEEN.fetch_add(1, Ordering::Relaxed);
            remember(ip_to_u32(sender_ip), sender_mac);
            serial::print_str("[arp] reply: ");
            print_ip(sender_ip);
            serial::print_str(" is at ");
            print_mac(&sender_mac);
            serial::print_str("\n");
        }
        ARP_REQUEST => {
            serial::print_str("[arp] request for ");
            print_ip(u32_to_ip(u32::from_be_bytes(
                p[24..28].try_into().unwrap_or([0; 4]),
            )));
            serial::print_str(" from ");
            print_ip(sender_ip);
            serial::print_str("\n");
            reply_to(frame);
        }
        _ => {}
    }
}

pub fn print_mac(m: &[u8; 6]) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for (i, b) in m.iter().enumerate() {
        if i > 0 {
            serial::print_str(":");
        }
        let s = [HEX[(b >> 4) as usize], HEX[(b & 0xF) as usize]];
        serial::print_str(core::str::from_utf8(&s).unwrap_or("??"));
    }
}

pub fn print_ip(ip: [u8; 4]) {
    for (i, b) in ip.iter().enumerate() {
        if i > 0 {
            serial::print_str(".");
        }
        print_dec(*b as u64);
    }
}

fn print_dec(mut v: u64) {
    let mut buf = [0u8; 20];
    let mut i = buf.len();
    if v == 0 {
        i -= 1;
        buf[i] = b'0';
    }
    while v > 0 {
        i -= 1;
        buf[i] = b'0' + (v % 10) as u8;
        v /= 10;
    }
    if let Ok(s) = core::str::from_utf8(&buf[i..]) {
        serial::print_str(s);
    }
}

/// Parse "a.b.c.d" into four octets.
pub fn parse_ip(s: &str) -> Option<[u8; 4]> {
    let mut out = [0u8; 4];
    let mut idx = 0usize;
    let mut cur: u32 = 0;
    let mut digits = 0;
    for ch in s.bytes() {
        match ch {
            b'0'..=b'9' => {
                cur = cur * 10 + (ch - b'0') as u32;
                digits += 1;
                if cur > 255 || digits > 3 {
                    return None;
                }
            }
            b'.' => {
                if digits == 0 || idx >= 3 {
                    return None;
                }
                out[idx] = cur as u8;
                idx += 1;
                cur = 0;
                digits = 0;
            }
            _ => return None,
        }
    }
    if digits == 0 || idx != 3 {
        return None;
    }
    out[3] = cur as u8;
    Some(out)
}
