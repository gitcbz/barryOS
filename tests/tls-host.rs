// Host-side driver for the kernel's TLS client.
//
// A TLS handshake is the one thing in this kernel that cannot be checked by
// reading its own output.  Every failure mode that matters — a key schedule off
// by one label, a sequence number that is not reset when the keys change, a
// transcript hash taken one message too early — produces a client that is
// internally consistent and a server that says nothing useful.  The kernel only
// shows that on a serial line, one boot at a time.
//
// So the same sources are compiled here for the host, with `crate::serial`
// replaced by stdout and `crate::net::tcp` by a real socket, and pointed at
// real servers.  That covers the cryptography, the record layer, the handshake
// and certificate validation.  It does NOT cover the kernel's own TCP: that
// layer is replaced here, and has to be exercised by booting.
//
//     tests/run-tls-host.sh www.bilibili.com www.baidu.com chat.deepseek.com
//
// Everything under test is the kernel's own file, reached by `#[path]`.  There
// are no copies.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

#[path = "../kernel/src/sha256.rs"]
mod sha256;

/// The kernel writes its log to COM1.  Here it goes to stdout, unmodified, so
/// the transcript below is the same text a serial capture would show.
mod serial {
    pub fn print_str(s: &str) {
        print!("{}", s);
        let _ = std::io::Write::flush(&mut std::io::stdout());
    }
    pub fn print_dec(v: u64) {
        print!("{}", v);
        let _ = std::io::Write::flush(&mut std::io::stdout());
    }
    pub fn print_hex(v: u64) {
        print!("{:x}", v);
        let _ = std::io::Write::flush(&mut std::io::stdout());
    }
    pub fn write_byte(_b: u8) {}
    pub fn write_char(_c: u8) {}
    pub fn init() {}
}

mod net {
    pub mod arp {
        /// The kernel's own dotted-quad parser is pure, so it is used here
        /// rather than reimplemented.
        pub fn parse_ip(s: &str) -> Option<[u8; 4]> {
            let mut out = [0u8; 4];
            let mut n = 0usize;
            for part in s.split('.') {
                if n == 4 || part.is_empty() {
                    return None;
                }
                out[n] = part.parse().ok()?;
                n += 1;
            }
            if n == 4 {
                Some(out)
            } else {
                None
            }
        }
    }

    /// Name resolution, through the host's resolver.  Same shape as the
    /// kernel's: 0 idle, 1 waiting, 2 answered, 3 failed, and `take_result`
    /// clears the state so a second call returns None.
    pub mod dns {
        use std::net::ToSocketAddrs;
        use std::sync::atomic::{AtomicU8, Ordering};

        static STATE: AtomicU8 = AtomicU8::new(0);
        static mut RESULT: [u8; 4] = [0; 4];

        pub fn server() -> [u8; 4] {
            [127, 0, 0, 1]
        }
        pub fn state() -> u8 {
            STATE.load(Ordering::Relaxed)
        }
        pub fn start(name: &str) -> bool {
            let resolved = (name, 443u16)
                .to_socket_addrs()
                .ok()
                .and_then(|mut it| it.find(|a| a.is_ipv4()));
            match resolved {
                Some(a) => {
                    if let std::net::IpAddr::V4(v4) = a.ip() {
                        unsafe { RESULT = v4.octets() };
                        STATE.store(2, Ordering::Relaxed);
                        true
                    } else {
                        STATE.store(3, Ordering::Relaxed);
                        false
                    }
                }
                None => {
                    STATE.store(3, Ordering::Relaxed);
                    false
                }
            }
        }
        pub fn take_result() -> Option<[u8; 4]> {
            if STATE.load(Ordering::Relaxed) != 2 {
                return None;
            }
            STATE.store(0, Ordering::Relaxed);
            Some(unsafe { RESULT })
        }
    }

    /// A byte stream over a real socket, presenting the same interface the
    /// kernel's TCP does: a receive buffer that is read at an offset and never
    /// consumed by the reader, plus a state the layer above polls.
    pub mod tcp {
        use crate::serial;
        use std::io::{ErrorKind, Read, Write};
        use std::net::{SocketAddr, TcpStream};
        use std::sync::atomic::{AtomicU32, AtomicU8, Ordering};
        use std::time::Duration;

        #[derive(Clone, Copy, PartialEq, Eq, Debug)]
        pub enum State {
            Closed,
            SynSent,
            Established,
            FinWait,
            FinSent,
            Failed,
        }

        const RX_CAP: usize = 32 * 1024;

        static STATE: AtomicU8 = AtomicU8::new(0);
        /// Absolute counts, like the kernel's: `TAIL` is everything appended
        /// and `HEAD` everything released, so `rx_len` is the difference and
        /// neither wraps when a response is larger than the buffer.
        static RX_TAIL: AtomicU32 = AtomicU32::new(0);
        static RX_HEAD: AtomicU32 = AtomicU32::new(0);
        static mut SOCK: Option<TcpStream> = None;
        static mut RX: [u8; RX_CAP] = [0; RX_CAP];
        static mut PEER_CLOSED: bool = false;

        pub fn state() -> State {
            match STATE.load(Ordering::Relaxed) {
                1 => State::SynSent,
                2 => State::Established,
                3 => State::FinWait,
                4 => State::FinSent,
                5 => State::Failed,
                _ => State::Closed,
            }
        }

        pub fn rx_len() -> usize {
            let total = RX_TAIL.load(Ordering::Relaxed);
            let head = RX_HEAD.load(Ordering::Relaxed);
            total.saturating_sub(head) as usize
        }

        pub fn rx_read_at(offset: usize, out: &mut [u8]) -> usize {
            let total = rx_len();
            if offset >= total {
                return 0;
            }
            let n = (total - offset).min(out.len());
            let base = std::ptr::addr_of!(RX) as *const u8;
            let head = RX_HEAD.load(Ordering::Relaxed) as usize % RX_CAP;
            for i in 0..n {
                out[i] = unsafe { std::ptr::read_volatile(base.add((head + offset + i) % RX_CAP)) };
            }
            n
        }

        pub fn rx_release(n: usize) {
            let total = rx_len();
            RX_HEAD.fetch_add(n.min(total) as u32, Ordering::Relaxed);
        }

        pub fn peer_closed() -> bool {
            unsafe { PEER_CLOSED }
        }

        pub fn connect(ip: [u8; 4], port: u16) -> bool {
            reset();
            let addr = SocketAddr::from((ip, port));
            match TcpStream::connect_timeout(&addr, Duration::from_secs(10)) {
                Ok(s) => {
                    let _ = s.set_nodelay(true);
                    let _ = s.set_nonblocking(true);
                    unsafe { SOCK = Some(s) };
                    STATE.store(2, Ordering::Relaxed);
                    true
                }
                Err(e) => {
                    serial::print_str("[host] connect failed: ");
                    serial::print_str(&e.to_string());
                    serial::print_str("\n");
                    STATE.store(5, Ordering::Relaxed);
                    false
                }
            }
        }

        pub fn send(data: &[u8]) -> bool {
            {
                use std::sync::atomic::{AtomicUsize, Ordering as O};
                static WIRE: AtomicUsize = AtomicUsize::new(0);
                if WIRE.load(O::Relaxed) == 0 {
                    let _ = std::fs::create_dir_all("build/trace");
                    let _ = std::fs::write("build/trace/wire-first-send.bin", data);
                    WIRE.store(1, O::Relaxed);
                }
            }
            let Some(s) = (unsafe { SOCK.as_mut() }) else {
                return false;
            };
            let mut off = 0usize;
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while off < data.len() {
                match s.write(&data[off..]) {
                    Ok(0) => return false,
                    Ok(n) => off += n,
                    Err(e) if e.kind() == ErrorKind::WouldBlock => {
                        if std::time::Instant::now() > deadline {
                            return false;
                        }
                        std::thread::sleep(Duration::from_micros(200));
                    }
                    Err(_) => return false,
                }
            }
            true
        }

        /// Move whatever the socket has into the receive buffer, and notice a
        /// close.  Never blocks.
        pub fn tick() {
            let Some(s) = (unsafe { SOCK.as_mut() }) else {
                return;
            };
            let mut tmp = [0u8; 4096];
            loop {
                // Never take more off the socket than there is room to keep.
                // The shim has no window to close, so a peer will happily send
                // past what fits, and bytes read and then dropped are bytes the
                // other end believes were delivered.
                let room = RX_CAP.saturating_sub(rx_len());
                if room == 0 {
                    return;
                }
                let want = room.min(tmp.len());
                match s.read(&mut tmp[..want]) {
                    Ok(0) => {
                        unsafe { PEER_CLOSED = true };
                        STATE.store(0, Ordering::Relaxed);
                        return;
                    }
                    Ok(n) => {
                        crate::RAW.with(|r| r.borrow_mut().extend_from_slice(&tmp[..n]));
                        let used = rx_len();
                        let room = RX_CAP.saturating_sub(used);
                        let take = n.min(room);
                        let tail = RX_TAIL.load(Ordering::Relaxed) as usize % RX_CAP;
                        let base = std::ptr::addr_of_mut!(RX) as *mut u8;
                        for i in 0..take {
                            unsafe { std::ptr::write_volatile(base.add((tail + i) % RX_CAP), tmp[i]) };
                        }
                        RX_TAIL.fetch_add(take as u32, Ordering::Relaxed);
                        if take < n {
                            // Cannot happen: the read was sized to the room.
                            // If it ever does, the bytes taken off the socket
                            // and not kept are bytes the peer believed were
                            // delivered, and the stream is now corrupt.
                            panic!("host tcp: read {} bytes into {} of room", n, room);
                        }
                    }
                    Err(e) if e.kind() == ErrorKind::WouldBlock => return,
                    Err(_) => {
                        STATE.store(5, Ordering::Relaxed);
                        return;
                    }
                }
            }
        }

        pub fn close() {
            if let Some(s) = unsafe { SOCK.as_mut() } {
                let _ = s.shutdown(std::net::Shutdown::Write);
            }
            STATE.store(3, Ordering::Relaxed);
        }

        pub fn reset() {
            unsafe {
                SOCK = None;
                PEER_CLOSED = false;
            }
            RX_TAIL.store(0, Ordering::Relaxed);
            RX_HEAD.store(0, Ordering::Relaxed);
            STATE.store(0, Ordering::Relaxed);
        }
    }
}

// mod.rs rather than the directory: rustc resolves a `#[path]` with no
// extension to `<that path>.rs`, so a bare directory is looked for as a file.
// Naming mod.rs also fixes where the submodules below it are found.
#[path = "../kernel/src/crypto/mod.rs"]
mod crypto;

use crypto::tls::{self, Phase};

/// Everything the server sent, before any of it was interpreted.
thread_local! {
    static RAW: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn usage() -> ! {
    eprintln!("usage: tls-host [--vectors] <host>...");
    std::process::exit(2);
}

/// Drive one connection to completion and print what came back.
///
/// `host` may carry a port, `host:port`, which is what the local test server
/// needs — a self-contained handshake against an implementation that can be
/// read, rather than whatever a network happens to be doing today.
fn fetch(spec: &str) -> bool {
    let (host, port) = match spec.rsplit_once(':') {
        Some((h, p)) => match p.parse::<u16>() {
            Ok(p) => (h, p),
            Err(_) => (spec, 443),
        },
        None => (spec, 443),
    };
    println!("\n=========== {} ===========", spec);
    if !tls::start(host, port) {
        println!("!! could not start: {}", tls::error());
        return false;
    }

    let start = Instant::now();
    loop {
        tls::tick();
        match tls::phase() {
            Phase::Established => break,
            Phase::Failed => {
                println!("!! handshake failed: {}", tls::error());
                dump_trace(host);
                return false;
            }
            _ => {}
        }
        if start.elapsed() > Duration::from_secs(20) {
            println!("!! timed out in phase {:?}", tls::phase());
            dump_trace(host);
            return false;
        }
        std::thread::sleep(Duration::from_micros(500));
    }
    let handshake = start.elapsed();
    println!("-- handshake ok in {} ms", handshake.as_millis());

    let req = format!(
        "GET / HTTP/1.1\r\nHost: {}\r\nUser-Agent: barryOS/0.1\r\n\
         Accept: text/html,*/*\r\nAccept-Encoding: identity\r\n\
         Connection: close\r\n\r\n",
        host
    );
    if !tls::send(req.as_bytes()) {
        println!("!! the request would not go out");
        return false;
    }

    // Read until the server closes or stops sending.
    let start = Instant::now();
    let mut last = 0usize;
    let mut quiet = Instant::now();
    loop {
        tls::tick();
        if tls::phase() == Phase::Failed {
            println!("!! failed mid-response: {}", tls::error());
            return false;
        }
        let n = tls::rx_len();
        if n != last {
            last = n;
            quiet = Instant::now();
        } else if quiet.elapsed() > Duration::from_secs(6) {
            break;
        }
        if start.elapsed() > Duration::from_secs(30) {
            println!("-- stopped reading at the 30 s mark");
            break;
        }
        std::thread::sleep(Duration::from_micros(500));
    }

    let mut head = vec![0u8; last.min(400)];
    let n = tls::rx_read_at(0, &mut head);
    head.truncate(n);
    let text = String::from_utf8_lossy(&head);
    let status = text.lines().next().unwrap_or("(nothing)");
    println!("-- {} bytes of plaintext, {} ms", last, start.elapsed().as_millis());
    println!("-- status: {}", status.trim());
    last > 0
}

/// Write the handshake trace out so a second implementation can check it.
///
/// When a signature will not verify there are two candidate explanations, and
/// "false" from the verifier does not distinguish them.  `tests/check-trace.py`
/// takes these four files and verifies the same signature over the same bytes
/// with an independent library: if that says yes, the fault is in the verifier
/// here; if it says no, the bytes handed to it were not the bytes the server
/// signed.
fn dump_trace(host: &str) {
    let dir = std::path::Path::new("build/trace");
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let name = host.replace('.', "_");
    let mut buf = vec![0u8; 64 * 1024];
    let n = tls::trace_messages(&mut buf);
    let _ = std::fs::write(dir.join(format!("{}-messages.bin", name)), &buf[..n]);
    let _ = std::fs::write(dir.join(format!("{}-at-cv.txt", name)),
                           format!("{}", tls::trace_at_cv()));
    let mut sig = vec![0u8; 1024];
    let n = tls::trace_signature(&mut sig);
    let _ = std::fs::write(dir.join(format!("{}-sig.bin", name)), &sig[..n]);
    let mut signed = vec![0u8; 256];
    let n = tls::trace_signed(&mut signed);
    let _ = std::fs::write(dir.join(format!("{}-signed.bin", name)), &signed[..n]);
    let mut leaf = vec![0u8; 4096];
    let n = tls::trace_leaf(&mut leaf);
    let _ = std::fs::write(dir.join(format!("{}-leaf.der", name)), &leaf[..n]);
    crate::RAW.with(|r| {
        let _ = std::fs::write(dir.join(format!("{}-raw.bin", name)), &r.borrow()[..]);
    });
    println!("-- trace written to build/trace/{}", name);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut hosts: Vec<String> = Vec::new();
    let mut vectors_only = false;

    // Every vector check the kernel runs at boot, on the host, where a failure
    // is a line of output rather than a boot that stops.
    crypto::init();

    for a in &args {
        match a.as_str() {
            "--vectors" => vectors_only = true,
            "-h" | "--help" => usage(),
            s if s.starts_with('-') => usage(),
            s => hosts.push(s.to_string()),
        }
    }
    if hosts.is_empty() && !vectors_only {
        usage();
    }

    let mut bad = 0usize;
    for h in &hosts {
        if !fetch(h) {
            bad += 1;
        }
    }
    if !hosts.is_empty() {
        println!("\n{} of {} host(s) failed", bad, hosts.len());
    }
    std::process::exit(if bad == 0 { 0 } else { 1 });
}
