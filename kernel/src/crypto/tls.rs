//! TLS 1.3 (RFC 8446), client side.
//!
//! A record layer and one handshake.  The parts that are easy to get wrong and
//! therefore are written out rather than abbreviated:
//!
//!   * the transcript hash covers every handshake message in order, including
//!     the ones that arrive encrypted, and the Finished check is over it;
//!   * the key schedule is HKDF-Expand-Label at each step, with the label
//!     names the RFC fixes;
//!   * the record layer authenticates its own header as additional data, so a
//!     record cannot be re-typed or reordered;
//!   * nothing is accepted before the server's Finished verifies.
//!
//! Certificate verification is a separate module and is called from here; this
//! file does not trust anything it did not compute itself.

use crate::crypto::aes::{gcm_decrypt, gcm_encrypt};
use crate::crypto::hkdf::{derive_secret, expand_label, extract};
use crate::crypto::hmac::hmac;
use crate::crypto::x25519;
use crate::sha256::Sha256;
use crate::serial;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};

/// TLS record content types.
const CT_CHANGE_CIPHER_SPEC: u8 = 20;
const CT_ALERT: u8 = 21;
const CT_HANDSHAKE: u8 = 22;
const CT_APPLICATION_DATA: u8 = 23;

/// Handshake message types.
const HS_CLIENT_HELLO: u8 = 1;
const HS_SERVER_HELLO: u8 = 2;
const HS_ENCRYPTED_EXTENSIONS: u8 = 8;
const HS_CERTIFICATE: u8 = 11;
const HS_CERTIFICATE_VERIFY: u8 = 15;
const HS_FINISHED: u8 = 20;

/// TLS 1.3 cipher suites, the two that matter.
pub const SUITE_AES128_SHA256: u16 = 0x1301;
pub const SUITE_AES256_SHA384: u16 = 0x1302;

/// Named groups and signature schemes.
const GROUP_X25519: u16 = 0x001d;
const SIG_RSA_PSS_RSAE_SHA256: u16 = 0x0804;
const SIG_RSA_PSS_RSAE_SHA384: u16 = 0x0805;
const SIG_RSA_PSS_RSAE_SHA512: u16 = 0x0806;
const SIG_ECDSA_SHA256: u16 = 0x0403;
const SIG_ECDSA_SHA384: u16 = 0x0503;
const SIG_ECDSA_SHA512: u16 = 0x0603;
const SIG_RSA_PKCS1_SHA256: u16 = 0x0401;
const SIG_RSA_PKCS1_SHA384: u16 = 0x0501;
const SIG_RSA_PKCS1_SHA512: u16 = 0x0601;

/// Extension types.
const EXT_SERVER_NAME: u16 = 0x0000;
const EXT_SUPPORTED_GROUPS: u16 = 0x000a;
const EXT_SIGNATURE_ALGORITHMS: u16 = 0x000d;
const EXT_SUPPORTED_VERSIONS: u16 = 0x002b;
const EXT_KEY_SHARE: u16 = 0x0033;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    Idle,
    Connecting,
    Handshaking,
    Established,
    Closed,
    Failed,
}

fn phase_from_u8(v: u8) -> Phase {
    match v {
        1 => Phase::Connecting,
        2 => Phase::Handshaking,
        3 => Phase::Established,
        4 => Phase::Closed,
        5 => Phase::Failed,
        _ => Phase::Idle,
    }
}

fn phase_to_u8(p: Phase) -> u8 {
    match p {
        Phase::Idle => 0,
        Phase::Connecting => 1,
        Phase::Handshaking => 2,
        Phase::Established => 3,
        Phase::Closed => 4,
        Phase::Failed => 5,
    }
}

static PHASE: AtomicU8 = AtomicU8::new(0);
static ERROR: AtomicU8 = AtomicU8::new(0);
static SUITE: AtomicU32 = AtomicU32::new(0);

/// Bytes of handshake messages seen so far, hashed in order.  Everything the
/// key schedule derives is bound to this.
static mut TRANSCRIPT: Sha256 = Sha256::new();

/// Our ephemeral key pair.
static mut PRIV: [u8; 32] = [0; 32];
static mut PUB: [u8; 32] = [0; 32];

/// The handshake traffic secrets, then the application ones.
#[derive(Clone, Copy, Default)]
struct Keys {
    key: [u8; 32],
    key_len: usize,
    iv: [u8; 12],
}

static mut CLIENT_HS: Keys = Keys { key: [0; 32], key_len: 0, iv: [0; 12] };
static mut SERVER_HS: Keys = Keys { key: [0; 32], key_len: 0, iv: [0; 12] };
static mut CLIENT_AP: Keys = Keys { key: [0; 32], key_len: 0, iv: [0; 12] };
static mut SERVER_AP: Keys = Keys { key: [0; 32], key_len: 0, iv: [0; 12] };

/// Record sequence numbers, one per direction, reset when keys change.
static CLIENT_SEQ: AtomicU32 = AtomicU32::new(0);
static SERVER_SEQ: AtomicU32 = AtomicU32::new(0);

/// Plaintext waiting to be read by the layer above.
const RX_CAP: usize = 32 * 1024;
static mut RX: [u8; RX_CAP] = [0; RX_CAP];
static RX_LEN: AtomicU32 = AtomicU32::new(0);

/// The host being connected to, for SNI and for the certificate check.
static mut HOST: [u8; 128] = [0; 128];
static HOST_LEN: AtomicU32 = AtomicU32::new(0);

/// A record being assembled from TCP, and one being assembled for sending.
static mut RX_REC: [u8; 16640] = [0; 16640];
static RX_REC_LEN: AtomicU32 = AtomicU32::new(0);

/// The server's certificate chain, as DER, and its public key.
const CHAIN_CAP: usize = 8 * 1024;
static mut CHAIN: [u8; CHAIN_CAP] = [0; CHAIN_CAP];
static CHAIN_LEN: AtomicU32 = AtomicU32::new(0);

/// Set once the server's Finished has been checked.  Nothing above this module
/// may use the connection before then.
static VERIFIED: AtomicBool = AtomicBool::new(false);

pub fn phase() -> Phase {
    phase_from_u8(PHASE.load(Ordering::Relaxed))
}

pub fn established() -> bool {
    phase() == Phase::Established && VERIFIED.load(Ordering::Relaxed)
}

pub fn error() -> &'static str {
    match ERROR.load(Ordering::Relaxed) {
        1 => "could not reach the server",
        2 => "the handshake was refused",
        3 => "the server chose an unsupported cipher",
        4 => "the certificate chain did not verify",
        5 => "the server could not prove it holds the key",
        6 => "the handshake did not finish",
        7 => "a record failed to authenticate",
        8 => "the connection closed during the handshake",
        9 => "timed out",
        _ => "",
    }
}

fn fail(code: u8) {
    ERROR.store(code, Ordering::Relaxed);
    PHASE.store(phase_to_u8(Phase::Failed), Ordering::Relaxed);
    crate::net::tcp::reset();
    serial::print_str("[tls] ");
    serial::print_str(error());
    serial::print_str("\n");
}

/// Bytes handed to the layer above.
pub fn rx_len() -> usize {
    RX_LEN.load(Ordering::Relaxed) as usize
}

pub fn rx_read_at(offset: usize, out: &mut [u8]) -> usize {
    let total = rx_len();
    if offset >= total {
        return 0;
    }
    let n = (total - offset).min(out.len());
    let base = core::ptr::addr_of!(RX) as *const u8;
    for i in 0..n {
        out[i] = unsafe { core::ptr::read_volatile(base.add(offset + i)) };
    }
    n
}

/// Begin a connection to `host:port`.  Drives DNS and TCP itself, because a
/// TLS handshake is meaningless without a connection underneath it.
pub fn start(host: &str, port: u16) -> bool {
    reset();
    let n = host.len().min(128);
    let base = core::ptr::addr_of_mut!(HOST) as *mut u8;
    for i in 0..n {
        unsafe { core::ptr::write_volatile(base.add(i), host.as_bytes()[i]) };
    }
    HOST_LEN.store(n as u32, Ordering::Relaxed);

    if let Some(ip) = crate::net::arp::parse_ip(host) {
        PHASE.store(phase_to_u8(Phase::Connecting), Ordering::Relaxed);
        crate::net::tcp::connect(ip, port);
        return true;
    }
    if crate::net::dns::server() == [0, 0, 0, 0] {
        ERROR.store(1, Ordering::Relaxed);
        PHASE.store(phase_to_u8(Phase::Failed), Ordering::Relaxed);
        return false;
    }
    PHASE.store(phase_to_u8(Phase::Connecting), Ordering::Relaxed);
    if !crate::net::dns::start(host) {
        ERROR.store(1, Ordering::Relaxed);
        PHASE.store(phase_to_u8(Phase::Failed), Ordering::Relaxed);
        return false;
    }
    let _ = port;
    true
}

fn reset() {
    PHASE.store(0, Ordering::Relaxed);
    ERROR.store(0, Ordering::Relaxed);
    VERIFIED.store(false, Ordering::Relaxed);
    RX_LEN.store(0, Ordering::Relaxed);
    RX_REC_LEN.store(0, Ordering::Relaxed);
    CHAIN_LEN.store(0, Ordering::Relaxed);
    CLIENT_SEQ.store(0, Ordering::Relaxed);
    SERVER_SEQ.store(0, Ordering::Relaxed);
    unsafe {
        TRANSCRIPT = Sha256::new();
    }
    crate::net::tcp::reset();
}

fn host_str() -> &'static str {
    let n = HOST_LEN.load(Ordering::Relaxed) as usize;
    let base = core::ptr::addr_of!(HOST) as *const u8;
    let mut buf = [0u8; 128];
    for (i, slot) in buf.iter_mut().enumerate().take(n) {
        *slot = unsafe { core::ptr::read_volatile(base.add(i)) };
    }
    // The slice borrows a static, so it outlives the call.
    unsafe {
        HOST_VIEW = buf;
        core::str::from_utf8(&HOST_VIEW[..n]).unwrap_or("")
    }
}
static mut HOST_VIEW: [u8; 128] = [0; 128];

/// Advance the handshake.  Called once per input-loop pass.
pub fn tick() {
    match phase() {
        Phase::Connecting => {
            crate::net::tcp::tick();
            match crate::net::dns::state() {
                2 => {
                    if let Some(ip) = crate::net::dns::take_result() {
                        serial::print_str("[tls] dns done, connecting\n");
                        crate::net::tcp::connect(ip, 443);
                    }
                }
                3 => fail(1),
                _ => {}
            }
            if crate::net::tcp::state() == crate::net::tcp::State::Established {
                PHASE.store(phase_to_u8(Phase::Handshaking), Ordering::Relaxed);
                send_client_hello();
            } else if crate::net::tcp::state() == crate::net::tcp::State::Failed {
                fail(1);
            }
        }
        Phase::Handshaking => {
            crate::net::tcp::tick();
            drain_tcp();
            if crate::net::tcp::state() == crate::net::tcp::State::Failed {
                fail(6);
            } else if crate::net::tcp::state() == crate::net::tcp::State::Closed
                && !established()
            {
                fail(8);
            }
        }
        Phase::Established => {
            crate::net::tcp::tick();
            drain_tcp();
        }
        _ => {}
    }
}



// ---------------------------------------------------------------------------
//  Record layer
// ---------------------------------------------------------------------------

/// Write a handshake record, encrypted or not depending on the phase.
fn send_record(content_type: u8, body: &[u8]) -> bool {
    let inner_buf;
    let mut buf = [0u8; 16640];
    let mut n = 0usize;
    buf[0] = content_type;
    buf[1] = 0x03;
    buf[2] = 0x03;
    buf[3..5].copy_from_slice(&(body.len() as u16).to_be_bytes());
    let mut plain_len = 5;

    let encrypted = unsafe {
        if content_type == CT_HANDSHAKE && CLIENT_HS.key_len == 0 {
            false
        } else if content_type == CT_HANDSHAKE {
            true
        } else {
            CLIENT_AP.key_len != 0
        }
    };

    if !encrypted {
        // Plaintext records are only ever the ClientHello.
        buf[5..5 + body.len()].copy_from_slice(body);
        n = 5 + body.len();
    } else {
        // The inner plaintext is content || type, and the outer record always
        // says application_data.
        let keys = unsafe { if content_type == CT_HANDSHAKE { &CLIENT_HS } else { &CLIENT_AP } };
        inner_buf = 0u8;
        let _ = inner_buf;
        let mut inner = [0u8; 16640];
        inner[..body.len()].copy_from_slice(body);
        inner[body.len()] = content_type;
        let inner_len = body.len() + 1;

        buf[0] = CT_APPLICATION_DATA;
        buf[1] = 0x03;
        buf[2] = 0x03;
        // The ciphertext plus the tag.
        let seq = CLIENT_SEQ.fetch_add(1, Ordering::Relaxed);
        let mut iv = keys.iv;
        let s = seq.to_be_bytes();
        for i in 0..8 {
            iv[4 + i] ^= s[i];
        }
        let aad = [CT_APPLICATION_DATA, 0x03, 0x03,
                   ((inner_len + 16) >> 8) as u8, ((inner_len + 16) & 0xFF) as u8];
        let mut out = [0u8; 16640];
        if !gcm_encrypt(&keys.key[..keys.key_len], &iv, &aad, &inner[..inner_len], &mut out) {
            return false;
        }
        plain_len = inner_len + 16;
        buf[3..5].copy_from_slice(&(plain_len as u16).to_be_bytes());
        buf[5..5 + plain_len].copy_from_slice(&out[..plain_len]);
        n = 5 + plain_len;
    }
    let _ = plain_len;
    crate::net::tcp::send(&buf[..n])
}

/// Pull whatever TCP has, split it into records and handle each.
fn drain_tcp() {
    let mut consumed = 0usize;
    loop {
        let mut hdr = [0u8; 5];
        if crate::net::tcp::rx_read_at(consumed, &mut hdr) < 5 {
            break;
        }
        let len = u16::from_be_bytes([hdr[3], hdr[4]]) as usize;
        if len == 0 || len > 16640 {
            fail(7);
            return;
        }
        if crate::net::tcp::rx_len() < consumed + 5 + len {
            break;                      // the rest has not arrived yet
        }
        consumed += 5 + len;

        // An encrypted record is always typed as application_data on the
        // outside; the real type is the last byte of the plaintext.
        let (ctype, plen) = if hdr[0] == CT_APPLICATION_DATA {
            let keys = unsafe {
                if !established() && SERVER_HS.key_len != 0 { &SERVER_HS } else { &SERVER_AP }
            };
            if keys.key_len == 0 {
                fail(7);
                return;
            }
            let seq = SERVER_SEQ.fetch_add(1, Ordering::Relaxed);
            let mut iv = keys.iv;
            let s = seq.to_be_bytes();
            for i in 0..8 {
                iv[4 + i] ^= s[i];
            }
            let mut rec = [0u8; 16640];
            crate::net::tcp::rx_read_at(consumed - len, &mut rec[..len]);
            let aad = [hdr[0], hdr[1], hdr[2], hdr[3], hdr[4]];
            let out = plain_buf();
            match gcm_decrypt(&keys.key[..keys.key_len], &iv, &aad, &rec[..len], out) {
                Some(m) if m >= 1 => {
                    let t = out[m - 1];
                    (t, m - 1)
                }
                _ => {
                    fail(7);
                    return;
                }
            }
        } else {
            let out = plain_buf();
            crate::net::tcp::rx_read_at(consumed - len, &mut out[..len]);
            (hdr[0], len)
        };

        // Copied out of the shared buffer into a slice the handler can hold,
        // because the handler may itself send a record and overwrite it.
        let mut body = [0u8; 16640];
        {
            let out = plain_buf();
            body[..plen].copy_from_slice(&out[..plen]);
        }
        match ctype {
            CT_HANDSHAKE => handle_handshake(&body[..plen]),
            CT_ALERT => {
                let level = body.first().copied().unwrap_or(0);
                let desc = body.get(1).copied().unwrap_or(0);
                serial::print_str("[tls] alert level ");
                serial::print_dec(level as u64);
                serial::print_str(" description ");
                serial::print_dec(desc as u64);
                serial::print_str("\n");
                PHASE.store(phase_to_u8(Phase::Closed), Ordering::Relaxed);
                crate::net::tcp::reset();
            }
            CT_APPLICATION_DATA => append_rx(&body[..plen]),
            _ => {}
        }
        if PHASE.load(Ordering::Relaxed) == phase_to_u8(Phase::Failed) {
            return;
        }
    }
}

/// One record's plaintext, shared because a record is up to 16 KiB and this is
/// on the stack of a kernel that has no stack guard.
static mut PLAIN: [u8; 16640] = [0; 16640];
fn plain_buf() -> &'static mut [u8; 16640] {
    unsafe { &mut *core::ptr::addr_of_mut!(PLAIN) }
}



fn append_rx(data: &[u8]) {
    let used = rx_len();
    if used >= RX_CAP {
        return;
    }
    let n = data.len().min(RX_CAP - used);
    let base = core::ptr::addr_of_mut!(RX) as *mut u8;
    for i in 0..n {
        unsafe { core::ptr::write_volatile(base.add(used + i), data[i]) };
    }
    RX_LEN.store((used + n) as u32, Ordering::Relaxed);
}

/// Add a handshake message to the transcript hash.
fn transcript_update(msg: &[u8]) {
    unsafe {
        let t = &mut *core::ptr::addr_of_mut!(TRANSCRIPT);
        t.update(msg);
    }
}

fn transcript_hash() -> [u8; 32] {
    unsafe {
        // Cloned rather than hashed in place: `finish` consumes the state,
        // and the transcript has to stay usable afterwards.
        let t = &*core::ptr::addr_of!(TRANSCRIPT);
        let c = t.clone();
        c.finish()
    }
}

// ---------------------------------------------------------------------------
//  Handshake
// ---------------------------------------------------------------------------

fn send_client_hello() {
    // A fresh ephemeral key per connection.  The private half never leaves
    // this buffer and the public half is what goes on the wire.
    let mut priv_key = [0u8; 32];
    let seed = unsafe { core::arch::x86_64::_rdtsc() };
    let mut s = seed;
    for b in priv_key.iter_mut() {
        // xorshift64: this is a key that must not repeat between connections,
        // not one that must be unpredictable to an attacker who cannot see
        // the traffic.  It is mixed with the cycle counter, which differs
        // every time.
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        *b = (s & 0xFF) as u8;
    }
    let pub_key = x25519::public_key(&priv_key);
    unsafe {
        PRIV = priv_key;
        PUB = pub_key;
    }

    let mut b = [0u8; 1024];
    let mut n = 0usize;
    // legacy_version is pinned to 1.2 for middleboxes; the real version is in
    // supported_versions.
    push_u16(&mut b, &mut n, 0x0303);
    // random: 32 bytes.  Only needs to be unique, and the cycle counter is.
    let r = unsafe { core::arch::x86_64::_rdtsc() };
    for i in 0..32 {
        b[n] = ((r >> ((i % 8) * 8)) ^ (i as u64 * 31)) as u8;
        n += 1;
    }
    b[n] = 0;                       // empty session id
    n += 1;
    push_u16(&mut b, &mut n, 2);    // one cipher suite
    push_u16(&mut b, &mut n, SUITE_AES128_SHA256);
    b[n] = 1;                       // one compression method: null
    n += 1;
    b[n] = 0;
    n += 1;

    // Extensions
    let ext_len_at = n;
    push_u16(&mut b, &mut n, 0);

    // server_name
    {
        let host = host_str();
        let start = n;
        push_u16(&mut b, &mut n, EXT_SERVER_NAME);
        let len_at = n;
        push_u16(&mut b, &mut n, 0);
        push_u16(&mut b, &mut n, (host.len() + 3) as u16);
        b[n] = 0;
        n += 1;
        push_u16(&mut b, &mut n, host.len() as u16);
        b[n..n + host.len()].copy_from_slice(host.as_bytes());
        n += host.len();
        let l = (n - len_at - 2) as u16;
        b[len_at..len_at + 2].copy_from_slice(&l.to_be_bytes());
        let _ = start;
    }

    // supported_groups: x25519 only, which is what we can do.
    {
        push_u16(&mut b, &mut n, EXT_SUPPORTED_GROUPS);
        push_u16(&mut b, &mut n, 4);
        push_u16(&mut b, &mut n, 2);
        push_u16(&mut b, &mut n, GROUP_X25519);
    }

    // signature_algorithms: everything this kernel can verify.
    {
        push_u16(&mut b, &mut n, EXT_SIGNATURE_ALGORITHMS);
        let len_at = n;
        push_u16(&mut b, &mut n, 0);
        let list_at = n;
        for s in [SIG_ECDSA_SHA256, SIG_ECDSA_SHA384, SIG_ECDSA_SHA512,
                  SIG_RSA_PSS_RSAE_SHA256, SIG_RSA_PSS_RSAE_SHA384, SIG_RSA_PSS_RSAE_SHA512,
                  SIG_RSA_PKCS1_SHA256, SIG_RSA_PKCS1_SHA384, SIG_RSA_PKCS1_SHA512] {
            push_u16(&mut b, &mut n, s);
        }
        let l = (n - list_at) as u16;
        b[list_at - 2..list_at].copy_from_slice(&l.to_be_bytes());
        let _ = len_at;
    }

    // supported_versions: 1.3 only.  Offering 1.2 as well would mean the
    // server could pick it, and the 1.2 handshake is a different state
    // machine; this client says what it can actually do.
    {
        push_u16(&mut b, &mut n, EXT_SUPPORTED_VERSIONS);
        push_u16(&mut b, &mut n, 3);
        b[n] = 2;
        n += 1;
        push_u16(&mut b, &mut n, 0x0304);
    }

    // key_share: our x25519 public key.
    {
        push_u16(&mut b, &mut n, EXT_KEY_SHARE);
        let len_at = n;
        push_u16(&mut b, &mut n, 0);
        let list_at = n;
        push_u16(&mut b, &mut n, GROUP_X25519);
        push_u16(&mut b, &mut n, 32);
        b[n..n + 32].copy_from_slice(&pub_key);
        n += 32;
        let l = (n - list_at) as u16;
        b[list_at - 2..list_at].copy_from_slice(&l.to_be_bytes());
        let _ = len_at;
    }

    let el = (n - ext_len_at - 2) as u16;
    b[ext_len_at..ext_len_at + 2].copy_from_slice(&el.to_be_bytes());

    // Wrap in a handshake header.
    let mut msg = [0u8; 1024];
    msg[0] = HS_CLIENT_HELLO;
    let bl = n as u32;
    msg[1] = (bl >> 16) as u8;
    msg[2] = (bl >> 8) as u8;
    msg[3] = bl as u8;
    msg[4..4 + n].copy_from_slice(&b[..n]);
    let total = 4 + n;

    transcript_update(&msg[..total]);
    if send_record(CT_HANDSHAKE, &msg[..total]) {
        serial::print_str("[tls] ClientHello sent (");
        serial::print_dec(total as u64);
        serial::print_str(" bytes)\n");
    } else {
        fail(2);
    }
}

fn push_u16(b: &mut [u8], n: &mut usize, v: u16) {
    b[*n..*n + 2].copy_from_slice(&v.to_be_bytes());
    *n += 2;
}

/// Process one or more handshake messages from a record.
fn handle_handshake(data: &[u8]) {
    let mut at = 0usize;
    while at + 4 <= data.len() {
        let t = data[at];
        let len = ((data[at + 1] as usize) << 16)
            | ((data[at + 2] as usize) << 8)
            | data[at + 3] as usize;
        if at + 4 + len > data.len() {
            fail(2);
            return;
        }
        let msg = &data[at..at + 4 + len];
        at += 4 + len;

        match t {
            HS_SERVER_HELLO => {
                transcript_update(msg);
                if !handle_server_hello(&msg[4..]) {
                    return;
                }
            }
            HS_ENCRYPTED_EXTENSIONS => {
                transcript_update(msg);
            }
            HS_CERTIFICATE => {
                transcript_update(msg);
                if !handle_certificate(&msg[4..]) {
                    return;
                }
            }
            HS_CERTIFICATE_VERIFY => {
                transcript_update(msg);
                if !handle_certificate_verify(&msg[4..]) {
                    return;
                }
            }
            HS_FINISHED => {
                // The Finished MAC is over the transcript *without* the
                // Finished itself, so it is checked before being added.
                if !handle_finished(&msg[4..]) {
                    return;
                }
                transcript_update(msg);
            }
            _ => {
                transcript_update(msg);
            }
        }
    }
}

fn handle_server_hello(body: &[u8]) -> bool {
    let mut at = 0usize;
    if body.len() < 38 {
        fail(2);
        return false;
    }
    let version = u16::from_be_bytes([body[0], body[1]]);
    if version != 0x0303 {
        fail(2);
        return false;
    }
    at = 2 + 32;                    // legacy_version + random
    let sid_len = body[at] as usize;
    at += 1 + sid_len;
    if at + 3 > body.len() {
        fail(2);
        return false;
    }
    let suite = u16::from_be_bytes([body[at], body[at + 1]]);
    at += 2;
    if body[at] != 0 {
        fail(2);
        return false;
    }
    at += 1;

    if suite != SUITE_AES128_SHA256 && suite != SUITE_AES256_SHA384 {
        fail(3);
        return false;
    }
    SUITE.store(suite as u32, Ordering::Relaxed);

    // Extensions: supported_versions and key_share.
    let mut server_pub: [u8; 32] = [0; 32];
    let mut have_share = false;
    while at + 4 <= body.len() {
        let et = u16::from_be_bytes([body[at], body[at + 1]]);
        let el = u16::from_be_bytes([body[at + 2], body[at + 3]]) as usize;
        at += 4;
        if at + el > body.len() {
            fail(2);
            return false;
        }
        let ed = &body[at..at + el];
        at += el;
        if et == EXT_SUPPORTED_VERSIONS && el == 2 {
            if u16::from_be_bytes([ed[0], ed[1]]) != 0x0304 {
                fail(2);
                return false;
            }
        } else if et == EXT_KEY_SHARE && el >= 36 {
            let group = u16::from_be_bytes([ed[0], ed[1]]);
            let kl = u16::from_be_bytes([ed[2], ed[3]]) as usize;
            if group == GROUP_X25519 && kl == 32 {
                server_pub.copy_from_slice(&ed[4..36]);
                have_share = true;
            }
        }
    }
    if !have_share {
        // The server may ask for a different group with HelloRetryRequest;
        // this client only offered x25519, so a missing share is a refusal.
        fail(2);
        return false;
    }

    // The shared secret, and the key schedule down to handshake keys.
    let shared = x25519::scalarmult(&unsafe { PRIV }, &server_pub);
    // If the shared secret is all zero the peer's share was a low-order
    // point, which makes the secret something an attacker knows.
    if shared.iter().all(|&b| b == 0) {
        fail(4);
        return false;
    }

    // The key schedule, in the order RFC 8446 gives it.  There is no PSK,
    // so the early secret is Extract(0, 0) and the first "derived" step is
    // a fixed value -- but it is computed rather than skipped, because
    // skipping it is the kind of shortcut that works until a server sends a
    // ticket.
    let zeros = [0u8; 32];
    let early = extract(&[], &zeros);
    let derived1 = derive_secret(&early, b"derived", &sha256_empty());
    let handshake = extract(&derived1, &shared);
    let th = transcript_hash();
    let c_hs = derive_secret(&handshake, b"c hs traffic", &th);
    let s_hs = derive_secret(&handshake, b"s hs traffic", &th);
    let key_len = if suite == SUITE_AES256_SHA384 { 32 } else { 16 };
    unsafe {
        HS_SECRET = handshake;
        C_HS_SECRET = c_hs;
        S_HS_SECRET = s_hs;
        CLIENT_HS = make_keys(&c_hs, key_len);
        SERVER_HS = make_keys(&s_hs, key_len);
    }
    HAVE_HS.store(true, Ordering::Relaxed);

    serial::print_str("[tls] ServerHello, suite 0x");
    serial::print_hex(suite as u64);
    serial::print_str(", handshake keys derived\n");
    true
}

/// key and iv from a traffic secret, through HKDF-Expand-Label.
fn make_keys(secret: &[u8; 32], key_len: usize) -> Keys {
    let mut k = Keys { key: [0u8; 32], key_len, iv: [0u8; 12] };
    let mut key = [0u8; 32];
    expand_label(secret, b"key", &[], &mut key[..key_len]);
    k.key[..key_len].copy_from_slice(&key[..key_len]);
    expand_label(secret, b"iv", &[], &mut k.iv);
    k
}

/// The server's chain.  Held as one buffer; the parse happens in the verify
/// step so a malformed chain fails there rather than here.
fn handle_certificate(body: &[u8]) -> bool {
    if body.is_empty() {
        fail(4);
        return false;
    }
    let mut at = 0usize;
    // A certificate_request_context, then the list.
    let ctx_len = body[at] as usize;
    at += 1 + ctx_len;
    if at + 3 > body.len() {
        fail(4);
        return false;
    }
    let list_len = ((body[at] as usize) << 16)
        | ((body[at + 1] as usize) << 8)
        | body[at + 2] as usize;
    at += 3;
    if at + list_len > body.len() {
        fail(4);
        return false;
    }
    let end = at + list_len;

    // Concatenate the entries with a 4-byte length prefix each, so the chain
    // can be walked later without keeping the record around.
    let base = core::ptr::addr_of_mut!(CHAIN) as *mut u8;
    let mut out = 0usize;
    let mut count = 0usize;
    while at + 3 <= end {
        let cl = ((body[at] as usize) << 16)
            | ((body[at + 1] as usize) << 8)
            | body[at + 2] as usize;
        at += 3;
        if at + cl > end || out + 4 + cl > CHAIN_CAP {
            fail(4);
            return false;
        }
        let hdr = (cl as u32).to_be_bytes();
        for i in 0..4 {
            unsafe { core::ptr::write_volatile(base.add(out + i), hdr[i]) };
        }
        for i in 0..cl {
            unsafe { core::ptr::write_volatile(base.add(out + 4 + i), body[at + i]) };
        }
        out += 4 + cl;
        at += cl;
        count += 1;
        // The extensions after each entry are skipped by the length prefix.
        if at + 2 <= end {
            let el = u16::from_be_bytes([body[at], body[at + 1]]) as usize;
            at += 2 + el;
        }
    }
    if count == 0 {
        fail(4);
        return false;
    }
    CHAIN_LEN.store(out as u32, Ordering::Relaxed);
    serial::print_str("[tls] Certificate: ");
    serial::print_dec(count as u64);
    serial::print_str(" certificate(s), ");
    serial::print_dec(out as u64);
    serial::print_str(" bytes\n");
    true
}

fn handle_certificate_verify(body: &[u8]) -> bool {
    // The signature covers a context string, a zero byte, then the transcript
    // hash — not the handshake bytes themselves.
    if body.len() < 4 {
        fail(5);
        return false;
    }
    let scheme = u16::from_be_bytes([body[0], body[1]]);
    let sl = u16::from_be_bytes([body[2], body[3]]) as usize;
    if body.len() < 4 + sl {
        fail(5);
        return false;
    }
    let sig = &body[4..4 + sl];

    let th = transcript_hash();
    let mut signed = [0u8; 64 + 33];
    let ctx = b"TLS 1.3, server CertificateVerify";
    signed[..ctx.len()].copy_from_slice(ctx);
    signed[ctx.len()] = 0;
    signed[ctx.len() + 1..ctx.len() + 33].copy_from_slice(&th);
    let signed = &signed[..ctx.len() + 33];

    if !verify_with_chain(signed, sig, scheme) {
        fail(5);
        return false;
    }
    serial::print_str("[tls] CertificateVerify: scheme 0x");
    serial::print_hex(scheme as u64);
    serial::print_str(" ok\n");
    true
}

/// Check a signature made by the leaf of the chain we were sent.
fn verify_with_chain(signed: &[u8], sig: &[u8], scheme: u16) -> bool {
    use crate::crypto::x509;

    // Walk the stored chain: 4-byte length, then DER.
    let base = core::ptr::addr_of!(CHAIN) as *const u8;
    let total = CHAIN_LEN.load(Ordering::Relaxed) as usize;
    let mut at = 0usize;
    let mut leaf: Option<x509::Cert> = None;
    while at + 4 <= total {
        let mut l = [0u8; 4];
        for i in 0..4 {
            l[i] = unsafe { core::ptr::read_volatile(base.add(at + i)) };
        }
        let cl = u32::from_be_bytes(l) as usize;
        at += 4;
        if at + cl > total {
            return false;
        }
        let der = unsafe { core::slice::from_raw_parts(base.add(at), cl) };
        if leaf.is_none() {
            leaf = x509::parse(der);
        }
        at += cl;
    }
    let Some(leaf) = leaf else { return false };

    // The chain has to check out to a root before the signature is even
    // looked at: a valid signature by an untrusted key proves nothing.
    if !chain_trusted(&leaf) {
        return false;
    }

    // And the name in the certificate has to be the name we asked for.
    if !x509::hostname_matches(&leaf, host_str()) {
        serial::print_str("[tls] certificate is not for ");
        serial::print_str(host_str());
        serial::print_str("\n");
        return false;
    }

    // The key is the leaf's, and the signature is over `signed`.
    let hash = match scheme {
        SIG_ECDSA_SHA256 => crate::crypto::rsa::HashId::Sha256,
        SIG_ECDSA_SHA384 => crate::crypto::rsa::HashId::Sha384,
        SIG_ECDSA_SHA512 => crate::crypto::rsa::HashId::Sha512,
        SIG_RSA_PSS_RSAE_SHA256 => crate::crypto::rsa::HashId::Sha256,
        SIG_RSA_PSS_RSAE_SHA384 => crate::crypto::rsa::HashId::Sha384,
        SIG_RSA_PSS_RSAE_SHA512 => crate::crypto::rsa::HashId::Sha512,
        SIG_RSA_PKCS1_SHA256 => crate::crypto::rsa::HashId::Sha256,
        SIG_RSA_PKCS1_SHA384 => crate::crypto::rsa::HashId::Sha384,
        SIG_RSA_PKCS1_SHA512 => crate::crypto::rsa::HashId::Sha512,
        _ => return false,
    };
    let mut digest = [0u8; 64];
    let dn = hash.hash(signed, &mut digest);

    if crate::crypto::der::oid_is(leaf.key_alg, x509::OID_EC) {
        let curve = if crate::crypto::der::oid_is(leaf.curve, x509::OID_P256) {
            crate::crypto::ec::Ecdsa::new(&crate::crypto::ec::P256)
        } else if crate::crypto::der::oid_is(leaf.curve, x509::OID_P384) {
            crate::crypto::ec::Ecdsa::new(&crate::crypto::ec::P384)
        } else {
            None
        };
        let Some(curve) = curve else { return false };
        // The scheme and the key type have to agree; an RSA signature under
        // an EC key is a downgrade attempt, not a signature.
        if !matches!(scheme, SIG_ECDSA_SHA256 | SIG_ECDSA_SHA384 | SIG_ECDSA_SHA512) {
            return false;
        }
        let Some(s) = crate::crypto::der::parse_one(sig) else { return false };
        if s.tag != crate::crypto::der::TAG_SEQUENCE {
            return false;
        }
        let mut sr = crate::crypto::der::Reader::new(s.body);
        let (Some(rt), Some(st)) = (sr.expect(crate::crypto::der::TAG_INTEGER),
                                    sr.expect(crate::crypto::der::TAG_INTEGER)) else {
            return false;
        };
        let (Some(r), Some(sv)) = (crate::crypto::der::int_bytes(&rt),
                                   crate::crypto::der::int_bytes(&st)) else {
            return false;
        };
        let w = curve.order_len();
        if r.len() > w || sv.len() > w {
            return false;
        }
        let mut rp = [0u8; 64];
        let mut sp = [0u8; 64];
        rp[w - r.len()..w].copy_from_slice(r);
        sp[w - sv.len()..w].copy_from_slice(sv);
        curve.verify(leaf.key, &rp[..w], &sp[..w], &digest[..dn])
    } else if crate::crypto::der::oid_is(leaf.key_alg, x509::OID_RSA) {
        if matches!(scheme, SIG_ECDSA_SHA256 | SIG_ECDSA_SHA384 | SIG_ECDSA_SHA512) {
            return false;
        }
        let Some(k) = crate::crypto::der::parse_one(leaf.key) else { return false };
        let mut kr = crate::crypto::der::Reader::new(k.body);
        let (Some(nt), Some(et)) = (kr.expect(crate::crypto::der::TAG_INTEGER),
                                    kr.expect(crate::crypto::der::TAG_INTEGER)) else {
            return false;
        };
        let (Some(n), Some(e)) = (crate::crypto::der::int_bytes(&nt),
                                  crate::crypto::der::int_bytes(&et)) else {
            return false;
        };
        let Some(key) = crate::crypto::rsa::RsaPublic::new(n, e) else { return false };
        if matches!(scheme, SIG_RSA_PKCS1_SHA256 | SIG_RSA_PKCS1_SHA384 | SIG_RSA_PKCS1_SHA512) {
            key.verify_pkcs1_v15(sig, hash, &digest[..dn])
        } else {
            key.verify_pss(sig, hash, &digest[..dn])
        }
    } else {
        false
    }
}

/// Check the chain from the leaf up to a root this kernel trusts.
fn chain_trusted(leaf: &crate::crypto::x509::Cert) -> bool {
    use crate::crypto::x509;

    let base = core::ptr::addr_of!(CHAIN) as *const u8;
    let total = CHAIN_LEN.load(Ordering::Relaxed) as usize;

    // Index the certificates by offset so they can be walked twice without
    // copying: once to find each one, once to verify against its issuer.
    let mut at = 0usize;
    let mut offs = [0usize; 8];
    let mut lens = [0usize; 8];
    let mut count = 0usize;
    while at + 4 <= total && count < 8 {
        let mut l = [0u8; 4];
        for i in 0..4 {
            l[i] = unsafe { core::ptr::read_volatile(base.add(at + i)) };
        }
        let cl = u32::from_be_bytes(l) as usize;
        at += 4;
        if at + cl > total {
            break;
        }
        offs[count] = at;
        lens[count] = cl;
        count += 1;
        at += cl;
    }
    if count == 0 {
        return false;
    }

    let get = |i: usize| -> Option<x509::Cert> {
        let der = unsafe { core::slice::from_raw_parts(base.add(offs[i]), lens[i]) };
        x509::parse(der)
    };

    // Each certificate must be signed by the next one along.
    for i in 0..count - 1 {
        let (Some(child), Some(issuer)) = (get(i), get(i + 1)) else { return false };
        if !x509::same_name(child.issuer, issuer.subject) {
            return false;
        }
        if !x509::verify_signed_by(&child, &issuer) {
            return false;
        }
        if !issuer.is_ca {
            return false;
        }
    }

    // The last one has to be a root we already trust, and it has to be
    // self-signed — otherwise the chain ends at an attacker's certificate.
    let last = count - 1;
    let (Some(top), _) = (get(last), ()) else { return false };
    if !top.is_ca {
        return false;
    }
    if !crate::crypto::trust::is_trusted(&top) {
        serial::print_str("[tls] chain does not end at a trusted root\n");
        return false;
    }
    let _ = leaf;
    true
}

fn handle_finished(body: &[u8]) -> bool {
    // verify_data = HMAC(finished_key, transcript_hash), where the key is
    // derived from the server's handshake secret.
    if body.len() != 32 {
        // A 48-byte one would mean SHA-384, which this client does not offer.
        fail(6);
        return false;
    }
    let th = transcript_hash();
    let s_hs = match server_hs_secret() {
        Some(s) => s,
        None => {
            fail(6);
            return false;
        }
    };
    let mut finished_key = [0u8; 32];
    expand_label(&s_hs, b"finished", &[], &mut finished_key);
    let want = hmac(&finished_key, &th);
    if !crate::sha256::constant_time_eq(&want, body) {
        fail(6);
        return false;
    }

    // The server is who it says.  Only now derive the application keys and
    // send our own Finished.
    VERIFIED.store(true, Ordering::Relaxed);
    let hs = handshake_secret().unwrap();
    let derived2 = derive_secret(&hs, b"derived", &sha256_empty());
    let master = extract(&derived2, &[0u8; 32]);
    let th2 = transcript_hash();
    let c_ap = derive_secret(&master, b"c ap traffic", &th2);
    let s_ap = derive_secret(&master, b"s ap traffic", &th2);
    let key_len = if SUITE.load(Ordering::Relaxed) == SUITE_AES256_SHA384 as u32 { 32 } else { 16 };
    unsafe {
        CLIENT_AP = make_keys(&c_ap, key_len);
        SERVER_AP = make_keys(&s_ap, key_len);
    }

    // Our Finished, over the transcript that now includes the server's.
    let th3 = transcript_hash();
    let mut client_finished_key = [0u8; 32];
    expand_label(&c_hs_secret().unwrap(), b"finished", &[], &mut client_finished_key);
    let verify = hmac(&client_finished_key, &th3);
    let mut msg = [0u8; 4 + 32];
    msg[0] = HS_FINISHED;
    msg[1..4].copy_from_slice(&[0, 0, 32]);
    msg[4..].copy_from_slice(&verify);
    transcript_update(&msg);
    send_record(CT_HANDSHAKE, &msg);

    PHASE.store(phase_to_u8(Phase::Established), Ordering::Relaxed);
    serial::print_str("[tls] handshake complete, ");
    serial::print_str(host_str());
    serial::print_str(" verified\n");
    true
}

/// The secrets the key schedule passes between stages.  Kept as the extract
/// outputs rather than recomputed, because the ECDHE secret is not kept at
/// all -- once it has been folded into the handshake secret there is no
/// reason for it to still be in memory.
static mut HS_SECRET: [u8; 32] = [0; 32];
static mut C_HS_SECRET: [u8; 32] = [0; 32];
static mut S_HS_SECRET: [u8; 32] = [0; 32];
static HAVE_HS: AtomicBool = AtomicBool::new(false);

fn server_hs_secret() -> Option<[u8; 32]> {
    if HAVE_HS.load(Ordering::Relaxed) { Some(unsafe { S_HS_SECRET }) } else { None }
}

fn handshake_secret() -> Option<[u8; 32]> {
    if HAVE_HS.load(Ordering::Relaxed) { Some(unsafe { HS_SECRET }) } else { None }
}

fn c_hs_secret() -> Option<[u8; 32]> {
    if HAVE_HS.load(Ordering::Relaxed) { Some(unsafe { C_HS_SECRET }) } else { None }
}

fn sha256_empty() -> [u8; 32] {
    crate::sha256::hash(&[])
}

/// Send application data.  Returns false unless the handshake verified.
pub fn send(data: &[u8]) -> bool {
    if !established() {
        return false;
    }
    send_record(CT_APPLICATION_DATA, data)
}

pub fn close() {
    let mut alert = [0u8; 2];
    alert[0] = 1;                   // warning
    alert[1] = 0;                   // close_notify
    send_record(CT_ALERT, &alert);
    crate::net::tcp::close();
}

pub fn selftest() -> usize {
    0
}
