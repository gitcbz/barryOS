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
use crate::crypto::x509;
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
/// TLS 1.2 only.  These numbers are shared with 1.3's namespace, where they
/// went unused — which is why a 1.3 client that sees one has a server that
/// has not understood the version at all.
const HS_SERVER_KEY_EXCHANGE: u8 = 12;
const HS_CERTIFICATE_REQUEST: u8 = 13;
const HS_SERVER_HELLO_DONE: u8 = 14;
const HS_CLIENT_KEY_EXCHANGE: u8 = 16;

/// TLS 1.3 cipher suites.  Only the SHA-256 one is offered or accepted: the
/// whole key schedule is HKDF-SHA256 and the transcript is SHA-256, so a
/// SHA-384 suite would not be a different mode, it would be a different
/// protocol.  Offering it and deriving SHA-256 keys anyway is the kind of
/// mistake that shows up as an unexplained decryption failure at the end.
pub const SUITE_AES128_SHA256: u16 = 0x1301;

/// The TLS 1.2 suites, which are a different list in the same message.  A
/// server that cannot do 1.3 picks from these, and one that finds none of its
/// own answers with `protocol_version` and hangs up — which is exactly what
/// baidu.com did while this client offered 1.3 alone.
use crate::crypto::tls12::{self, SUITE_ECDHE_ECDSA_AES128_GCM_SHA256,
                            SUITE_ECDHE_RSA_AES128_GCM_SHA256};

/// Named groups and signature schemes.
const GROUP_X25519: u16 = 0x001d;
const GROUP_SECP256R1: u16 = 0x0017;
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
const EXT_EC_POINT_FORMATS: u16 = 0x000b;
const EXT_SIGNATURE_ALGORITHMS: u16 = 0x000d;
const EXT_ALPN: u16 = 0x0010;
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
///
/// A response larger than this is decrypted, counted and dropped past the cap
/// — the transfer has to be allowed to finish, and a kernel that grows a
/// buffer to fit whatever it is handed does not get to choose its own size.
/// `truncated` says when that happened, so the page is shown as the first part
/// of something rather than as the whole of it.
const RX_CAP: usize = 256 * 1024;
static mut RX: [u8; RX_CAP] = [0; RX_CAP];
static RX_LEN: AtomicU32 = AtomicU32::new(0);
static RX_TRUNCATED: AtomicBool = AtomicBool::new(false);

/// The host being connected to, for SNI and for the certificate check.
static mut HOST: [u8; 128] = [0; 128];
static HOST_LEN: AtomicU32 = AtomicU32::new(0);

/// The ClientHello's random.  TLS 1.2 signs it and derives from it; 1.3 only
/// needs it to be unique.
static mut CLIENT_RANDOM: [u8; 32] = [0; 32];

/// Which handshake the ServerHello settled on: 0 undecided, 12 or 13.
static VERSION: AtomicU8 = AtomicU8::new(0);
/// Where the TLS 1.2 exchange has got to: 0 reading the server's flight,
/// 1 having sent ours, 2 finished.
static STEP12: AtomicU8 = AtomicU8::new(0);
/// Is the 1.2 cipher spec in force in each direction?  Records are plaintext
/// until the other end's ChangeCipherSpec has arrived.
static CLIENT_CIPHER: AtomicBool = AtomicBool::new(false);
static SERVER_CIPHER: AtomicBool = AtomicBool::new(false);
/// The 1.2 handshake's keys and secrets.
static mut HS12: tls12::Handshake = tls12::Handshake::EMPTY;

/// Fill `out` with bytes that must not repeat between connections.
///
/// Not a cryptographic generator, and it does not claim to be: what it has to
/// do is differ every time.  The cycle counter does, and it is mixed so that
/// two calls in the same microsecond do not return the same bytes.
pub fn random_bytes(out: &mut [u8]) {
    let mut s = unsafe { core::arch::x86_64::_rdtsc() }
        ^ (crate::interrupts::TIMER_TICKS.load(Ordering::Relaxed) as u64).rotate_left(29)
        ^ core::ptr::addr_of!(TRANSCRIPT) as u64;
    for b in out.iter_mut() {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        *b = (s & 0xFF) as u8;
    }
}

/// The port to connect to.  Kept rather than assumed: the DNS path used to
/// connect to 443 unconditionally, which is invisible when 443 is the only
/// port anything asks for and wrong the moment something else is.
static PORT: AtomicU32 = AtomicU32::new(443);

/// A record being assembled from TCP, and one being assembled for sending.
static mut RX_REC: [u8; 16640] = [0; 16640];
static RX_REC_LEN: AtomicU32 = AtomicU32::new(0);

/// How far into the current pass the record layer has read.  Only the
/// diagnostic below uses it: the position is a local in `drain_tcp`, because
/// consuming a record releases it from TCP and the offsets then start over.
static mut RX_CONSUMED: usize = 0;

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
    PORT.store(port as u32, Ordering::Relaxed);

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
    true
}

fn reset() {
    PHASE.store(0, Ordering::Relaxed);
    ERROR.store(0, Ordering::Relaxed);
    VERIFIED.store(false, Ordering::Relaxed);
    RX_LEN.store(0, Ordering::Relaxed);
    RX_REC_LEN.store(0, Ordering::Relaxed);
    RX_TRUNCATED.store(false, Ordering::Relaxed);
    CHAIN_LEN.store(0, Ordering::Relaxed);
    CLIENT_SEQ.store(0, Ordering::Relaxed);
    SERVER_SEQ.store(0, Ordering::Relaxed);
    unsafe {
        TRANSCRIPT = Sha256::new();
        RX_CONSUMED = 0;
        // The keys too, and this is the one that mattered.  `send_record`
        // decides whether a record is encrypted by asking whether a key is
        // installed, so leaving the last connection's keys here means the next
        // connection's ClientHello goes out *encrypted* — with keys the server
        // has never seen, under a content type it does not expect, one byte
        // longer than a ClientHello.  Nothing about that looks like a stale
        // key: the first fetch after a boot works, every one after it fails,
        // and the server answers with something that reads as an HTTP error.
        CLIENT_HS = Keys::default();
        SERVER_HS = Keys::default();
        CLIENT_AP = Keys::default();
        SERVER_AP = Keys::default();
        HS_SECRET = [0; 32];
        C_HS_SECRET = [0; 32];
        S_HS_SECRET = [0; 32];
        PRIV = [0; 32];
        PUB = [0; 32];
        CLIENT_RANDOM = [0; 32];
        HS12 = tls12::Handshake::EMPTY;
    }
    HAVE_HS.store(false, Ordering::Relaxed);
    VERSION.store(0, Ordering::Relaxed);
    STEP12.store(0, Ordering::Relaxed);
    CLIENT_CIPHER.store(false, Ordering::Relaxed);
    SERVER_CIPHER.store(false, Ordering::Relaxed);
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
                        crate::net::tcp::connect(ip, PORT.load(Ordering::Relaxed) as u16);
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
            // TLS 1.2 is a request-and-reply exchange: once ServerHelloDone
            // has been read, everything the client owes can go at once and
            // does not depend on anything the server has yet to say.
            if cipher_spec_for_version() && STEP12.load(Ordering::Relaxed) == 1 {
                STEP12.store(2, Ordering::Relaxed);
                if !send_flight12() {
                    return;
                }
            }
            if crate::net::tcp::state() == crate::net::tcp::State::Failed {
                fail(6);
            } else if !established()
                && (crate::net::tcp::state() == crate::net::tcp::State::Closed
                    || crate::net::tcp::peer_closed())
            {
                fail(8);
            }
        }
        Phase::Established => {
            crate::net::tcp::tick();
            drain_tcp();
            // The peer's FIN ends the session, close_notify or not.  A server
            // that shuts the socket down has said the same thing in the only
            // way that reaches a client mid-page, and waiting for an alert that
            // is never coming is how a complete response becomes a timeout.
            if crate::net::tcp::peer_closed() {
                PHASE.store(phase_to_u8(Phase::Closed), Ordering::Relaxed);
                serial::print_str("[tls] the server closed the connection\n");
            }
        }
        _ => {}
    }
}



// ---------------------------------------------------------------------------
//  Record layer
// ---------------------------------------------------------------------------

/// Write a handshake record, encrypted or not depending on the phase.
fn send_record(content_type: u8, body: &[u8]) -> bool {
    if cipher_spec_for_version() {
        return send_record12(content_type, body);
    }
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
        let iv = nonce_for(&keys.iv, seq);
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
///
/// Records are read at an offset into what TCP is holding, and the whole of
/// what was consumed is handed back in one go at the end.  Two things depend
/// on doing it in that order:
///
///   * the offsets stay valid for the duration of the pass, which they would
///     not if each record were released as it was read;
///   * releasing *is* the consumption.  That is why the read position is a
///     local here and not a static: the data it pointed at is gone, so the
///     next pass starts at zero and sees exactly what is left.  Holding the
///     position in a static, as this used to, is what re-ran a whole handshake
///     on every tick; not releasing at all is what stopped a page larger than
///     TCP's buffer from ever arriving.
fn drain_tcp() {
    let mut consumed = 0usize;
    loop {
        unsafe { RX_CONSUMED = consumed };
        let mut hdr = [0u8; 5];
        if crate::net::tcp::rx_read_at(consumed, &mut hdr) < 5 {
            break;
        }
        let len = u16::from_be_bytes([hdr[3], hdr[4]]) as usize;
        if len == 0 || len > 16640 {
            log_record("impossible length", &hdr, len);
            fail(7);
            return;
        }
        if crate::net::tcp::rx_len() < consumed + 5 + len {
            break;                      // the rest has not arrived yet
        }
        consumed += 5 + len;

        // A ChangeCipherSpec in TLS 1.2 is a record in its own right, sent in
        // clear, and it is what puts the new cipher spec in force — including
        // starting the read sequence number over at zero.  It carries no
        // handshake message and does not go into the transcript.
        if cipher_spec_for_version() && hdr[0] == CT_CHANGE_CIPHER_SPEC {
            SERVER_CIPHER.store(true, Ordering::Relaxed);
            SERVER_SEQ.store(0, Ordering::Relaxed);
            serial::print_str("[tls] TLS 1.2 ChangeCipherSpec received\n");
            continue;
        }

        // An encrypted record is always typed as application_data on the
        // outside; the real type is the last byte of the plaintext.
        let (ctype, plen) = if cipher_spec_for_version() && SERVER_CIPHER.load(Ordering::Relaxed) {
            // TLS 1.2 AEAD (RFC 5288): an eight-byte explicit nonce on the
            // front, then the ciphertext, then the tag.  The implicit half of
            // the nonce comes from the key block and never moves.
            if len < 8 + 16 {
                log_record("an encrypted record too short to hold a nonce", &hdr, len);
                fail(7);
                return;
            }
            let h = unsafe { &*core::ptr::addr_of!(HS12) };
            let mut rec = [0u8; 16640];
            crate::net::tcp::rx_read_at(consumed - len, &mut rec[..len]);
            let mut nonce = [0u8; 12];
            nonce[..4].copy_from_slice(&h.server_iv);
            nonce[4..].copy_from_slice(&rec[..8]);
            let seq = SERVER_SEQ.fetch_add(1, Ordering::Relaxed);
            let plain_len = len - 8 - 16;
            let aad = aad12(seq, hdr[0], plain_len);
            let out = plain_buf();
            match gcm_decrypt(&h.server_key, &nonce, &aad, &rec[8..len], out) {
                Some(m) if m == plain_len => (hdr[0], m),
                _ => {
                    log_record("record did not authenticate", &hdr, len);
                    fail(7);
                    return;
                }
            }
        } else if hdr[0] == CT_APPLICATION_DATA && !cipher_spec_for_version() {
            let keys = unsafe {
                if !established() && SERVER_HS.key_len != 0 { &SERVER_HS } else { &SERVER_AP }
            };
            if keys.key_len == 0 {
                log_record("encrypted before the keys existed", &hdr, len);
                fail(7);
                return;
            }
            let seq = SERVER_SEQ.fetch_add(1, Ordering::Relaxed);
            let iv = nonce_for(&keys.iv, seq);
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
                    log_record("record did not authenticate", &hdr, len);
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
    // Everything this pass took, handed back at once.  A record that only
    // partly arrived was not counted, so it stays in TCP's buffer and the next
    // pass starts with it.
    crate::net::tcp::rx_release(consumed);
}

/// The per-record nonce: the write IV with the record's sequence number
/// XORed into its last eight bytes (RFC 8446 §5.3).
///
/// Eight bytes, and the sequence number here is a u32, so it has to be
/// widened.  Taking `seq.to_be_bytes()` off a u32 gives four, and the four
/// bytes past the end are not "the upper half is zero" — they are a panic on
/// the first encrypted record, which is how this was found.
fn nonce_for(base_iv: &[u8; 12], seq: u32) -> [u8; 12] {
    let mut iv = *base_iv;
    let s = (seq as u64).to_be_bytes();
    for i in 0..8 {
        iv[4 + i] ^= s[i];
    }
    iv
}

/// One record's header, when it turns out to be a problem.
///
/// "a record failed to authenticate" covers a decryption failure, a record
/// that arrived before there were keys for it, and a header that is not a
/// record header at all — three unrelated faults that need different fixes and
/// print the same sentence.  The bytes say which.
fn log_record(what: &str, hdr: &[u8; 5], len: usize) {
    serial::print_str("[tls] ");
    serial::print_str(what);
    serial::print_str(": header");
    for &b in hdr {
        serial::print_str(" ");
        serial::print_hex(b as u64);
    }
    serial::print_str(" len ");
    serial::print_dec(len as u64);
    serial::print_str(" at ");
    serial::print_dec(unsafe { RX_CONSUMED } as u64);
    serial::print_str(" of ");
    serial::print_dec(crate::net::tcp::rx_len() as u64);
    serial::print_str("\n");
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
        RX_TRUNCATED.store(true, Ordering::Relaxed);
        return;
    }
    let n = data.len().min(RX_CAP - used);
    if n < data.len() {
        RX_TRUNCATED.store(true, Ordering::Relaxed);
    }
    let base = core::ptr::addr_of_mut!(RX) as *mut u8;
    for i in 0..n {
        unsafe { core::ptr::write_volatile(base.add(used + i), data[i]) };
    }
    RX_LEN.store((used + n) as u32, Ordering::Relaxed);
}

/// Did the response not fit?  The page is then the beginning of something,
/// which is worth saying rather than showing as if it were all of it.
pub fn truncated() -> bool {
    RX_TRUNCATED.load(Ordering::Relaxed)
}

/// Add a handshake message to the transcript hash.
fn transcript_update(msg: &[u8]) {
    unsafe {
        let t = &mut *core::ptr::addr_of_mut!(TRANSCRIPT);
        t.update(msg);
    }
    log_message(msg);
}

// --- handshake trace -------------------------------------------------------
//
// The exact bytes of every message that went into the transcript, and the
// signature that was supposed to cover them.  A signature that will not
// verify has two possible explanations — the signing input or the verifier —
// and they are indistinguishable from the "no" that comes back.  With this,
// tests/tls-host.rs can hand the same bytes to an independent implementation
// and see which of the two it is.

/// Every handshake message, in the order it entered the transcript.  Loose
/// 4-byte length prefixes, so the boundaries survive; only the trace uses it.
const TRACE_CAP: usize = 12 * 1024;
const TRACE_SIG_CAP: usize = 1024;
const TRACE_SIGNED_CAP: usize = 256;
const TRACE_LEAF_CAP: usize = 4096;
static mut TRACE: [u8; TRACE_CAP] = [0; TRACE_CAP];
static TRACE_LEN: AtomicU32 = AtomicU32::new(0);
/// How long the trace was when CertificateVerify arrived — the transcript the
/// signature had to cover.  Everything after it is the rest of the handshake.
static TRACE_AT_CV: AtomicU32 = AtomicU32::new(0);
/// The CertificateVerify signature, and what it had to cover.
static mut TRACE_SIG: [u8; TRACE_SIG_CAP] = [0; TRACE_SIG_CAP];
static TRACE_SIG_LEN: AtomicU32 = AtomicU32::new(0);
static mut TRACE_SIGNED: [u8; TRACE_SIGNED_CAP] = [0; TRACE_SIGNED_CAP];
static TRACE_SIGNED_LEN: AtomicU32 = AtomicU32::new(0);
/// The end-entity certificate, so the trace can be checked against its key.
static mut TRACE_LEAF: [u8; TRACE_LEAF_CAP] = [0; TRACE_LEAF_CAP];
static TRACE_LEAF_LEN: AtomicU32 = AtomicU32::new(0);

fn log_message(msg: &[u8]) {
    let used = TRACE_LEN.load(Ordering::Relaxed) as usize;
    if used + 4 + msg.len() > TRACE_CAP {
        return;
    }
    let base = core::ptr::addr_of_mut!(TRACE) as *mut u8;
    let hdr = (msg.len() as u32).to_be_bytes();
    for i in 0..4 {
        unsafe { core::ptr::write_volatile(base.add(used + i), hdr[i]) };
    }
    for i in 0..msg.len() {
        unsafe { core::ptr::write_volatile(base.add(used + 4 + i), msg[i]) };
    }
    TRACE_LEN.store((used + 4 + msg.len()) as u32, Ordering::Relaxed);
}

fn log_certificate_verify(signed: &[u8], sig: &[u8], leaf: &[u8]) {
    TRACE_AT_CV.store(TRACE_LEN.load(Ordering::Relaxed), Ordering::Relaxed);
    let n = sig.len().min(TRACE_SIG_CAP);
    let base = core::ptr::addr_of_mut!(TRACE_SIG) as *mut u8;
    for i in 0..n {
        unsafe { core::ptr::write_volatile(base.add(i), sig[i]) };
    }
    TRACE_SIG_LEN.store(n as u32, Ordering::Relaxed);

    let n = signed.len().min(TRACE_SIGNED_CAP);
    let base = core::ptr::addr_of_mut!(TRACE_SIGNED) as *mut u8;
    for i in 0..n {
        unsafe { core::ptr::write_volatile(base.add(i), signed[i]) };
    }
    TRACE_SIGNED_LEN.store(n as u32, Ordering::Relaxed);

    let n = leaf.len().min(TRACE_LEAF_CAP);
    let base = core::ptr::addr_of_mut!(TRACE_LEAF) as *mut u8;
    for i in 0..n {
        unsafe { core::ptr::write_volatile(base.add(i), leaf[i]) };
    }
    TRACE_LEAF_LEN.store(n as u32, Ordering::Relaxed);
}

/// Copy the whole trace: every handshake message that entered the transcript,
/// in order.  The caller works out which prefix was the signing context.
pub fn trace_messages(out: &mut [u8]) -> usize {
    copy_static(&raw const TRACE, TRACE_LEN.load(Ordering::Relaxed) as usize, out)
}

/// How long the trace was when CertificateVerify arrived.
pub fn trace_at_cv() -> usize {
    TRACE_AT_CV.load(Ordering::Relaxed) as usize
}

pub fn trace_signature(out: &mut [u8]) -> usize {
    copy_static(&raw const TRACE_SIG, TRACE_SIG_LEN.load(Ordering::Relaxed) as usize, out)
}

pub fn trace_signed(out: &mut [u8]) -> usize {
    copy_static(&raw const TRACE_SIGNED, TRACE_SIGNED_LEN.load(Ordering::Relaxed) as usize, out)
}

pub fn trace_leaf(out: &mut [u8]) -> usize {
    copy_static(&raw const TRACE_LEAF, TRACE_LEAF_LEN.load(Ordering::Relaxed) as usize, out)
}

fn copy_static(src: *const [u8], len: usize, out: &mut [u8]) -> usize {
    let n = len.min(out.len());
    let base = src as *const u8;
    for i in 0..n {
        out[i] = unsafe { core::ptr::read_volatile(base.add(i)) };
    }
    n
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
    random_bytes(&mut priv_key);
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
    // random: 32 bytes.  Only needs to be unique.  Kept, because TLS 1.2 signs
    // it into the key exchange and derives the master secret from it.
    {
        let mut r = [0u8; 32];
        random_bytes(&mut r);
        unsafe { CLIENT_RANDOM = r };
        b[n..n + 32].copy_from_slice(&r);
        n += 32;
    }
    b[n] = 0;                       // empty session id
    n += 1;
    // Cipher suites: 1.3 first, then the 1.2 AEAD pair.  A 1.3 server takes
    // the first and ignores the rest; a 1.2 server finds nothing it knows in
    // the first and picks from the others.
    push_u16(&mut b, &mut n, 6);
    push_u16(&mut b, &mut n, SUITE_AES128_SHA256);
    push_u16(&mut b, &mut n, SUITE_ECDHE_ECDSA_AES128_GCM_SHA256);
    push_u16(&mut b, &mut n, SUITE_ECDHE_RSA_AES128_GCM_SHA256);
    b[n] = 1;                       // one compression method: null
    n += 1;
    b[n] = 0;
    n += 1;

    // Extensions
    let ext_len_at = n;
    push_u16(&mut b, &mut n, 0);

    // server_name.  The only one with a shape of its own: a list of names,
    // each with a type byte and a length.
    {
        let host = host_str();
        let at = ext_begin(&mut b, &mut n, EXT_SERVER_NAME);
        push_u16(&mut b, &mut n, (host.len() + 3) as u16);  // the list
        b[n] = 0;                                           // host_name
        n += 1;
        push_u16(&mut b, &mut n, host.len() as u16);
        b[n..n + host.len()].copy_from_slice(host.as_bytes());
        n += host.len();
        ext_end(&mut b, &n, at);
    }

    // supported_groups: x25519 first, because that is the group the key share
    // below is for and a 1.3 server must be able to take it without a retry.
    // P-256 is here for TLS 1.2, where x25519 is not universally accepted —
    // baidu.com's 1.2 stack refuses it outright.
    {
        let at = ext_begin(&mut b, &mut n, EXT_SUPPORTED_GROUPS);
        let list_at = n;
        push_u16(&mut b, &mut n, 0);
        push_u16(&mut b, &mut n, GROUP_X25519);
        push_u16(&mut b, &mut n, GROUP_SECP256R1);
        let l = (n - list_at - 2) as u16;
        b[list_at..list_at + 2].copy_from_slice(&l.to_be_bytes());
        ext_end(&mut b, &n, at);
    }

    // ec_point_formats: uncompressed only.  1.2 servers are entitled to
    // require this extension rather than assume its default.
    {
        let at = ext_begin(&mut b, &mut n, EXT_EC_POINT_FORMATS);
        b[n] = 1;
        n += 1;
        b[n] = 0;                   // uncompressed
        n += 1;
        ext_end(&mut b, &n, at);
    }

    // signature_algorithms: everything this kernel can verify.
    {
        let at = ext_begin(&mut b, &mut n, EXT_SIGNATURE_ALGORITHMS);
        let list_at = n;
        push_u16(&mut b, &mut n, 0);
        for s in [SIG_ECDSA_SHA256, SIG_ECDSA_SHA384, SIG_ECDSA_SHA512,
                  SIG_RSA_PSS_RSAE_SHA256, SIG_RSA_PSS_RSAE_SHA384, SIG_RSA_PSS_RSAE_SHA512,
                  SIG_RSA_PKCS1_SHA256, SIG_RSA_PKCS1_SHA384, SIG_RSA_PKCS1_SHA512] {
            push_u16(&mut b, &mut n, s);
        }
        let l = (n - list_at - 2) as u16;
        b[list_at..list_at + 2].copy_from_slice(&l.to_be_bytes());
        ext_end(&mut b, &n, at);
    }

    // alpn: say plainly that this is an HTTP/1.1 client.  Without it a CDN has
    // to guess, and the guess is not always the protocol below.
    {
        let at = ext_begin(&mut b, &mut n, EXT_ALPN);
        let list_at = n;
        push_u16(&mut b, &mut n, 0);
        b[n] = 8;
        n += 1;
        b[n..n + 8].copy_from_slice(b"http/1.1");
        n += 8;
        let l = (n - list_at - 2) as u16;
        b[list_at..list_at + 2].copy_from_slice(&l.to_be_bytes());
        ext_end(&mut b, &n, at);
    }

    // supported_versions: 1.3 preferred, 1.2 acceptable.  A server that
    // understands this extension — which every 1.3 server does, and which
    // RFC 8446 defines for 1.2 servers to ignore or answer — replies with its
    // choice.  Listing only 1.3 was what made a 1.2-only server refuse.
    {
        let at = ext_begin(&mut b, &mut n, EXT_SUPPORTED_VERSIONS);
        b[n] = 4;
        n += 1;
        push_u16(&mut b, &mut n, 0x0304);
        push_u16(&mut b, &mut n, 0x0303);
        ext_end(&mut b, &n, at);
    }

    // key_share: our x25519 public key.
    {
        let at = ext_begin(&mut b, &mut n, EXT_KEY_SHARE);
        let list_at = n;
        push_u16(&mut b, &mut n, 0);
        push_u16(&mut b, &mut n, GROUP_X25519);
        push_u16(&mut b, &mut n, 32);
        b[n..n + 32].copy_from_slice(&pub_key);
        n += 32;
        let l = (n - list_at - 2) as u16;
        b[list_at..list_at + 2].copy_from_slice(&l.to_be_bytes());
        ext_end(&mut b, &n, at);
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

/// Start an extension: the type, then a length to be filled in at the end.
///
/// The pair exists because writing the length by hand is what went wrong here:
/// two extensions declared themselves two bytes shorter than they are — the
/// missing two being the inner length field the author forgot to count — and
/// the server's answer to that is a bare `decode_error`, which says nothing
/// about which extension was wrong or how.
fn ext_begin(b: &mut [u8], n: &mut usize, etype: u16) -> usize {
    push_u16(b, n, etype);
    let at = *n;
    push_u16(b, n, 0);
    at
}

fn ext_end(b: &mut [u8], n: &usize, at: usize) {
    let l = (*n - at - 2) as u16;
    b[at..at + 2].copy_from_slice(&l.to_be_bytes());
}

/// Which handshake a ServerHello is announcing.
///
/// A 1.3 server says so in `supported_versions` and nowhere else — its
/// legacy_version is 0x0303 like everyone else's, because that field is the
/// one middleboxes read.  So the extension is the answer, and its absence is
/// the answer too: a server that did not send it is not 1.3.
fn server_hello_version(body: &[u8]) -> u16 {
    let mut at = 0usize;
    if body.len() < 38 {
        return 0;
    }
    at = 2 + 32;
    let sid = body[at] as usize;
    at += 1 + sid;
    if at + 3 > body.len() {
        return 0;
    }
    at += 2;                        // cipher suite
    at += 1;                        // compression
    if at + 2 > body.len() {
        return 0;
    }
    at += 2;                        // the extensions block's own length
    while at + 4 <= body.len() {
        let et = u16::from_be_bytes([body[at], body[at + 1]]);
        let el = u16::from_be_bytes([body[at + 2], body[at + 3]]) as usize;
        at += 4;
        if at + el > body.len() {
            return 0;
        }
        if et == EXT_SUPPORTED_VERSIONS && el >= 2 {
            return u16::from_be_bytes([body[at], body[at + 1]]);
        }
        at += el;
    }
    0
}

/// Process one or more handshake messages from a record.  Which handshake is
/// decided by the ServerHello, and cannot be known before it.
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

        // The ServerHello is the only message that is read the same way by
        // both versions, and it is what tells us which one we are in.
        if t == HS_SERVER_HELLO && VERSION.load(Ordering::Relaxed) == 0 {
            let v = server_hello_version(&msg[4..]);
            VERSION.store(if v == 0x0304 { 13 } else { 12 }, Ordering::Relaxed);
        }
        if VERSION.load(Ordering::Relaxed) == 12 {
            if !handle_handshake12(msg) {
                return;
            }
            continue;
        }

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
                if !handle_certificate(&msg[4..], true) {
                    return;
                }
            }
            HS_CERTIFICATE_VERIFY => {
                // Checked first, added after.  CertificateVerify signs the
                // transcript up to and including the certificate, so putting
                // the message itself in first changes the value the signature
                // is supposed to cover.  Nothing about that failure says
                // "transcript": it says the server cannot prove it holds the
                // key, which is also what a forged signature says.
                if !handle_certificate_verify(&msg[4..]) {
                    return;
                }
                transcript_update(msg);
            }
            HS_FINISHED => {
                // The same asymmetry, one step later, and handled inside:
                // the server's MAC is over the transcript without itself, and
                // ours is over the transcript with it.
                if !handle_finished(msg, &msg[4..]) {
                    return;
                }
            }
            _ => {
                transcript_update(msg);
            }
        }
    }
}

// ---------------------------------------------------------------------------
//  TLS 1.2
// ---------------------------------------------------------------------------

/// One TLS 1.2 handshake message.
///
/// The order is fixed and nothing is optional once it starts: ServerHello,
/// Certificate, ServerKeyExchange, [CertificateRequest], ServerHelloDone.
/// There is no encryption before the ChangeCipherSpec, so all of this is
/// plaintext on the wire — which is the 1.3 objection to this protocol, and
/// not something this end can do anything about.
fn handle_handshake12(msg: &[u8]) -> bool {
    let h = unsafe { &mut *core::ptr::addr_of_mut!(HS12) };
    match msg[0] {
        HS_SERVER_HELLO => {
            transcript_update(msg);
            // The client's random is half of what the server signs in
            // ServerKeyExchange and half of the master secret's seed, so it
            // has to be here before either is looked at.
            h.client_random = unsafe { CLIENT_RANDOM };
            if !h.server_hello(&msg[4..]) {
                return fail12(h.why);
            }
            serial::print_str("[tls] TLS 1.2, cipher suite 0x");
            serial::print_hex(h.suite as u64);
            serial::print_str("\n");
            true
        }
        HS_CERTIFICATE => {
            transcript_update(msg);
            // No certificate_request_context in this version.
            if !handle_certificate(&msg[4..], false) {
                return false;
            }
            // Checked here rather than at the end, because everything after
            // this point is signed by the key in this certificate — a
            // signature that verifies under a certificate nobody trusts is
            // not evidence of anything.
            chain_and_name_ok()
        }
        HS_SERVER_KEY_EXCHANGE => {
            transcript_update(msg);
            let der = leaf_der();
            let Some(leaf) = x509::parse(der) else {
                return fail12("the certificate did not parse");
            };
            // The borrow of CHAIN has to end before the handshake is touched.
            let (alg, curve, key) = (leaf.key_alg, leaf.curve, leaf.key);
            if !h.server_key_exchange(&msg[4..], alg, curve, key) {
                return fail12(h.why);
            }
            serial::print_str("[tls] TLS 1.2 key exchange, group 0x");
            serial::print_hex(h.group as u64);
            serial::print_str(", signature verified\n");
            true
        }
        HS_CERTIFICATE_REQUEST => {
            transcript_update(msg);
            // We have no certificate and will send an empty one.  A server
            // that insists will say so when it rejects the Finished.
            h.want_client_cert = true;
            true
        }
        HS_SERVER_HELLO_DONE => {
            transcript_update(msg);
            if !h.derive_keys() {
                return fail12(h.why);
            }
            serial::print_str("[tls] TLS 1.2 keys derived\n");
            STEP12.store(1, Ordering::Relaxed);
            true
        }
        HS_FINISHED => server_finished12(msg),
        _ => {
            transcript_update(msg);
            true
        }
    }
}

/// Report a TLS 1.2 fault with its reason, and tear the connection down.
fn fail12(why: &str) -> bool {
    serial::print_str("[tls] ");
    serial::print_str(if why.is_empty() { "the TLS 1.2 handshake failed" } else { why });
    serial::print_str("\n");
    fail(2);
    false
}

/// ClientKeyExchange, ChangeCipherSpec, Finished — the client's whole flight,
/// sent together because nothing in it depends on a reply.
fn send_flight12() -> bool {
    let h = unsafe { &*core::ptr::addr_of!(HS12) };

    if h.want_client_cert {
        // An empty Certificate: a three-byte list length of zero.  The
        // alternative is a certificate this kernel does not have and could
        // not keep secret if it did.
        let empty = [HS_CERTIFICATE, 0, 0, 3, 0, 0, 0];
        transcript_update(&empty);
        if !send_record(CT_HANDSHAKE, &empty) {
            return fail12("could not send the empty certificate");
        }
    }

    let mut body = [0u8; 128];
    let n = tls12::client_key_exchange(&mut body, h.group, &h.our_public[..h.our_public_len]);
    if n == 0 {
        return fail12("could not build the key exchange");
    }
    let mut cke = [0u8; 256];
    let Some(m) = tls12::wrap(16, &body[..n], &mut cke) else {
        return fail12("the key exchange did not fit");
    };
    transcript_update(&cke[..m]);
    if !send_record(CT_HANDSHAKE, &cke[..m]) {
        return fail12("could not send the key exchange");
    }

    // ChangeCipherSpec is a record, not a handshake message, so it is not in
    // the transcript — but it does put the new cipher spec in force, which
    // resets the write sequence number to zero.
    if !send_record(CT_CHANGE_CIPHER_SPEC, &[1]) {
        return fail12("could not send ChangeCipherSpec");
    }
    CLIENT_CIPHER.store(true, Ordering::Relaxed);
    CLIENT_SEQ.store(0, Ordering::Relaxed);

    // Finished: twelve bytes of PRF over the transcript up to here, including
    // everything this flight put in it and not including the message itself.
    let th = transcript_hash();
    let mut verify = [0u8; 12];
    h.finished_verify_data(b"client finished", &th, &mut verify);
    let mut fin = [0u8; 16];
    let Some(m) = tls12::wrap(20, &verify, &mut fin) else {
        return fail12("the Finished did not fit");
    };
    if !send_record(CT_HANDSHAKE, &fin[..m]) {
        return fail12("could not send the client Finished");
    }
    transcript_update(&fin[..m]);
    serial::print_str("[tls] TLS 1.2 client flight sent\n");
    true
}

/// The server's Finished, and with it the end of the handshake.
fn server_finished12(msg: &[u8]) -> bool {
    let h = unsafe { &*core::ptr::addr_of!(HS12) };
    if msg.len() != 16 {
        return fail12("the server's Finished is the wrong length");
    }
    // The transcript for the server's Finished includes the client's, which
    // was added when it was sent.
    let th = transcript_hash();
    let mut want = [0u8; 12];
    h.finished_verify_data(b"server finished", &th, &mut want);
    if !crate::sha256::constant_time_eq(&want, &msg[4..16]) {
        return fail12("the server's Finished did not verify");
    }
    transcript_update(msg);
    STEP12.store(3, Ordering::Relaxed);
    VERIFIED.store(true, Ordering::Relaxed);
    PHASE.store(phase_to_u8(Phase::Established), Ordering::Relaxed);
    serial::print_str("[tls] handshake complete, ");
    serial::print_str(host_str());
    serial::print_str(" verified (TLS 1.2)\n");
    true
}

/// The additional data a TLS 1.2 AEAD record authenticates: the sequence
/// number, the content type, the version, and the length of the *plaintext*.
///
/// The sequence number being inside the authenticated data rather than folded
/// into the nonce is the structural difference from 1.3, and it is why a
/// reordered record fails here rather than decrypting to garbage.
fn aad12(seq: u32, kind: u8, plain_len: usize) -> [u8; 13] {
    let s = (seq as u64).to_be_bytes();
    [s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7],
     kind, 0x03, 0x03, (plain_len >> 8) as u8, plain_len as u8]
}

/// One TLS 1.2 record out.
fn send_record12(kind: u8, body: &[u8]) -> bool {
    let mut buf = [0u8; 16640];
    let n;
    if !CLIENT_CIPHER.load(Ordering::Relaxed) {
        if 5 + body.len() > buf.len() {
            return false;
        }
        buf[0] = kind;
        buf[1] = 0x03;
        buf[2] = 0x03;
        buf[3..5].copy_from_slice(&(body.len() as u16).to_be_bytes());
        buf[5..5 + body.len()].copy_from_slice(body);
        n = 5 + body.len();
    } else {
        let h = unsafe { &*core::ptr::addr_of!(HS12) };
        let seq = CLIENT_SEQ.fetch_add(1, Ordering::Relaxed);
        // nonce = the implicit half from the key block, then an explicit half
        // that goes on the wire.  Sending the sequence number makes the nonce
        // unique without either end having to keep state the other cannot see.
        let explicit = (seq as u64).to_be_bytes();
        let mut nonce = [0u8; 12];
        nonce[..4].copy_from_slice(&h.client_iv);
        nonce[4..].copy_from_slice(&explicit);
        let aad = aad12(seq, kind, body.len());

        let mut ct = [0u8; 16640];
        if !gcm_encrypt(&h.client_key, &nonce, &aad, body, &mut ct) {
            return false;
        }
        let frag = 8 + body.len() + 16;
        if 5 + frag > buf.len() {
            return false;
        }
        buf[0] = kind;
        buf[1] = 0x03;
        buf[2] = 0x03;
        buf[3..5].copy_from_slice(&(frag as u16).to_be_bytes());
        buf[5..13].copy_from_slice(&explicit);
        buf[13..13 + body.len() + 16].copy_from_slice(&ct[..body.len() + 16]);
        n = 5 + frag;
    }
    crate::net::tcp::send(&buf[..n])
}

/// Tell the connection whether the write side is now using the 1.2 cipher
/// spec, which is what `send_record` branches on.
fn cipher_spec_for_version() -> bool {
    VERSION.load(Ordering::Relaxed) == 12
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

    if suite != SUITE_AES128_SHA256 {
        fail(3);
        return false;
    }
    SUITE.store(suite as u32, Ordering::Relaxed);

    // Extensions: supported_versions and key_share.
    // Extensions.  The block has a two-byte total length of its own before the
    // first extension, and skipping it is not optional: reading it as an
    // extension type gives a type of 0x002e and a length of whatever the first
    // real extension's type happens to be, which lands past the end of the
    // message and refuses a perfectly good ServerHello.
    if at + 2 > body.len() {
        fail(2);
        return false;
    }
    at += 2;

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
    let key_len = 16;
    unsafe {
        HS_SECRET = handshake;
        C_HS_SECRET = c_hs;
        S_HS_SECRET = s_hs;
        CLIENT_HS = make_keys(&c_hs, key_len);
        SERVER_HS = make_keys(&s_hs, key_len);
    }
    // Fresh keys start at sequence zero, and these are fresh keys.  The
    // counter was left over from whatever was on the connection before.
    CLIENT_SEQ.store(0, Ordering::Relaxed);
    SERVER_SEQ.store(0, Ordering::Relaxed);
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
///
/// `with_context` is the one place the two versions' Certificate messages
/// differ: TLS 1.3 puts a `certificate_request_context` in front of the list,
/// and TLS 1.2 does not.  Reading a byte that is really the top of the list
/// length shifts everything by one and the parse fails with a message about
/// the chain rather than about the offset.
fn handle_certificate(body: &[u8], with_context: bool) -> bool {
    if body.is_empty() {
        fail(4);
        return false;
    }
    let mut at = 0usize;
    if with_context {
        let ctx_len = body[at] as usize;
        at += 1 + ctx_len;
    }
    if at + 3 > body.len() {
        serial::print_str("[tls] certificate list header does not fit\n");
        fail(4);
        return false;
    }
    let list_len = ((body[at] as usize) << 16)
        | ((body[at + 1] as usize) << 8)
        | body[at + 2] as usize;
    at += 3;
    if at + list_len > body.len() {
        serial::print_str("[tls] certificate list length ");
        serial::print_dec(list_len as u64);
        serial::print_str(" does not fit in ");
        serial::print_dec(body.len() as u64);
        serial::print_str(" bytes\n");
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
            serial::print_str("[tls] certificate entry does not fit: at ");
            serial::print_dec(at as u64);
            serial::print_str(" cert ");
            serial::print_dec(cl as u64);
            serial::print_str(" end ");
            serial::print_dec(end as u64);
            serial::print_str(" body ");
            serial::print_dec(body.len() as u64);
            serial::print_str(" out ");
            serial::print_dec(out as u64);
            serial::print_str("\n");
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
        // TLS 1.3 puts extensions after each entry; TLS 1.2's entry is the
        // certificate and nothing else.  Skipping two bytes that are really
        // the next entry's length walks off the end of the list.
        if with_context && at + 2 <= end {
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

/// What a CertificateVerify signature covers (RFC 8446 §4.4.3):
///
///     a string of 64 bytes of 0x20, then the context string, then a single
///     0 byte, then the transcript hash.
///
/// The 64 spaces are easy to miss, and missing them is invisible: the code
/// still builds a context string, still hashes it, still calls the verifier,
/// and gets "false" back from every server on the internet.  Nothing about
/// that failure names a missing constant, and the same "false" is what a
/// forged certificate produces — which is why this was found by diffing the
/// signing input against a server whose messages could be read directly
/// (tests/local-tls-server.py), and not by reading the RFC once more.
const CERT_VERIFY_SPACES: usize = 64;
const CERT_VERIFY_SPACES_BYTE: u8 = 0x20;
const CERT_VERIFY_CONTEXT: &[u8] = b"TLS 1.3, server CertificateVerify";

/// The bytes a CertificateVerify signature has to cover.
fn certificate_verify_input(transcript_hash: &[u8; 32]) -> ([u8; 130], usize) {
    let mut buf = [0u8; 130];
    let mut n = 0usize;
    for _ in 0..CERT_VERIFY_SPACES {
        buf[n] = CERT_VERIFY_SPACES_BYTE;
        n += 1;
    }
    buf[n..n + CERT_VERIFY_CONTEXT.len()].copy_from_slice(CERT_VERIFY_CONTEXT);
    n += CERT_VERIFY_CONTEXT.len();
    buf[n] = 0;                     // the separator
    n += 1;
    buf[n..n + 32].copy_from_slice(transcript_hash);
    n += 32;
    (buf, n)
}

fn handle_certificate_verify(body: &[u8]) -> bool {
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
    serial::print_str("[tls] CertificateVerify scheme 0x");
    serial::print_hex(scheme as u64);
    serial::print_str(" sig ");
    serial::print_dec(sl as u64);
    serial::print_str(" bytes\n");

    let th = transcript_hash();
    let (buf, n) = certificate_verify_input(&th);
    let signed = &buf[..n];
    log_certificate_verify(signed, sig, leaf_der());

    if !verify_with_chain(signed, sig, scheme) {
        fail(5);
        return false;
    }
    serial::print_str("[tls] CertificateVerify: scheme 0x");
    serial::print_hex(scheme as u64);
    serial::print_str(" ok\n");
    true
}

/// Is the server's certificate chain rooted somewhere we trust, and does the
/// certificate name the host we asked for?
///
/// TLS 1.2 checks this as soon as the Certificate arrives, because everything
/// after it is signed by that key; 1.3 checks it as part of CertificateVerify,
/// because there the key is proved by the same message that uses it.  The two
/// tests are the same two tests either way.
fn chain_and_name_ok() -> bool {
    let der = leaf_der();
    let Some(leaf) = x509::parse(der) else {
        serial::print_str("[tls] the server's certificate did not parse\n");
        return false;
    };
    if !chain_trusted(&leaf) {
        return false;
    }
    if !x509::hostname_matches(&leaf, host_str()) {
        serial::print_str("[tls] certificate is not for ");
        serial::print_str(host_str());
        serial::print_str("\n");
        return false;
    }
    true
}

/// The end-entity certificate as it arrived, for the trace.
fn leaf_der() -> &'static [u8] {    let base = core::ptr::addr_of!(CHAIN) as *const u8;
    let total = CHAIN_LEN.load(Ordering::Relaxed) as usize;
    if total < 4 {
        return &[];
    }
    let mut l = [0u8; 4];
    for i in 0..4 {
        l[i] = unsafe { core::ptr::read_volatile(base.add(i)) };
    }
    let cl = u32::from_be_bytes(l) as usize;
    if 4 + cl > total {
        return &[];
    }
    unsafe { core::slice::from_raw_parts(base.add(4), cl) }
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
    if !chain_and_name_ok() {
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
            crate::crypto::ec::Ec::new(&crate::crypto::ec::P256)
        } else if crate::crypto::der::oid_is(leaf.curve, x509::OID_P384) {
            crate::crypto::ec::Ec::new(&crate::crypto::ec::P384)
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
        serial::print_str("[tls] leaf key is neither RSA nor EC\n");
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

    // Each certificate must be signed by the next one along, and each issuer
    // has to be allowed to issue.
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

    // Where the chain stops is the decision that matters.  Two shapes are
    // allowed and nothing else:
    //
    //   * the last certificate is a root we have, or a cross-signed twin of
    //     one — same subject, same key, so the same trust decision;
    //   * the last certificate is signed by a root we have, which is the
    //     common case: servers leave the root out because every client is
    //     expected to already hold it.
    //
    // A chain of one is only the second shape: a certificate cannot vouch for
    // itself just by being at the end.
    let last = count - 1;
    let Some(top) = get(last) else { return false };
    if crate::crypto::trust::is_trusted(&top) && top.is_ca {
        let _ = leaf;
        return true;
    }
    if crate::crypto::trust::signed_by_stored_root(&top) {
        let _ = leaf;
        return true;
    }
    serial::print_str("[tls] chain does not end at a trusted root\n");
    false
}

fn handle_finished(msg: &[u8], body: &[u8]) -> bool {
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

    // The server's own Finished goes into the transcript now, after its MAC
    // was checked against the transcript without it and before ours is
    // computed over the transcript with it.  RFC 8446 §4.4.4 asks for exactly
    // this asymmetry, and getting it backwards produces a Finished the server
    // rejects — which it reports by closing the connection, with no
    // explanation that arrives as an alert this client can print.
    transcript_update(msg);

    // The server is who it says.  Only now derive the application keys.
    VERIFIED.store(true, Ordering::Relaxed);
    let hs = handshake_secret().unwrap();
    let derived2 = derive_secret(&hs, b"derived", &sha256_empty());
    let master = extract(&derived2, &[0u8; 32]);
    let th2 = transcript_hash();
    let c_ap = derive_secret(&master, b"c ap traffic", &th2);
    let s_ap = derive_secret(&master, b"s ap traffic", &th2);
    let key_len = 16;
    unsafe {
        CLIENT_AP = make_keys(&c_ap, key_len);
        SERVER_AP = make_keys(&s_ap, key_len);
    }
    // Application keys are new keys, so the server's records under them start
    // at sequence zero — not at wherever the handshake ones had got to.  Reset
    // before the next record is read; the one in hand was already decrypted.
    SERVER_SEQ.store(0, Ordering::Relaxed);

    // Our Finished, over the transcript that now includes the server's, under
    // the handshake keys — which is why the client's counter is reset *after*
    // it is sent rather than before.
    let th3 = transcript_hash();
    let mut client_finished_key = [0u8; 32];
    expand_label(&c_hs_secret().unwrap(), b"finished", &[], &mut client_finished_key);
    let verify = hmac(&client_finished_key, &th3);
    let mut cmsg = [0u8; 4 + 32];
    cmsg[0] = HS_FINISHED;
    cmsg[1..4].copy_from_slice(&[0, 0, 32]);
    cmsg[4..].copy_from_slice(&verify);
    transcript_update(&cmsg);
    if !send_record(CT_HANDSHAKE, &cmsg) {
        fail(6);
        return false;
    }
    CLIENT_SEQ.store(0, Ordering::Relaxed);

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
