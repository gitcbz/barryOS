//! Users, groups, permissions and password checking.
//!
//! Follows the Linux arrangement: the account list lives in `/etc/passwd`
//! (`name:x:uid:gid:gecos:home:shell`) and the password hashes live in
//! `/etc/shadow`, mode 0600 and owned by root, so an ordinary user can read
//! the account list but not the hashes.
//!
//! Hashes are salted and stretched — `sha256` chained a few hundred times —
//! and compared in constant time.  That is a long way from crypt(3), but it is
//! a real one-way function rather than a checksum dressed up as one.

use crate::fs::{vfs, ramfs};
use crate::serial;
use crate::sha256;
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

pub const UID_ROOT: u32 = 0;
pub const GID_ROOT: u32 = 0;

// --- permission bits -------------------------------------------------------
pub const R: u16 = 4;
pub const W: u16 = 2;
pub const X: u16 = 1;

// --- modes ----------------------------------------------------------------
pub const MODE_FILE: u16 = 0o644;   // rw-r--r--
pub const MODE_EXEC: u16 = 0o755;   // rwxr-xr-x
pub const MODE_DIR:  u16 = 0o755;   // rwxr-xr-x
pub const MODE_RONLY: u16 = 0o444;  // r--r--r--
/// `/etc/shadow`: readable only by root, exactly as on Linux.
pub const MODE_SECRET: u16 = 0o600;

pub const MAX_USERS: usize = 8;

#[derive(Clone, Copy)]
pub struct User {
    pub uid: u32,
    pub gid: u32,
    pub name: [u8; 16],
    pub name_len: usize,
    pub home: [u8; 24],
    pub home_len: usize,
    pub shell: [u8; 16],
    pub shell_len: usize,
}

impl User {
    const fn blank() -> Self {
        Self {
            uid: 0, gid: 0,
            name: [0; 16], name_len: 0,
            home: [0; 24], home_len: 0,
            shell: [0; 16], shell_len: 0,
        }
    }
    pub fn name_str(&self) -> &str {
        core::str::from_utf8(&self.name[..self.name_len]).unwrap_or("?")
    }
    pub fn home_str(&self) -> &str {
        core::str::from_utf8(&self.home[..self.home_len]).unwrap_or("/")
    }
    pub fn shell_str(&self) -> &str {
        core::str::from_utf8(&self.shell[..self.shell_len]).unwrap_or("/bin/sh")
    }
}

static mut USERS: [User; MAX_USERS] = [User::blank(); MAX_USERS];
static USER_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Who owns the shell right now.
static CURRENT_UID: AtomicU32 = AtomicU32::new(UID_ROOT);

pub fn user_count() -> usize {
    USER_COUNT.load(Ordering::Acquire)
}

pub fn user_at(i: usize) -> Option<User> {
    if i >= user_count() {
        return None;
    }
    unsafe { Some(*(core::ptr::addr_of!(USERS) as *const User).add(i)) }
}

pub fn user_by_uid(uid: u32) -> Option<User> {
    (0..user_count()).find_map(|i| user_at(i).filter(|u| u.uid == uid))
}

pub fn user_by_name(name: &str) -> Option<User> {
    (0..user_count()).find_map(|i| user_at(i).filter(|u| u.name_str() == name))
}

/// Copy a uid's name into `out`; returns its length.
///
/// `user_by_uid(..).map(|u| u.name_str())` does not compile — the name borrows
/// from the `User` the closure owns — so callers that just want the text use
/// this instead.
pub fn user_name_into(uid: u32, out: &mut [u8]) -> usize {
    match user_by_uid(uid) {
        Some(u) => {
            let s = u.name_str().as_bytes();
            let n = s.len().min(out.len());
            out[..n].copy_from_slice(&s[..n]);
            n
        }
        None => {
            if !out.is_empty() {
                out[0] = b'?';
                1
            } else {
                0
            }
        }
    }
}

pub fn current_uid() -> u32 {
    CURRENT_UID.load(Ordering::Relaxed)
}

pub fn current_gid() -> u32 {
    user_by_uid(current_uid()).map(|u| u.gid).unwrap_or(GID_ROOT)
}

pub fn current_ids() -> (u32, u32) {
    let uid = current_uid();
    (uid, user_by_uid(uid).map(|u| u.gid).unwrap_or(GID_ROOT))
}

pub fn set_current_uid(uid: u32) -> bool {
    if user_by_uid(uid).is_none() {
        return false;
    }
    CURRENT_UID.store(uid, Ordering::Relaxed);
    true
}

pub fn is_root() -> bool {
    current_uid() == UID_ROOT
}

// ---------------------------------------------------------------------------
//  /etc/passwd
// ---------------------------------------------------------------------------

fn copy_into(dst: &mut [u8], s: &str) -> usize {
    let n = s.len().min(dst.len());
    dst[..n].copy_from_slice(&s.as_bytes()[..n]);
    n
}

/// Read `/etc/passwd` into the in-memory account table.
pub fn load_from_passwd() -> usize {
    let id = vfs::resolve("/etc/passwd");
    if id == 0 {
        serial::print_str("[perm] /etc/passwd is missing\n");
        USER_COUNT.store(0, Ordering::Release);
        return 0;
    }
    let mut buf = [0u8; 1024];
    let n = ramfs::read_file(id, &mut buf);
    let Ok(text) = core::str::from_utf8(&buf[..n]) else {
        return 0;
    };

    let mut count = 0usize;
    let base = unsafe { core::ptr::addr_of_mut!(USERS) as *mut User };

    for line in text.split('\n') {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if count >= MAX_USERS {
            break;
        }
        // name:passwd:uid:gid:gecos:home:shell
        let mut it = line.split(':');
        let name = it.next().unwrap_or("");
        let _pw = it.next().unwrap_or("");
        let uid = it.next().unwrap_or("0");
        let gid = it.next().unwrap_or("0");
        let _gecos = it.next().unwrap_or("");
        let home = it.next().unwrap_or("/");
        let shell = it.next().unwrap_or("/bin/sh");
        if name.is_empty() {
            continue;
        }

        let mut u = User::blank();
        u.name_len = copy_into(&mut u.name, name);
        u.home_len = copy_into(&mut u.home, home);
        u.shell_len = copy_into(&mut u.shell, shell);
        u.uid = parse_u32(uid).unwrap_or(0);
        u.gid = parse_u32(gid).unwrap_or(0);
        unsafe { core::ptr::write_volatile(base.add(count), u) };
        count += 1;
    }

    USER_COUNT.store(count, Ordering::Release);
    count
}

/// Write the initial account files.  This is the "installer" step: a fresh
/// system needs accounts before anyone can log in.
pub fn install_defaults() -> bool {
    let etc = match vfs::lookup(vfs::ROOT_ID, "etc") {
        id if id != 0 => id,
        _ => vfs::create_dir(vfs::ROOT_ID, "etc"),
    };
    if etc == 0 {
        serial::print_str("[perm] could not create /etc\n");
        return false;
    }

    // name:passwd:uid:gid:gecos:home:shell
    let passwd = "root:x:0:0:root:/:/bin/sh\n\
                  user:x:1000:1000:user:/home:/bin/sh\n\
                  guest:x:1001:1001:guest:/home:/bin/sh\n";

    // Default passwords: the account name.  Documented rather than clever —
    // there is no password-changing workflow yet.
    let accounts: [(&str, u32, u32, &str); 3] = [
        ("root", 0, 0, "root"),
        ("user", 1000, 1000, "user"),
        ("guest", 1001, 1001, "guest"),
    ];

    let mut shadow = [0u8; 512];
    let mut k = 0usize;
    for (name, _, _, pw) in accounts.iter() {
        let mut line = [0u8; 160];
        let n = shadow_line(name, pw, &mut line);
        shadow[k..k + n].copy_from_slice(&line[..n]);
        k += n;
    }

    let pid = write_new(etc, "passwd", passwd.as_bytes());
    let sid = write_new(etc, "shadow", &shadow[..k]);
    if pid == 0 || sid == 0 {
        serial::print_str("[perm] could not write account files\n");
        return false;
    }

    // The whole point of shadow: hashes are not world-readable.
    vfs::set_mode(sid, MODE_SECRET);
    vfs::set_owner(sid, UID_ROOT, GID_ROOT);
    vfs::set_mode(pid, MODE_FILE);

    serial::print_str("[perm] installed /etc/passwd and /etc/shadow (");
    serial::print_hex(accounts.len() as u64);
    serial::print_str(" accounts; default passwords are the account names)\n");
    true
}

fn write_new(parent: u64, name: &str, data: &[u8]) -> u64 {
    let id = match vfs::lookup(parent, name) {
        0 => vfs::create_file(parent, name),
        existing => existing,
    };
    if id == 0 {
        return 0;
    }
    ramfs::write_file(id, data);
    id
}

// ---------------------------------------------------------------------------
//  /etc/shadow
// ---------------------------------------------------------------------------

/// How many times the hash is chained.  Not a serious work factor, but it does
/// mean a stolen shadow file cannot be compared against a rainbow table of
/// plain SHA-256 digests.
const HASH_ROUNDS: u32 = 1000;

/// Derive the stored hash: `h = sha256(salt || password)`, then chain
/// `h = sha256(h || salt)` for the remaining rounds.
fn derive(password: &[u8], salt: &[u8], rounds: u32) -> [u8; 32] {
    let mut h = {
        let mut s = sha256::Sha256::new();
        s.update(salt);
        s.update(password);
        s.finish()
    };
    for _ in 1..rounds {
        let mut s = sha256::Sha256::new();
        s.update(&h);
        s.update(salt);
        h = s.finish();
    }
    h
}

fn to_hex(src: &[u8], out: &mut [u8]) -> usize {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let n = (src.len() * 2).min(out.len());
    for i in 0..src.len() {
        if i * 2 + 1 >= n {
            break;
        }
        out[i * 2] = HEX[(src[i] >> 4) as usize];
        out[i * 2 + 1] = HEX[(src[i] & 0xF) as usize];
    }
    n
}

/// Format one `name:$sha256$rounds$salt$hash:...` shadow line.
fn shadow_line(name: &str, password: &str, out: &mut [u8]) -> usize {
    // Salt is derived from the name so a fresh install is reproducible — a
    // real installer would read a random source.
    let mut salt = [0u8; 8];
    let seed = sha256::hash(name.as_bytes());
    salt.copy_from_slice(&seed[..8]);

    let digest = derive(password.as_bytes(), &salt, HASH_ROUNDS);
    let mut salt_hex = [0u8; 16];
    to_hex(&salt, &mut salt_hex);
    let mut hash_hex = [0u8; 64];
    to_hex(&digest, &mut hash_hex);

    let mut s = [0u8; 160];
    let mut k = 0usize;
    let mut push = |bytes: &[u8], k: &mut usize| {
        for &b in bytes {
            if *k < s.len() {
                s[*k] = b;
                *k += 1;
            }
        }
    };
    push(name.as_bytes(), &mut k);
    push(b":$sha256$", &mut k);
    let mut rbuf = [0u8; 10];
    let rn = u32_to_dec(HASH_ROUNDS, &mut rbuf);
    push(&rbuf[..rn], &mut k);
    push(b"$", &mut k);
    push(&salt_hex, &mut k);
    push(b"$", &mut k);
    push(&hash_hex, &mut k);
    // lastchange:min:max:warn:inactive:expire:reserved
    push(b":0:0:99999:7:::\n", &mut k);

    out[..k].copy_from_slice(&s[..k]);
    k
}

/// Check a password against `/etc/shadow`.
pub fn verify_password(name: &str, password: &str) -> bool {
    let id = vfs::resolve("/etc/shadow");
    if id == 0 {
        return false;
    }
    let mut buf = [0u8; 1024];
    let n = ramfs::read_file(id, &mut buf);
    let Ok(text) = core::str::from_utf8(&buf[..n]) else {
        return false;
    };

    for line in text.split('\n') {
        let mut it = line.split(':');
        if it.next().unwrap_or("") != name {
            continue;
        }
        let field = it.next().unwrap_or("");
        // $sha256$<rounds>$<salt hex>$<hash hex>
        let mut parts = field.split('$');
        if parts.next() != Some("") {
            return false;
        }
        if parts.next() != Some("sha256") {
            return false;
        }
        let rounds = parse_u32(parts.next().unwrap_or("")).unwrap_or(0);
        let Some(salt_hex) = parts.next() else { return false };
        let Some(hash_hex) = parts.next() else { return false };
        if rounds == 0 {
            return false;
        }

        let mut salt = [0u8; 8];
        if from_hex(salt_hex, &mut salt) != salt.len() {
            return false;
        }
        let mut want = [0u8; 32];
        if from_hex(hash_hex, &mut want) != want.len() {
            return false;
        }

        let got = derive(password.as_bytes(), &salt, rounds);
        return sha256::constant_time_eq(&got, &want);
    }
    false
}

// ---------------------------------------------------------------------------
//  Helpers
// ---------------------------------------------------------------------------

fn parse_u32(s: &str) -> Option<u32> {
    if s.is_empty() {
        return None;
    }
    let mut v: u32 = 0;
    for ch in s.bytes() {
        if !ch.is_ascii_digit() {
            return None;
        }
        v = v.checked_mul(10)?.checked_add((ch - b'0') as u32)?;
    }
    Some(v)
}

fn u32_to_dec(mut v: u32, out: &mut [u8]) -> usize {
    let mut tmp = [0u8; 10];
    let mut i = tmp.len();
    if v == 0 {
        i -= 1;
        tmp[i] = b'0';
    }
    while v > 0 {
        i -= 1;
        tmp[i] = b'0' + (v % 10) as u8;
        v /= 10;
    }
    let n = (tmp.len() - i).min(out.len());
    out[..n].copy_from_slice(&tmp[i..i + n]);
    n
}

fn from_hex(s: &str, out: &mut [u8]) -> usize {
    let b = s.as_bytes();
    let n = (b.len() / 2).min(out.len());
    for i in 0..n {
        let hi = hex_val(b[i * 2]);
        let lo = hex_val(b[i * 2 + 1]);
        match (hi, lo) {
            (Some(h), Some(l)) => out[i] = (h << 4) | l,
            _ => return i,
        }
    }
    n
}

fn hex_val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
//  Permission checks
// ---------------------------------------------------------------------------

/// The three-bit field that applies to the current user for this vnode.
fn class_bits(perm: vfs::Perm, uid: u32, gid: u32) -> u16 {
    if uid == perm.uid {
        (perm.mode >> 6) & 7
    } else if gid == perm.gid {
        (perm.mode >> 3) & 7
    } else {
        perm.mode & 7
    }
}

/// May the current user do `want` (a combination of R/W/X) to this vnode?
pub fn allowed(id: u64, want: u16) -> bool {
    if is_root() {
        return true;
    }
    let Some(perm) = vfs::get_perm(id) else {
        return false;
    };
    let (uid, gid) = current_ids();
    class_bits(perm, uid, gid) & want == want
}

/// May we create or remove entries in this directory?  Needs write *and*
/// search (execute) on the directory itself, exactly as on Unix.
pub fn may_modify_dir(dir_id: u64) -> bool {
    allowed(dir_id, W | X)
}

/// Render a mode as "rwxr-xr-x", with `d` or `-` in the type column.
pub fn format_mode(vtype: vfs::VnodeType, mode: u16, out: &mut [u8; 10]) {
    out[0] = match vtype {
        vfs::VnodeType::Dir => b'd',
        vfs::VnodeType::Symlink => b'l',
        _ => b'-',
    };
    const FLAGS: [u8; 3] = [b'r', b'w', b'x'];
    for group in 0..3 {
        let bits = (mode >> (6 - group * 3)) & 7;
        for bit in 0..3 {
            out[1 + group * 3 + bit] = if bits & (4 >> bit) != 0 { FLAGS[bit] } else { b'-' };
        }
    }
}

pub fn init() {
    let n = load_from_passwd();
    serial::print_str("[perm] ");
    serial::print_hex(n as u64);
    serial::print_str(" accounts loaded; running as ");
    // Bind the user first: `name_str` borrows from it, and a temporary would
    // not live long enough to pass the &str on.
    match user_by_uid(current_uid()) {
        Some(u) => serial::print_str(u.name_str()),
        None => serial::print_str("?"),
    }
    serial::print_str("\n");
}
