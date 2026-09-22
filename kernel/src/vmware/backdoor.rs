//! VMware backdoor — detection, direct commands, and the RPCI tools channel.
//!
//! The backdoor is an I/O port (0x5658) with a register protocol: EAX carries
//! the magic `0x564D5868`, ECX the command, EDX the port, and the rest are
//! arguments and results.  One `in` instruction does everything.
//!
//! Two things make this module look the way it does:
//!
//!   * Rust forbids `rbx` as an inline-asm operand — LLVM reserves it — and the
//!     protocol needs EBX, EBP and EDI.  So the register shuttling lives in
//!     `global_asm!` and Rust only fills in and reads back a small struct.
//!
//!   * An `in` from a port the hypervisor does not implement raises #GP on
//!     some hosts, so every probe is gated behind the CPUID hypervisor check
//!     first.  Nothing here runs unless we already know we are on VMware.
//!
//! Protocol cross-checked against open-vm-tools (`vm_rpc_open` / `vm_rpc_send`
//! / `vm_rpc_get_length` / `vm_rpc_get_data`) and OpenBSD's `vmt.c`.

use crate::serial;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// "The" magic number, always in EAX.
const MAGIC: u32 = 0x564D_5868;
/// Command port; the RPC data port is next to it.
const PORT_CMD: u16 = 0x5658;
const PORT_RPC: u16 = 0x5659;

// --- direct commands (ECX low) ---------------------------------------------
const CMD_GETVERSION: u32 = 10;
const CMD_GETHWVERSION: u32 = 17;
const CMD_GETUUID: u32 = 19;
const CMD_GETMEMSIZE: u32 = 20;
const CMD_GETTIME: u32 = 23;          // deprecated, but still answers
const CMD_MESSAGE: u32 = 30;          // the older, register-poking RPCI
const CMD_GETTIMEFULL: u32 = 46;
const CMD_GET_CLIPBOARD_LEN: u32 = 6;
const CMD_GET_CLIPBOARD: u32 = 7;

// --- RPC sub-commands (ECX high) -------------------------------------------
const RPC_OPEN: u32 = 0x00;
const RPC_SET_LENGTH: u32 = 0x01;
const RPC_GET_LENGTH: u32 = 0x03;
const RPC_GET_END: u32 = 0x05;
const RPC_CLOSE: u32 = 0x06;

/// Channel magic words, passed in EBX with RPC_OPEN.
const RPC_OPEN_TCLO: u32 = 0x4F4C_4354;   // 'TCLO' — the tools channel
const RPC_FLAG_COOKIE: u32 = 0x8000_0000;
/// Enhanced (bulk) data transfer flag and its echo on success.
const RPC_ENH_DATA: u32 = 0x0001_0000;

// --- reply flags, returned in ECX high -------------------------------------
const RPC_REPLY_SUCCESS: u32 = 0x0001;
const RPC_REPLY_DORECV: u32 = 0x0002;
const RPC_REPLY_CLOSED: u32 = 0x0004;

/// EAX on return when the command is not implemented.
const UNSUPPORTED: u32 = 0xFFFF_FFFF;

/// The register block handed to `barryos_backdoor`.
///
/// Field order matches the assembly, which loads them in this sequence.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Frame {
    pub eax: u32,
    pub ebx: u32,
    pub ecx: u32,
    pub edx: u32,
    pub esi: u32,
    pub edi: u32,
    pub ebp: u32,
}

impl Frame {
    const fn blank() -> Self {
        Self { eax: MAGIC, ebx: 0, ecx: 0, edx: 0, esi: 0, edi: 0, ebp: 0 }
    }
    /// Pack an RPC command: `sub` in ECX's high half, `channel` (+ port) in
    /// EDX's high half.
    fn rpc_cmd(cmd: u32, sub: u32, channel: u32) -> Self {
        Self {
            eax: MAGIC,
            ebx: 0,
            ecx: (sub << 16) | (cmd & 0xFFFF),
            edx: (channel << 16) | PORT_CMD as u32,
            esi: 0,
            edi: 0,
            ebp: 0,
        }
    }
}

// The three register-block operations.  `rep insb` reads into ES:RDI and
// `rep outsb` writes from DS:RSI, which is exactly where the protocol wants
// the buffer — the same convention open-vm-tools uses.
core::arch::global_asm!(
    r#"
    .text
    .globl barryos_backdoor_in
    .type  barryos_backdoor_in, @function
barryos_backdoor_in:
    push rbx
    push rbp
    push rsi
    push rdi
    mov  r11, rdi
    mov  eax, [r11 +  0]
    mov  ebx, [r11 +  4]
    mov  ecx, [r11 +  8]
    mov  edx, [r11 + 12]
    mov  esi, [r11 + 16]
    mov  edi, [r11 + 20]
    mov  ebp, [r11 + 24]
    in   eax, dx
    mov  [r11 +  0], eax
    mov  [r11 +  4], ebx
    mov  [r11 +  8], ecx
    mov  [r11 + 12], edx
    mov  [r11 + 16], esi
    mov  [r11 + 20], edi
    mov  [r11 + 24], ebp
    pop  rdi
    pop  rsi
    pop  rbp
    pop  rbx
    ret
    .size barryos_backdoor_in, . - barryos_backdoor_in

    .globl barryos_backdoor_outs
    .type  barryos_backdoor_outs, @function
barryos_backdoor_outs:
    push rbx
    push rbp
    push rsi
    push rdi
    mov  r11, rdi
    mov  eax, [r11 +  0]
    mov  ebx, [r11 +  4]
    mov  ecx, [r11 +  8]
    mov  edx, [r11 + 12]
    mov  esi, [r11 + 16]
    mov  edi, [r11 + 20]
    mov  ebp, [r11 + 24]
    cld
    rep outsb
    mov  [r11 +  0], eax
    mov  [r11 +  4], ebx
    mov  [r11 +  8], ecx
    mov  [r11 + 12], edx
    mov  [r11 + 16], esi
    mov  [r11 + 24], ebp
    pop  rdi
    pop  rsi
    pop  rbp
    pop  rbx
    ret
    .size barryos_backdoor_outs, . - barryos_backdoor_outs

    .globl barryos_backdoor_ins
    .type  barryos_backdoor_ins, @function
barryos_backdoor_ins:
    push rbx
    push rbp
    push rsi
    push rdi
    mov  r11, rdi
    mov  eax, [r11 +  0]
    mov  ebx, [r11 +  4]
    mov  ecx, [r11 +  8]
    mov  edx, [r11 + 12]
    mov  esi, [r11 + 16]
    mov  edi, [r11 + 20]
    mov  ebp, [r11 + 24]
    cld
    rep insb
    mov  [r11 +  0], eax
    mov  [r11 +  4], ebx
    mov  [r11 +  8], ecx
    mov  [r11 + 12], edx
    mov  [r11 + 20], edi
    mov  [r11 + 24], ebp
    pop  rdi
    pop  rsi
    pop  rbp
    pop  rbx
    ret
    .size barryos_backdoor_ins, . - barryos_backdoor_ins
"#
);

extern "C" {
    fn barryos_backdoor_in(frame: *mut Frame);
    fn barryos_backdoor_outs(frame: *mut Frame);
    fn barryos_backdoor_ins(frame: *mut Frame);
}

// ---------------------------------------------------------------------------
//  State
// ---------------------------------------------------------------------------

static PRESENT: AtomicBool = AtomicBool::new(false);
static HW_VERSION: AtomicU32 = AtomicU32::new(0);

/// An open RPC channel.  The cookies are handed out at open time and must be
/// presented on every subsequent call on that channel.
#[derive(Clone, Copy)]
pub struct Channel {
    pub id: u32,
    cookie1: u32,
    cookie2: u32,
    open: bool,
}

static mut TCLO: Channel = Channel { id: 0, cookie1: 0, cookie2: 0, open: false };

/// Scratch space for RPCI traffic.  Static rather than stack: the address is
/// handed to the hypervisor, and a large stack buffer in the input loop would
/// be wasteful.
const RPC_BUF: usize = 1024;
static mut RPC_TX: [u8; RPC_BUF] = [0; RPC_BUF];
static mut RPC_RX: [u8; RPC_BUF] = [0; RPC_BUF];

/// Status strings for the UI.
static mut STATUS: [u8; 64] = [0; 64];
static mut STATUS_LEN: usize = 0;

pub static RPC_SENT: AtomicU32 = AtomicU32::new(0);
pub static RPC_REPLIES: AtomicU32 = AtomicU32::new(0);

pub fn present() -> bool {
    PRESENT.load(Ordering::Relaxed)
}

pub fn status_str() -> &'static str {
    unsafe {
        core::str::from_utf8(&STATUS[..STATUS_LEN]).unwrap_or("")
    }
}

fn set_status(s: &str) {
    unsafe {
        let n = s.len().min(STATUS.len());
        STATUS[..n].copy_from_slice(&s.as_bytes()[..n]);
        STATUS_LEN = n;
    }
}

// ---------------------------------------------------------------------------
//  Detection
// ---------------------------------------------------------------------------

/// Detect VMware without touching the backdoor port.
///
/// CPUID leaf 0x40000000 is the architecturally defined "hypervisor vendor and
/// signature" leaf; VMware reports "VMwareVMware" there.  Unlike the port probe
/// this cannot fault, so it is safe to run anywhere.
pub fn detect_vmware() -> bool {
    let hypervisor_present =
        unsafe { core::arch::x86_64::__cpuid(1) }.ecx & (1 << 31) != 0;
    if !hypervisor_present {
        return false;
    }
    let v = unsafe { core::arch::x86_64::__cpuid(0x4000_0000) };
    let got = [
        v.ebx as u8, (v.ebx >> 8) as u8, (v.ebx >> 16) as u8, (v.ebx >> 24) as u8,
        v.ecx as u8, (v.ecx >> 8) as u8, (v.ecx >> 16) as u8, (v.ecx >> 24) as u8,
        v.edx as u8, (v.edx >> 8) as u8, (v.edx >> 16) as u8, (v.edx >> 24) as u8,
    ];
    let want = *b"VMwareVMware";
    let mut i = 0;
    while i < 12 {
        if got[i] != want[i] {
            return false;
        }
        i += 1;
    }
    true
}

// ---------------------------------------------------------------------------
//  Direct commands
// ---------------------------------------------------------------------------

/// Issue a direct command.  Returns None if the hypervisor says it has no
/// such command (EAX comes back as -1) or if we are not on VMware at all.
fn direct(cmd: u32, ebx: u32, ecx_high: u32) -> Option<Frame> {
    if !present() {
        return None;
    }
    let mut f = Frame::blank();
    f.ebx = ebx;
    f.ecx = (ecx_high << 16) | (cmd & 0xFFFF);
    f.edx = PORT_CMD as u32;
    unsafe { barryos_backdoor_in(&mut f) };
    if f.eax == UNSUPPORTED {
        return None;
    }
    Some(f)
}

/// Hypervisor version, as a packed integer.
pub fn version() -> Option<u32> {
    direct(CMD_GETVERSION, 0, 0xFFFF).map(|f| f.eax)
}

pub fn hw_version() -> u32 {
    HW_VERSION.load(Ordering::Relaxed)
}

/// The host's wall clock: (seconds since the Unix epoch, microseconds).
///
/// GETTIMEFULL (46) is the current command; GETTIME (23) is its deprecated
/// predecessor and still answers on older hosts.  Both put seconds in EAX and
/// the sub-second part in EBX.
pub fn host_time() -> Option<(u64, u32)> {
    if let Some(f) = direct(CMD_GETTIMEFULL, 0, 0) {
        if f.eax != 0 {
            return Some((f.eax as u64, f.ebx));
        }
    }
    direct(CMD_GETTIME, 0, 0).map(|f| (f.eax as u64, f.ebx))
}

/// Host memory size in MiB, if the hypervisor reports one.
pub fn host_mem_mib() -> Option<u32> {
    direct(CMD_GETMEMSIZE, 0, 0).map(|f| f.eax)
}

/// The BIOS UUID the host assigned to this VM.
pub fn bios_uuid() -> Option<[u8; 16]> {
    let f = direct(CMD_GETUUID, 0, 0)?;
    // Two 64-bit halves, little-endian within each half.
    let lo = (f.ebx as u64) | ((f.ecx as u64) << 32);
    let hi = (f.edx as u64) | ((f.ebp as u64) << 32);
    let mut out = [0u8; 16];
    out[..8].copy_from_slice(&lo.to_le_bytes());
    out[8..].copy_from_slice(&hi.to_le_bytes());
    Some(out)
}

/// Length of the host clipboard, if the host exposes one.
pub fn clipboard_len() -> Option<u32> {
    // 4 bytes per character on the legacy path, and not length-prefixed by
    // anything we can trust; just report the raw count.
    direct(CMD_GET_CLIPBOARD_LEN, 0, 0).map(|f| f.eax).filter(|&n| n < 64 * 1024)
}

// ---------------------------------------------------------------------------
//  RPCI
// ---------------------------------------------------------------------------

/// Open the TCLO (tools) channel.
fn rpc_open_tclo() -> Option<Channel> {
    if !present() {
        return None;
    }
    let mut f = Frame::rpc_cmd(CMD_MESSAGE, RPC_OPEN, 0);
    f.ebx = RPC_OPEN_TCLO | RPC_FLAG_COOKIE;
    unsafe { barryos_backdoor_in(&mut f) };

    // Success is ECX high == 1 and EDX low == 0; the channel number comes back
    // in EDX high and the two cookies in ESI/EDI.
    if (f.ecx >> 16) != 1 || (f.edx & 0xFFFF) != 0 {
        return None;
    }
    Some(Channel {
        id: f.edx >> 16,
        cookie1: f.esi,
        cookie2: f.edi,
        open: true,
    })
}

fn rpc_close(ch: Channel) {
    if !ch.open {
        return;
    }
    let mut f = Frame::rpc_cmd(CMD_MESSAGE, RPC_CLOSE, ch.id);
    f.esi = ch.cookie1;
    f.edi = ch.cookie2;
    unsafe { barryos_backdoor_in(&mut f) };
}

/// Send `data` on the channel.  Two calls: the length, then the bulk data.
fn rpc_send(ch: Channel, data: &[u8]) -> bool {
    if !ch.open || data.len() > RPC_BUF {
        return false;
    }

    // 1. length
    let mut f = Frame::rpc_cmd(CMD_MESSAGE, RPC_SET_LENGTH, ch.id);
    f.ebx = data.len() as u32;
    f.esi = ch.cookie1;
    f.edi = ch.cookie2;
    unsafe { barryos_backdoor_in(&mut f) };
    if (f.ecx >> 16) & RPC_REPLY_SUCCESS == 0 {
        return false;
    }
    if data.is_empty() {
        return true;
    }

    // 2. the data itself, over the RPC port with the bulk flag
    unsafe {
        let tx = core::ptr::addr_of_mut!(RPC_TX) as *mut u8;
        for (i, &b) in data.iter().enumerate() {
            core::ptr::write_volatile(tx.add(i), b);
        }
    }
    let mut f = Frame::blank();
    f.ebx = RPC_ENH_DATA;
    f.ecx = data.len() as u32;
    f.edx = (ch.id << 16) | PORT_RPC as u32;
    f.esi = unsafe { core::ptr::addr_of!(RPC_TX) as u32 };
    f.edi = ch.cookie2;
    f.ebp = ch.cookie1;
    unsafe { barryos_backdoor_outs(&mut f) };
    if f.ebx != RPC_ENH_DATA {
        return false;
    }
    RPC_SENT.fetch_add(1, Ordering::Relaxed);
    true
}

/// Is there a reply waiting, and how long is it?
fn rpc_pending(ch: Channel) -> Option<(usize, u32)> {
    let mut f = Frame::rpc_cmd(CMD_MESSAGE, RPC_GET_LENGTH, ch.id);
    f.ebx = 0;
    f.esi = ch.cookie1;
    f.edi = ch.cookie2;
    unsafe { barryos_backdoor_in(&mut f) };

    let hi = f.ecx >> 16;
    if hi & RPC_REPLY_SUCCESS == 0 {
        return None;
    }
    if hi & RPC_REPLY_DORECV == 0 {
        return Some((0, 0));        // nothing waiting
    }
    let len = (f.ebx as usize).min(RPC_BUF);
    let dataid = f.edx >> 16;
    Some((len, dataid))
}

/// Read a waiting reply into `RPC_RX`.  Returns its length.
fn rpc_receive(ch: Channel) -> Option<usize> {
    let (len, dataid) = rpc_pending(ch)?;
    if len == 0 {
        return Some(0);
    }
    let mut f = Frame::blank();
    f.ebx = RPC_ENH_DATA;
    f.ecx = len as u32;
    f.edx = (ch.id << 16) | PORT_RPC as u32;
    f.esi = ch.cookie1;
    f.edi = unsafe { core::ptr::addr_of_mut!(RPC_RX) as u32 };
    f.ebp = ch.cookie2;
    unsafe { barryos_backdoor_ins(&mut f) };
    if f.ebx != RPC_ENH_DATA {
        return None;
    }

    // Acknowledge, or the host will not send the next message.
    let mut a = Frame::rpc_cmd(CMD_MESSAGE, RPC_GET_END, ch.id);
    a.ebx = dataid;
    a.esi = ch.cookie1;
    a.edi = ch.cookie2;
    unsafe { barryos_backdoor_in(&mut a) };
    if (a.ecx >> 16) == 0 {
        return None;
    }

    RPC_REPLIES.fetch_add(1, Ordering::Relaxed);
    Some(len)
}

/// Send one RPCI command and wait for the reply.  Returns the payload length
/// in `RPC_RX`, or None.
///
/// A reply is `"1 <data>"` on success and `"0 <error>"` on failure, with a
/// trailing NUL.
pub fn rpci_command(cmd: &str) -> Option<usize> {
    let ch = unsafe { *core::ptr::addr_of!(TCLO) };
    if !ch.open {
        return None;
    }
    if !rpc_send(ch, cmd.as_bytes()) {
        return None;
    }

    // The host answers asynchronously; poll for a bounded number of tries so a
    // silent host cannot hang the caller.
    for _ in 0..20_000 {
        match rpc_receive(ch) {
            Some(0) => continue,
            Some(n) => {
                let rx = unsafe { core::ptr::addr_of!(RPC_RX) as *const u8 };
                let first = unsafe { core::ptr::read_volatile(rx) };
                let second = unsafe { core::ptr::read_volatile(rx.add(1)) };
                if first == b'1' && second == b' ' {
                    return Some(n);
                }
                return None;
            }
            None => return None,
        }
    }
    None
}

/// The payload of the last reply, as a string.
pub fn rpci_reply() -> &'static str {
    unsafe {
        let p = core::ptr::addr_of!(RPC_RX) as *const u8;
        let mut n = 0usize;
        while n < RPC_BUF {
            if core::ptr::read_volatile(p.add(n)) == 0 {
                break;
            }
            n += 1;
        }
        core::str::from_utf8(core::slice::from_raw_parts(p, n)).unwrap_or("")
    }
}

// ---------------------------------------------------------------------------
//  Bring-up
// ---------------------------------------------------------------------------

/// Probe the hypervisor and open the tools channel if there is one.
pub fn init() {
    if !detect_vmware() {
        PRESENT.store(false, Ordering::SeqCst);
        set_status("not running under VMware");
        serial::print_str("[vmware-bd] no VMware hypervisor signature\n");
        return;
    }
    PRESENT.store(true, Ordering::SeqCst);

    let ver = version().unwrap_or(0);
    let hw = direct(CMD_GETHWVERSION, 0, 0).map(|f| f.eax).unwrap_or(0);
    HW_VERSION.store(hw, Ordering::SeqCst);

    serial::print_str("[vmware-bd] backdoor alive, version 0x");
    serial::print_hex(ver as u64);
    serial::print_str(" hw 0x");
    serial::print_hex(hw as u64);
    serial::print_str("\n");

    match rpc_open_tclo() {
        Some(ch) => {
            unsafe { core::ptr::write_volatile(core::ptr::addr_of_mut!(TCLO), ch) };
            serial::print_str("[vmware-bd] TCLO channel open, id=");
            serial::print_hex(ch.id as u64);
            serial::print_str("\n");
            set_status("TCLO channel open");
        }
        None => {
            serial::print_str("[vmware-bd] TCLO channel not available\n");
            set_status("backdoor alive, no TCLO channel");
        }
    }
}

/// Close the channel again (used if the guest shuts down cleanly).
pub fn shutdown() {
    let ch = unsafe { *core::ptr::addr_of!(TCLO) };
    rpc_close(ch);
    unsafe { core::ptr::write_volatile(core::ptr::addr_of_mut!(TCLO), Channel { id: 0, cookie1: 0, cookie2: 0, open: false }) };
}

/// One line of status for the terminal.
pub fn print_status() {
    serial::print_str("[vmware-bd] ");
    serial::print_str(status_str());
    serial::print_str("\n");
}
