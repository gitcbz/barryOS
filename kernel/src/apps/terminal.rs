//! Terminal application — an interactive shell.
//!
//! The screen contents live in a character buffer, not directly in the
//! framebuffer.  The compositor repaints the whole desktop whenever anything
//! changes — dragging a window, opening the launcher, typing — and a terminal
//! that drew straight to the framebuffer would be erased by the very next
//! pass.  `render` replays the buffer instead, so the session survives.
//!
//! Commands operate on the real VFS, relative to a per-terminal working
//! directory, and every filesystem operation goes through the permission
//! checks in `fs::perm`.

use crate::dev::framebuffer;
use crate::fs::{perm, ramfs, vfs};
use crate::serial;
use crate::wm::font;
use crate::wm::window;
use core::sync::atomic::{AtomicU64, Ordering};

/// Character buffer.  Sized for a maximised window; only the part that fits is
/// replayed.
const COLS: usize = 118;
const ROWS: usize = 40;
/// 0 means "empty cell".
static mut CELLS: [[u16; COLS]; ROWS] = [[0; COLS]; ROWS];
/// Cursor position within the buffer.
static mut ROW: usize = 0;
static mut COL: usize = 0;
/// Column where the line currently being typed begins — i.e. just past the
/// prompt.  Reading the input line from column 0 instead would include the
/// prompt itself, so every command would arrive as "$ ps".
static mut INPUT_COL: usize = 0;

/// Colour palette; a cell's high byte indexes this.
const PAL_WHITE: u8 = 0;
const PAL_GREEN: u8 = 1;
const PAL_BLUE: u8 = 2;
const PAL_GREY: u8 = 3;
const PAL_RED: u8 = 4;
const PAL_YELLOW: u8 = 5;
const PALETTE: [(u8, u8, u8); 6] = [
    (0xFF, 0xFF, 0xFF),
    (0x10, 0xB9, 0x81),
    (0xAA, 0xCC, 0xFF),
    (0xCC, 0xCC, 0xCC),
    (0xFF, 0x44, 0x44),
    (0xFF, 0xFF, 0x00),
];

/// Origin of the window we render into, read back from the window table.
static mut ORIGIN_X: u32 = 0;
static mut ORIGIN_Y: u32 = 0;
static mut WIN_ID: u64 = 0;

/// Content area starts below the title bar.
const CONTENT_OFFSET_X: u32 = 8;
const CONTENT_OFFSET_Y: u32 = 28;
const LINE_HEIGHT: u32 = 16;

/// Window geometry (a hint -- `create` may clamp it).
const WIN_POS_X: u32 = 24;
const WIN_POS_Y: u32 = 44;
const WIN_W: u32 = 360;
const WIN_H: u32 = 200;

/// Working directory of this shell.
static CWD: AtomicU64 = AtomicU64::new(vfs::ROOT_ID);

/// How deep we are inside `run`; scripts calling scripts need a limit.
const MAX_RUN_DEPTH: usize = 3;
static mut RUN_DEPTH: usize = 0;

/// Password prompt state.  While `SECRET` is set the line is echoed as '*' and
/// Enter means "check this password", not "run this command".
static mut SECRET: bool = false;
static mut PENDING_USER: [u8; 16] = [0; 16];
static mut PENDING_USER_LEN: usize = 0;

/// Window id, for the compositor to match content to frame.
pub fn window_id() -> u64 { unsafe { WIN_ID } }

/// Initialize the terminal: create its window.
pub fn init() {
    let id = window::create(WIN_POS_X, WIN_POS_Y, WIN_W, WIN_H, "Terminal");
    unsafe { WIN_ID = id; }
    clear_buffer();
    CWD.store(vfs::ROOT_ID, Ordering::SeqCst);
    serial::print_str("[apps] terminal: window created id=");
    serial::print_hex(id);
    serial::print_str("\n");
}

/// Open the terminal, or raise it if it is already running.
pub fn open() {
    if window::exists(window_id()) {
        window::focus(window_id());
        return;
    }
    init();
}

fn clear_buffer() {
    unsafe {
        let p = core::ptr::addr_of_mut!(CELLS) as *mut u16;
        for i in 0..(COLS * ROWS) {
            core::ptr::write_volatile(p.add(i), 0);
        }
        ROW = 0;
        COL = 0;
        INPUT_COL = 0;
    }
}

// ---------------------------------------------------------------------------
//  Buffer writing
// ---------------------------------------------------------------------------

fn put_char(ch: u8, pal: u8) {
    unsafe {
        if COL >= COLS {
            newline();
        }
        let p = core::ptr::addr_of_mut!(CELLS) as *mut u16;
        core::ptr::write_volatile(p.add(ROW * COLS + COL), ((pal as u16) << 8) | ch as u16);
        COL += 1;
    }
}

fn put_str(text: &str, pal: u8) {
    for &ch in text.as_bytes() {
        if ch == b'\n' {
            newline();
        } else {
            put_char(ch, pal);
        }
    }
}

fn put_line(text: &str, pal: u8) {
    put_str(text, pal);
    newline();
}

/// Write the prompt and remember the column typing starts at.
fn prompt() {
    put_str("$ ", PAL_GREEN);
    unsafe { INPUT_COL = COL };
}

/// Append text produced by something other than the shell.
///
/// A program run through the compatibility layer writes here, so its output
/// lands in the same character buffer as everything else and survives the
/// compositor repainting the window.
pub fn write_external(text: &str) {
    put_str(text, PAL_WHITE);
}

fn newline() {
    unsafe {
        COL = 0;
        ROW += 1;
        if ROW >= ROWS {
            scroll_buffer();
            ROW = ROWS - 1;
        }
    }
}

/// Move every row up one and blank the last.
fn scroll_buffer() {
    unsafe {
        let p = core::ptr::addr_of_mut!(CELLS) as *mut u16;
        for r in 1..ROWS {
            for c in 0..COLS {
                let v = core::ptr::read_volatile(p.add(r * COLS + c));
                core::ptr::write_volatile(p.add((r - 1) * COLS + c), v);
            }
        }
        for c in 0..COLS {
            core::ptr::write_volatile(p.add((ROWS - 1) * COLS + c), 0);
        }
    }
}

fn ok(msg: &str) {
    put_line(msg, PAL_GREEN);
}

fn err(msg: &str) {
    put_line(msg, PAL_RED);
}

// ---------------------------------------------------------------------------
//  Rendering
// ---------------------------------------------------------------------------

/// How many columns and rows fit in the window as it is right now.
fn visible_grid() -> (usize, usize) {
    let (_, _, w, h) = window::geometry(unsafe { WIN_ID })
        .unwrap_or((WIN_POS_X, WIN_POS_Y, WIN_W, WIN_H));
    let cols = ((w.saturating_sub(CONTENT_OFFSET_X + 8)) / font::GLYPH_WIDTH) as usize;
    let rows = ((h.saturating_sub(CONTENT_OFFSET_Y + 8)) / LINE_HEIGHT) as usize;
    (cols.max(1).min(COLS), rows.max(1).min(ROWS))
}

/// Replay the buffer into the window.
pub fn render() {
    let (wx, wy, w, h) = window::geometry(unsafe { WIN_ID })
        .unwrap_or((WIN_POS_X, WIN_POS_Y, WIN_W, WIN_H));
    unsafe {
        ORIGIN_X = wx;
        ORIGIN_Y = wy;
    }

    let (cols, rows) = visible_grid();
    let (br, bg, bb) = window::background(unsafe { WIN_ID }).unwrap_or((0x1A, 0x1F, 0x35));

    let cx = wx + CONTENT_OFFSET_X - 4;
    let cy = wy + CONTENT_OFFSET_Y - 4;
    framebuffer::fill_rect(cx, cy, w.saturating_sub(CONTENT_OFFSET_X + 4), h.saturating_sub(CONTENT_OFFSET_Y + 4), br, bg, bb);

    let (cur_row, cur_col) = unsafe { (ROW, COL) };
    let first = if cur_row + 1 > rows { cur_row + 1 - rows } else { 0 };

    for (i, r) in (first..=cur_row).enumerate() {
        let py = cy + 4 + (i as u32) * LINE_HEIGHT;
        for c in 0..cols {
            let v = unsafe {
                let p = core::ptr::addr_of!(CELLS) as *const u16;
                core::ptr::read_volatile(p.add(r * COLS + c))
            };
            if v == 0 {
                continue;
            }
            let pal = (v >> 8) as usize;
            let (cr, cg, cb) = PALETTE.get(pal).copied().unwrap_or((0xFF, 0xFF, 0xFF));
            font::draw_char((v & 0xFF) as u8, cx + 4 + (c as u32) * font::GLYPH_WIDTH, py, cr, cg, cb);
        }
    }

    let cur_view_row = cur_row - first;
    let px = cx + 4 + (cur_col.min(cols - 1) as u32) * font::GLYPH_WIDTH;
    let py = cy + 4 + (cur_view_row as u32) * LINE_HEIGHT + LINE_HEIGHT - 2;
    framebuffer::fill_rect(px, py, font::GLYPH_WIDTH, 2, 0x10, 0xB9, 0x81);

    serial::print_str("[apps] terminal: rendered\n");
}

// ---------------------------------------------------------------------------
//  Keyboard input
// ---------------------------------------------------------------------------

/// Echo a typed character and append it to the line being edited.
pub fn input_char(ch: u8) {
    unsafe {
        // Never let a typed line wrap: `input_enter` reads the command from
        // INPUT_COL on the *current* row, so a wrap would lose its beginning.
        if COL + 1 >= COLS {
            return;
        }
        if SECRET {
            // Passwords are never echoed.
            put_char(b'*', PAL_GREY);
            return;
        }
    }
    put_char(ch, PAL_WHITE);
}

/// Erase the last character of the line being edited.  Stops at the prompt:
/// backspacing over it would corrupt the line the next Enter reads.
pub fn input_backspace() {
    unsafe {
        if COL <= INPUT_COL {
            return;
        }
        COL -= 1;
        let p = core::ptr::addr_of_mut!(CELLS) as *mut u16;
        core::ptr::write_volatile(p.add(ROW * COLS + COL), 0);
    }
}

/// Submit the line being edited.
pub fn input_enter() {
    // Copy the line out to a stack buffer.  Building a `&str` straight off a
    // `static mut` array is the kind of aliasing that lets the optimiser do
    // surprising things.
    let mut buf = [0u8; COLS];
    let n = unsafe {
        // Only the typed part: everything from the prompt onwards.
        let start = INPUT_COL.min(COLS);
        let end = COL.max(start).min(COLS);
        let p = core::ptr::addr_of!(CELLS) as *const u16;
        for (i, slot) in buf.iter_mut().enumerate().take(end - start) {
            *slot = (core::ptr::read_volatile(p.add(ROW * COLS + start + i)) & 0xFF) as u8;
        }
        end - start
    };

    newline();

    // A password prompt: Enter checks the password instead of running a command.
    if unsafe { SECRET } {
        unsafe { SECRET = false };
        let mut name = [0u8; 16];
        let name_len = unsafe { PENDING_USER_LEN };
        unsafe { name[..name_len].copy_from_slice(&PENDING_USER[..name_len]) };
        if n > 0 {
            let pw = core::str::from_utf8(&buf[..n]).unwrap_or("");
            let who = core::str::from_utf8(&name[..name_len]).unwrap_or("");
            finish_su(who, pw);
        }
        prompt();
        return;
    }

    if n == 0 {
        prompt();
        return;
    }
    let line = core::str::from_utf8(&buf[..n]).unwrap_or("");
    process_command(line);
}

/// Complete an `su`: check the password and, if it is right, switch user.
fn finish_su(name: &str, password: &str) {
    if perm::verify_password(name, password) {
        if let Some(u) = perm::user_by_name(name) {
            perm::set_current_uid(u.uid);
            put_str("now running as ", PAL_GREEN);
            put_line(u.name_str(), PAL_GREEN);
            return;
        }
    }
    err("su: authentication failure");
}

/// A key was pressed while the terminal had focus.
pub fn handle_key(key: u8) -> bool {
    use crate::dev::keyboard::{KEY_BACKSPACE, KEY_ENTER};
    match key {
        KEY_ENTER => input_enter(),
        KEY_BACKSPACE => input_backspace(),
        ch if ch >= 0x20 && ch < 0x7F => input_char(ch),
        _ => return false,
    }
    true
}

// ---------------------------------------------------------------------------
//  Output helpers
// ---------------------------------------------------------------------------

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
        put_str(s, PAL_WHITE);
    }
}

fn print_ip(ip: [u8; 4]) {
    for (i, b) in ip.iter().enumerate() {
        if i > 0 {
            put_char(b'.', PAL_GREY);
        }
        print_dec(*b as u64);
    }
}

/// Write the absolute path of a vnode.
fn print_path(id: u64, pal: u8) {
    let mut buf = [0u8; 96];
    let n = vfs::path_of(id, &mut buf);
    if let Ok(s) = core::str::from_utf8(&buf[..n]) {
        put_str(s, pal);
    }
}

/// `<mode> <size> <name>` — the shape `ls -l` gives.
fn print_entry(e: &vfs::DirEntry) {
    let mut mode = [0u8; 10];
    let perm = vfs::get_perm(e.id).unwrap_or(vfs::Perm { uid: 0, gid: 0, mode: 0 });
    perm::format_mode(e.vtype, perm.mode, &mut mode);
    if let Ok(s) = core::str::from_utf8(&mode) {
        put_str(s, if e.vtype == vfs::VnodeType::Dir { PAL_BLUE } else { PAL_GREY });
    }
    put_char(b' ', PAL_GREY);
    // Right-align the size in six columns.
    let mut tmp = [0u8; 20];
    let n = fmt_dec(e.size, &mut tmp);
    for _ in n..6 {
        put_char(b' ', PAL_GREY);
    }
    if let Ok(s) = core::str::from_utf8(&tmp[..n]) {
        put_str(s, PAL_GREY);
    }
    put_char(b' ', PAL_GREY);
    let colour = if e.vtype == vfs::VnodeType::Dir { PAL_BLUE } else { PAL_WHITE };
    put_line(e.name_str(), colour);
}

fn fmt_dec(mut v: u64, out: &mut [u8]) -> usize {
    let mut tmp = [0u8; 20];
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

/// First whitespace-separated word after the command name.
fn arg(line: &str, n: usize) -> &str {
    line.split_whitespace().nth(n).unwrap_or("")
}

// ---------------------------------------------------------------------------
//  Command processing
// ---------------------------------------------------------------------------

/// Process a command string.
pub fn process_command(cmd: &str) {
    serial::print_str("[apps] terminal: cmd=\"");
    serial::print_str(cmd);
    serial::print_str("\"\n");

    put_str(cmd, PAL_WHITE);
    newline();

    let cmd_word = cmd.split(' ').next().unwrap_or("");
    let cwd = CWD.load(Ordering::Relaxed);

    match cmd_word {
        "help" => cmd_help(),
        "ver" => {
            put_line("barryOS v0.12.0", PAL_WHITE);
            put_line("Stage 11 + filesystem permissions", PAL_BLUE);
        }
        "pwd" => print_path(cwd, PAL_WHITE),
        "cd" => cmd_cd(arg(cmd, 1)),
        "ls" => cmd_ls(cwd, arg(cmd, 1)),
        "cat" => cmd_cat(cwd, arg(cmd, 1)),
        "touch" => cmd_touch(cwd, arg(cmd, 1)),
        "mkdir" => cmd_mkdir(cwd, arg(cmd, 1)),
        "rm" => cmd_rm(cwd, arg(cmd, 1)),
        "mv" => cmd_mv(cwd, arg(cmd, 1), arg(cmd, 2)),
        "edit" => cmd_edit(cwd, arg(cmd, 1)),
        "run" => cmd_run(cwd, arg(cmd, 1)),
        "stat" => cmd_stat(cwd, arg(cmd, 1)),
        "chmod" => cmd_chmod(cwd, arg(cmd, 1), arg(cmd, 2)),
        "chown" => cmd_chown(cwd, arg(cmd, 1), arg(cmd, 2)),
        "whoami" => cmd_whoami(),
        "users" => cmd_users(),
        "su" => cmd_su(arg(cmd, 1)),
        "mem" => cmd_mem(),
        "ps" => cmd_ps(),
        "echo" => {
            let rest = cmd.get(5..).unwrap_or("").trim_start();
            put_line(rest, PAL_YELLOW);
        }
        "net" => cmd_net(),
        "sync" => {
            if crate::fs::sync() {
                ok("filesystem written to disk");
            } else {
                err("sync: no disk filesystem (booted from the built-in tree)");
            }
        }
        "df" => cmd_df(),
        "vmx" => cmd_vmx(cmd),
        "ip" => cmd_ip(arg(cmd, 1)),
        "arp" => cmd_arp(arg(cmd, 1)),
        "ping" => cmd_ping(arg(cmd, 1)),
        "dns" => cmd_dns(arg(cmd, 1)),
        "http" => cmd_http(cmd.get(5..).unwrap_or("").trim_start()),
        "browse" => crate::apps::browser::open(),
        "clear" => clear_buffer(),
        "" => {}
        _ => {
            put_str("unknown command: ", PAL_RED);
            put_line(cmd_word, PAL_RED);
            put_line("  try 'help'", PAL_GREY);
        }
    }

    prompt();
}

// ---------------------------------------------------------------------------
//  Navigation
// ---------------------------------------------------------------------------

fn cmd_help() {
    for line in [
        "barryOS shell — filesystem",
        "  ls [path]     list a directory",
        "  cd <path>     change directory",
        "  pwd           print working directory",
        "  cat <file>    print a file",
        "  edit <file>   open in the editor",
        "  touch <file>  create an empty file",
        "  mkdir <dir>   create a directory",
        "  rm <path>     delete",
        "  mv <old> <new> rename",
        "  run <file>    execute a script",
        "permissions",
        "  whoami  users  su <user>",
        "  stat <path>   owner and mode",
        "  chmod <octal> <path>",
        "  chown <user> <path>",
        "system",
        "  ver  mem  ps  net  ip  arp  clear",
        "network",
        "  ip            address, mask, gateway, dns, dhcp state",
        "  ping <host>   three ICMP echoes",
        "  dns <name>    resolve a name",
        "  http <url>    fetch a page and show its head",
        "  browse [url]  open the browser window",
        "storage",
        "  df            filesystem and disks",
        "  sync          write the filesystem to disk",
        "vmware tools",
        "  vmx           backdoor and tools channel",
        "  vmx time      read the host clock",
        "  vmx rpci <c>  send an RPCI command",
    ] {
        let pal = if line.ends_with("permissions") || line.ends_with("system")
            || line.starts_with("barryOS shell") { PAL_GREEN } else { PAL_GREY };
        put_line(line, pal);
    }
}

fn cmd_cd(path: &str) {
    if path.is_empty() {
        // No home directories yet; "/" is the sensible default.
        CWD.store(vfs::ROOT_ID, Ordering::SeqCst);
        print_path(vfs::ROOT_ID, PAL_GREEN);
        newline();
        return;
    }
    let cwd = CWD.load(Ordering::Relaxed);
    let id = vfs::resolve_from(cwd, path);
    if id == 0 {
        err("cd: no such directory");
        return;
    }
    if vfs::get_type(id) != Some(vfs::VnodeType::Dir) {
        err("cd: not a directory");
        return;
    }
    // Traversing a directory needs execute on it.
    if !perm::allowed(id, perm::X) {
        err("cd: permission denied");
        return;
    }
    CWD.store(id, Ordering::SeqCst);
    print_path(id, PAL_GREEN);
    newline();
}

fn cmd_ls(cwd: u64, path: &str) {
    let dir = if path.is_empty() { cwd } else { vfs::resolve_from(cwd, path) };
    if dir == 0 {
        err("ls: no such file or directory");
        return;
    }
    if vfs::get_type(dir) != Some(vfs::VnodeType::Dir) {
        // Listing a plain file: show it, the way `ls file` does.
        let mut mode = [0u8; 10];
        let p = vfs::get_perm(dir).unwrap_or(vfs::Perm { uid: 0, gid: 0, mode: 0 });
        perm::format_mode(vfs::VnodeType::File, p.mode, &mut mode);
        put_str(core::str::from_utf8(&mode).unwrap_or("?"), PAL_GREY);
        put_char(b' ', PAL_GREY);
        put_line(path, PAL_WHITE);
        return;
    }
    // Listing needs read and execute on the directory.
    if !perm::allowed(dir, perm::R) || !perm::allowed(dir, perm::X) {
        err("ls: permission denied");
        return;
    }

    let mut entries = [vfs::DirEntry::empty(); vfs::MAX_CHILDREN];
    let n = vfs::read_dir(dir, &mut entries);

    // Directories first, then files — a listing is much easier to read that
    // way, and it is what `ls --group-directories-first` does.
    for e in entries.iter().take(n) {
        if e.vtype == vfs::VnodeType::Dir {
            print_entry(e);
        }
    }
    for e in entries.iter().take(n) {
        if e.vtype != vfs::VnodeType::Dir {
            print_entry(e);
        }
    }
    if n == 0 {
        put_line("(empty)", PAL_GREY);
    } else {
        put_str("  ", PAL_GREY);
        print_dec(n as u64);
        put_line(" entries", PAL_GREY);
    }
}

// ---------------------------------------------------------------------------
//  Files
// ---------------------------------------------------------------------------

fn cmd_cat(cwd: u64, path: &str) {
    if path.is_empty() {
        err("usage: cat <file>");
        return;
    }
    let id = vfs::resolve_from(cwd, path);
    if id == 0 {
        err("cat: no such file");
        return;
    }
    if vfs::get_type(id) == Some(vfs::VnodeType::Dir) {
        err("cat: is a directory");
        return;
    }
    if !perm::allowed(id, perm::R) {
        err("cat: permission denied");
        return;
    }
    // Read in chunks so the terminal can be given lines as they come.
    let mut buf = [0u8; 512];
    let mut off = 0u64;
    let size = vfs::get_size(id);
    while off < size {
        let got = ramfs::read_file_at(id, off, &mut buf);
        if got == 0 {
            break;
        }
        if let Ok(s) = core::str::from_utf8(&buf[..got]) {
            for line in s.split('\n') {
                if !line.is_empty() {
                    put_line(line, PAL_WHITE);
                }
            }
        }
        off += got as u64;
    }
    if size == 0 {
        put_line("(empty file)", PAL_GREY);
    }
}

fn cmd_touch(cwd: u64, name: &str) {
    if name.is_empty() {
        err("usage: touch <file>");
        return;
    }
    let Some((parent, nbuf, nlen)) = vfs::split_parent(cwd, name) else {
        err("touch: bad path");
        return;
    };
    if !perm::may_modify_dir(parent) {
        err("touch: permission denied");
        return;
    }
    let Ok(name) = core::str::from_utf8(&nbuf[..nlen]) else { return };
    if vfs::lookup(parent, name) != 0 {
        err("touch: already exists");
        return;
    }
    if ramfs::create_file_in(parent, name) == 0 {
        err("touch: could not create");
        return;
    }
    ok("created");
}

fn cmd_mkdir(cwd: u64, name: &str) {
    if name.is_empty() {
        err("usage: mkdir <dir>");
        return;
    }
    let Some((parent, nbuf, nlen)) = vfs::split_parent(cwd, name) else {
        err("mkdir: bad path");
        return;
    };
    if !perm::may_modify_dir(parent) {
        err("mkdir: permission denied");
        return;
    }
    let Ok(name) = core::str::from_utf8(&nbuf[..nlen]) else { return };
    if vfs::create_dir(parent, name) == 0 {
        err("mkdir: could not create (exists? full?)");
        return;
    }
    ok("directory created");
}

fn cmd_rm(cwd: u64, path: &str) {
    if path.is_empty() {
        err("usage: rm <path>");
        return;
    }
    let Some((parent, nbuf, nlen)) = vfs::split_parent(cwd, path) else {
        err("rm: bad path");
        return;
    };
    let Ok(name) = core::str::from_utf8(&nbuf[..nlen]) else { return };
    let id = vfs::lookup(parent, name);
    if id == 0 {
        err("rm: no such file or directory");
        return;
    }
    // Removing an entry rewrites the directory, so that is what needs write.
    if !perm::may_modify_dir(parent) {
        err("rm: permission denied");
        return;
    }
    // Sticky-bit-free model: deleting someone else's file still requires
    // write on the directory, which is the only rule we enforce.
    if ramfs::unlink(parent, name) {
        ok("removed");
    } else {
        err("rm: failed (directory not empty?)");
    }
}

fn cmd_mv(cwd: u64, from: &str, to: &str) {
    if from.is_empty() || to.is_empty() {
        err("usage: mv <old> <new>");
        return;
    }

    // If the destination is an existing directory, move *into* it under the
    // original name, the way `mv` does everywhere else.
    let mut joined = [0u8; 96];
    let mut target = to;
    let dst = vfs::resolve_from(cwd, to);
    if dst != 0 && vfs::get_type(dst) == Some(vfs::VnodeType::Dir) {
        let base = from.trim_end_matches('/').rsplit('/').next().unwrap_or(from);
        let mut k = 0usize;
        for &b in to.trim_end_matches('/').as_bytes() {
            if k < joined.len() { joined[k] = b; k += 1; }
        }
        if k < joined.len() { joined[k] = b'/'; k += 1; }
        for &b in base.as_bytes() {
            if k < joined.len() { joined[k] = b; k += 1; }
        }
        if let Ok(s) = core::str::from_utf8(&joined[..k]) {
            target = s;
        }
    }

    let Some((src_parent, sbuf, slen)) = vfs::split_parent(cwd, from) else {
        err("mv: bad source");
        return;
    };
    let Ok(old) = core::str::from_utf8(&sbuf[..slen]) else { return };
    let Some((dst_parent, dbuf, dlen)) = vfs::split_parent(cwd, target) else {
        err("mv: bad destination");
        return;
    };
    let Ok(new) = core::str::from_utf8(&dbuf[..dlen]) else { return };

    if vfs::lookup(src_parent, old) == 0 {
        err("mv: no such file");
        return;
    }
    if !perm::may_modify_dir(src_parent) || !perm::may_modify_dir(dst_parent) {
        err("mv: permission denied");
        return;
    }

    // Same directory: a rename keeps the inode, so just change the name.
    if src_parent == dst_parent {
        if vfs::rename(src_parent, old, new) {
            ok("renamed");
        } else {
            err("mv: rename failed (name taken?)");
        }
        return;
    }

    // Across directories: copy the contents into a fresh node and drop the
    // original.  Directories would need a recursive move, which this
    // single-level VFS does not model.
    let id = vfs::lookup(src_parent, old);
    if vfs::get_type(id) == Some(vfs::VnodeType::Dir) {
        err("mv: cannot move a directory between parents");
        return;
    }
    let size = (vfs::get_size(id) as usize).min(vfs::MAX_FILE_SIZE);
    let mut buf = [0u8; vfs::MAX_FILE_SIZE];
    let n = ramfs::read_file(id, &mut buf[..size]);

    let new_id = ramfs::create_file_in(dst_parent, new);
    if new_id == 0 {
        err("mv: could not create destination");
        return;
    }
    ramfs::write_file(new_id, &buf[..n]);
    if let Some(p) = vfs::get_perm(id) {
        vfs::set_mode(new_id, p.mode);
    }
    ramfs::unlink(src_parent, old);
    ok("moved");
}

fn cmd_edit(cwd: u64, path: &str) {
    if path.is_empty() {
        err("usage: edit <file>");
        return;
    }
    let id = vfs::resolve_from(cwd, path);
    if id == 0 {
        err("edit: no such file");
        return;
    }
    if vfs::get_type(id) == Some(vfs::VnodeType::Dir) {
        err("edit: is a directory");
        return;
    }
    if !perm::allowed(id, perm::R) {
        err("edit: permission denied (read)");
        return;
    }
    if !perm::allowed(id, perm::W) {
        // Open it anyway, but say plainly that saving will not work.  That is
        // more useful than refusing to show the file at all.
        put_line("warning: read-only, saving will be refused", PAL_YELLOW);
    }
    crate::apps::editor::open_file(id);
    ok("opened in editor");
}

// ---------------------------------------------------------------------------
//  Executables
// ---------------------------------------------------------------------------

/// Run a file as a program.
///
/// There is no user mode yet, so "a program" is a script: the file must carry
/// the execute bit and its lines are fed back through this same parser.
fn cmd_run(cwd: u64, path: &str) {
    if path.is_empty() {
        err("usage: run <file>");
        return;
    }
    let id = vfs::resolve_from(cwd, path);
    if id == 0 {
        err("run: no such file");
        return;
    }
    if vfs::get_type(id) == Some(vfs::VnodeType::Dir) {
        err("run: is a directory");
        return;
    }
    if !perm::allowed(id, perm::X) {
        err("run: permission denied (no execute bit)");
        err("     try: chmod 755 <file>");
        return;
    }
    if !perm::allowed(id, perm::R) {
        err("run: permission denied (cannot read the script)");
        return;
    }

    let mut buf = [0u8; vfs::MAX_FILE_SIZE];
    let n = ramfs::read_file(id, &mut buf);

    // A PE image is not a script.  This has to be decided before anything
    // treats the bytes as text.
    if crate::compat::is_pe(&buf[..n]) {
        run_pe_image(&buf[..n], path);
        return;
    }

    unsafe {
        if RUN_DEPTH >= MAX_RUN_DEPTH {
            err("run: script nesting too deep");
            return;
        }
        RUN_DEPTH += 1;
    }

    let text = core::str::from_utf8(&buf[..n]).unwrap_or("");

    put_str("running ", PAL_GREEN);
    print_path(id, PAL_GREEN);
    put_line(" ...", PAL_GREEN);

    for line in text.split('\n') {
        let line = line.trim();
        // Skip blanks, comments, and the shebang: those are for the loader,
        // which is us.
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.len() >= COLS {
            err("  (line too long, skipped)");
            continue;
        }
        process_command(line);
    }

    unsafe { RUN_DEPTH -= 1 };
    ok("script finished");
}

/// Load and run a PE image, then report what the loader did with it.
fn run_pe_image(data: &[u8], path: &str) {
    put_str("executing ", PAL_GREEN);
    put_str(path, PAL_GREEN);
    put_line(" (PE image)", PAL_GREEN);
    newline();

    match crate::compat::run_pe(data) {
        Ok(r) => {
            newline();
            put_str("exit code ", PAL_GREY);
            print_dec(r.exit_code as u64);
            put_str("   ", PAL_GREY);
            print_dec(r.sections as u64);
            put_str(" sections, ", PAL_GREY);
            print_dec(r.imports as u64);
            put_str(" imports, ", PAL_GREY);
            print_dec(r.relocs as u64);
            put_line(" relocations", PAL_GREY);
        }
        Err(why) => {
            put_str("cannot run: ", PAL_RED);
            put_line(why, PAL_RED);
        }
    }
}

// ---------------------------------------------------------------------------
//  Permissions
// ---------------------------------------------------------------------------

fn cmd_stat(cwd: u64, path: &str) {
    if path.is_empty() {
        err("usage: stat <path>");
        return;
    }
    let id = vfs::resolve_from(cwd, path);
    if id == 0 {
        err("stat: no such file");
        return;
    }
    let ty = vfs::get_type(id).unwrap_or(vfs::VnodeType::File);
    let p = vfs::get_perm(id).unwrap_or(vfs::Perm { uid: 0, gid: 0, mode: 0 });
    let mut mode = [0u8; 10];
    perm::format_mode(ty, p.mode, &mut mode);

    put_str("path   ", PAL_GREY);
    print_path(id, PAL_WHITE);
    newline();
    put_str("mode   ", PAL_GREY);
    put_line(core::str::from_utf8(&mode).unwrap_or("?"), PAL_WHITE);
    put_str("owner  ", PAL_GREY);
    let mut nb = [0u8; 16];
    let nn = perm::user_name_into(p.uid, &mut nb);
    put_line(core::str::from_utf8(&nb[..nn]).unwrap_or("?"), PAL_WHITE);
    put_str("size   ", PAL_GREY);
    print_dec(vfs::get_size(id));
    newline();
}

fn cmd_chmod(cwd: u64, mode_str: &str, path: &str) {
    if mode_str.is_empty() || path.is_empty() {
        err("usage: chmod <octal> <path>   e.g. chmod 755 /bin/hello");
        return;
    }
    let Some(mode) = parse_octal(mode_str) else {
        err("chmod: mode must be octal, e.g. 644 or 755");
        return;
    };
    let id = vfs::resolve_from(cwd, path);
    if id == 0 {
        err("chmod: no such file");
        return;
    }
    // Only the owner (or root) may change a mode — matching what `allowed`
    // does for W, but the owner bit specifically.
    if !perm::is_root() {
        let p = vfs::get_perm(id).unwrap_or(vfs::Perm { uid: 0, gid: 0, mode: 0 });
        if p.uid != perm::current_uid() {
            err("chmod: not the owner");
            return;
        }
    }
    vfs::set_mode(id, mode);
    ok("mode changed");
}

fn cmd_chown(cwd: u64, user: &str, path: &str) {
    if user.is_empty() || path.is_empty() {
        err("usage: chown <user> <path>   (root only)");
        return;
    }
    if !perm::is_root() {
        err("chown: only root can give files away");
        return;
    }
    let Some(u) = perm::user_by_name(user) else {
        err("chown: no such user");
        return;
    };
    let id = vfs::resolve_from(cwd, path);
    if id == 0 {
        err("chown: no such file");
        return;
    }
    vfs::set_owner(id, u.uid, u.gid);
    ok("owner changed");
}

fn cmd_whoami() {
    let mut nb = [0u8; 16];
    let nn = perm::user_name_into(perm::current_uid(), &mut nb);
    put_line(core::str::from_utf8(&nb[..nn]).unwrap_or("?"), PAL_WHITE);
}

fn cmd_users() {
    for i in 0..perm::user_count() {
        let Some(u) = perm::user_at(i) else { continue };
        put_str("  ", PAL_GREY);
        put_str(u.name_str(), if u.uid == perm::current_uid() { PAL_GREEN } else { PAL_WHITE });
        put_str("  uid ", PAL_GREY);
        print_dec(u.uid as u64);
        put_str("  gid ", PAL_GREY);
        print_dec(u.gid as u64);
        put_str("  ", PAL_GREY);
        put_str(u.shell_str(), PAL_GREY);
        newline();
    }
}

fn cmd_su(name: &str) {
    if name.is_empty() {
        err("usage: su <user>");
        return;
    }
    let Some(u) = perm::user_by_name(name) else {
        err("su: no such user");
        return;
    };
    // Ask for the password rather than switching outright: the account list is
    // world-readable, so without this `su root` would be a formality.
    let n = u.name_str().len().min(16);
    unsafe {
        PENDING_USER[..n].copy_from_slice(&u.name_str().as_bytes()[..n]);
        PENDING_USER_LEN = n;
        SECRET = true;
    }
    put_str("Password: ", PAL_GREY);
    // Typing now starts after the prompt, not at column 0.
    unsafe { INPUT_COL = COL };
}

/// Parse a 3- or 4-digit octal mode.
fn parse_octal(s: &str) -> Option<u16> {
    let mut v: u16 = 0;
    let mut digits = 0;
    for ch in s.bytes() {
        if !(b'0'..=b'7').contains(&ch) {
            return None;
        }
        v = v * 8 + (ch - b'0') as u16;
        digits += 1;
        if digits > 4 {
            return None;
        }
    }
    if digits == 0 || v > 0o7777 {
        return None;
    }
    Some(v & 0o777)
}

// ---------------------------------------------------------------------------
//  System commands
// ---------------------------------------------------------------------------

fn cmd_mem() {
    let fa = crate::mem::frame_allocator();
    put_str("frames usable ", PAL_GREY);
    print_dec(fa.usable_pages() as u64);
    put_str(" / ", PAL_GREY);
    print_dec(fa.used_pages() as u64);
    put_line(" used", PAL_GREY);
    put_str("file data pool ", PAL_GREY);
    print_dec(ramfs::bytes_used());
    put_str(" bytes used, ", PAL_GREY);
    print_dec(ramfs::bytes_free() as u64);
    put_line(" free", PAL_GREY);
}

fn cmd_ps() {
    put_line("4 processes", PAL_WHITE);
    for p in ["  PID0 idle", "  PID1 thread-A", "  PID2 thread-B", "  PID3 thread-C"] {
        put_line(p, PAL_GREY);
    }
}

// ---------------------------------------------------------------------------
//  VMware
// ---------------------------------------------------------------------------

fn cmd_vmx(line: &str) {
    // "vmx" / "vmx time" / "vmx rpci <command>"
    let sub = arg(line, 1);
    match sub {
        "" | "status" => {
            put_str("backdoor   ", PAL_GREY);
            put_line(crate::vmware::status_line(), PAL_WHITE);
            if !crate::vmware::present() {
                return;
            }
            put_str("hw version ", PAL_GREY);
            print_dec(crate::vmware::backdoor::hw_version() as u64);
            newline();
            put_str("rpci       ", PAL_GREY);
            print_dec(crate::vmware::backdoor::RPC_SENT.load(core::sync::atomic::Ordering::Relaxed) as u64);
            put_str(" sent, ", PAL_GREY);
            print_dec(crate::vmware::backdoor::RPC_REPLIES.load(core::sync::atomic::Ordering::Relaxed) as u64);
            put_line(" replies", PAL_GREY);
            if let Some(mib) = crate::vmware::backdoor::host_mem_mib() {
                put_str("host mem   ", PAL_GREY);
                print_dec(mib as u64);
                put_line(" MiB", PAL_GREY);
            }
        }
        "time" => match crate::vmware::host_time() {
            Some((secs, usec)) => {
                put_str("host clock ", PAL_GREEN);
                print_date(secs);
                newline();
                put_str("epoch      ", PAL_GREY);
                print_dec(secs);
                put_char(b'.', PAL_GREY);
                // Microseconds, zero-padded.
                let mut us = usec;
                let mut pad = [0u8; 6];
                for i in (0..6).rev() {
                    pad[i] = b'0' + (us % 10) as u8;
                    us /= 10;
                }
                if let Ok(s) = core::str::from_utf8(&pad) {
                    put_line(s, PAL_GREY);
                }
            }
            None => err("vmx: the host did not report a time"),
        },
        "rpci" => {
            // Everything after the second word is the command, spaces and all.
            let rest = line.splitn(3, ' ').nth(2).unwrap_or("").trim();
            if rest.is_empty() {
                err("usage: vmx rpci <command>   e.g. vmx rpci info-get guestinfo.ip");
                return;
            }
            put_str("rpci > ", PAL_GREY);
            put_line(rest, PAL_WHITE);
            match crate::vmware::rpci(rest) {
                Some(reply) => {
                    put_str("rpci < ", PAL_GREEN);
                    put_line(reply, PAL_WHITE);
                }
                None => err("no reply (channel closed, or the host refused)"),
            }
        }
        _ => err("usage: vmx [status|time|rpci <command>]"),
    }
}

/// Format a Unix timestamp as `YYYY-MM-DD HH:MM:SS UTC`.
///
/// The date arithmetic is Howard Hinnant's `civil_from_days`: shift the epoch
/// to March (so leap days land at the end of the year), count 400-year eras,
/// then un-shift.  An epoch number on its own tells a human nothing.
fn print_date(secs: u64) {
    let days = (secs / 86400) as i64;
    let rem = secs % 86400;

    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let mut y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u64;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    if m <= 2 {
        y += 1;
    }

    let two = |v: u64, out: &mut [u8; 19], at: usize| {
        out[at] = b'0' + ((v / 10) % 10) as u8;
        out[at + 1] = b'0' + (v % 10) as u8;
    };
    let mut b = [0u8; 19];
    let year = y as u64;
    b[0] = b'0' + ((year / 1000) % 10) as u8;
    b[1] = b'0' + ((year / 100) % 10) as u8;
    two(year, &mut b, 2);
    b[4] = b'-';
    two(m, &mut b, 5);
    b[7] = b'-';
    two(d, &mut b, 8);
    b[10] = b' ';
    two(rem / 3600, &mut b, 11);
    b[13] = b':';
    two((rem % 3600) / 60, &mut b, 14);
    b[16] = b':';
    two(rem % 60, &mut b, 17);
    if let Ok(s) = core::str::from_utf8(&b) {
        put_str(s, PAL_WHITE);
    }
    put_str(" UTC", PAL_GREY);
}

fn cmd_df() {
    put_str("filesystem ", PAL_GREY);
    match crate::fs::mount_drive() {
        Some(d) => {
            put_str("barryFS on drive ", PAL_GREEN);
            print_dec(d as u64);
            newline();
        }
        None => {
            put_line("built-in RAM tree (nothing installed)", PAL_YELLOW);
        }
    }
    put_str("data pool  ", PAL_GREY);
    print_dec(ramfs::bytes_used());
    put_str(" used, ", PAL_GREY);
    print_dec(ramfs::bytes_free() as u64);
    put_line(" free", PAL_GREY);
    if crate::dev::ata::present() {
        put_str("disks      ", PAL_GREY);
        print_dec(crate::dev::ata::count() as u64);
        put_line(" attached", PAL_GREY);
    } else {
        put_line("disks      none (ATA probe found nothing)", PAL_YELLOW);
    }
}

fn cmd_net() {
    crate::net::print_status();
    let present = crate::net::link_present();
    put_str("adapter  ", PAL_GREY);
    put_line(if present { "Intel e1000" } else { "absent" }, PAL_WHITE);
    if present {
        put_str("link     ", PAL_GREY);
        put_line(if crate::net::e1000::link_up() { "up" } else { "down" }, PAL_WHITE);
        put_str("mac      ", PAL_GREY);
        if let Some(m) = crate::net::e1000::mac() {
            for (i, b) in m.iter().enumerate() {
                if i > 0 {
                    put_char(b':', PAL_WHITE);
                }
                put_hex_byte(*b);
            }
        }
        newline();
        put_str("ip       ", PAL_GREY);
        print_ip(crate::net::arp::our_ip());
        newline();
    }
}

fn cmd_ip(arg: &str) {
    if arg.is_empty() {
        // No argument: report rather than guess.  Every one of these came
        // from DHCP, so printing them is how you tell a lease from a fallback.
        put_str("address  ", PAL_GREY);
        print_ip(crate::net::ipv4::our_ip());
        newline();
        put_str("mask     ", PAL_GREY);
        print_ip(crate::net::ipv4::mask());
        newline();
        put_str("gateway  ", PAL_GREY);
        print_ip(crate::net::ipv4::gateway());
        newline();
        put_str("dns      ", PAL_GREY);
        print_ip(crate::net::dns::server());
        newline();
        put_str("dhcp     ", PAL_GREY);
        put_line(crate::net::dhcp::status_str(), PAL_WHITE);
        return;
    }
    match crate::net::arp::parse_ip(arg) {
        Some(ip) => {
            crate::net::arp::set_our_ip(ip);
            put_str("ip set to ", PAL_GREY);
            print_ip(ip);
            newline();
        }
        None => err("usage: ip [a.b.c.d]"),
    }
}

// ---------------------------------------------------------------------------
//  Network
// ---------------------------------------------------------------------------

fn now_ticks() -> u64 {
    crate::interrupts::TIMER_TICKS.load(Ordering::Relaxed)
}

/// Drive the stack for `ticks` PIT ticks.
///
/// There is no preemption and no network thread: the only thing that moves a
/// packet is `net::poll`, so anything that waits for a reply waits by calling
/// it.  The compositor stops for the duration, which is why every use of this
/// is bounded by a small number of ticks.
fn net_wait(ticks: u64) {
    let start = now_ticks();
    while now_ticks().saturating_sub(start) < ticks {
        crate::net::poll();
    }
}

/// Parse an address, or resolve a name if it is not one.
fn resolve(target: &str) -> Option<[u8; 4]> {
    if let Some(ip) = crate::net::arp::parse_ip(target) {
        return Some(ip);
    }
    if crate::net::dns::server() == [0, 0, 0, 0] {
        err("no DNS server (DHCP did not hand one out)");
        return None;
    }
    put_str("resolving ", PAL_GREY);
    put_line(target, PAL_GREY);
    if !crate::net::dns::start(target) {
        err("dns: could not send the query");
        return None;
    }
    let start = now_ticks();
    while crate::net::dns::state() == 1 && now_ticks().saturating_sub(start) < 600 {
        crate::net::poll();
    }
    match crate::net::dns::take_result() {
        Some(ip) => Some(ip),
        None => {
            err("dns: no answer");
            None
        }
    }
}

fn cmd_dns(arg: &str) {
    if arg.is_empty() {
        err("usage: dns <name>");
        return;
    }
    match resolve(arg) {
        Some(ip) => {
            put_str(arg, PAL_WHITE);
            put_str("  ", PAL_GREY);
            print_ip(ip);
            newline();
        }
        None => {}
    }
}

fn cmd_ping(arg: &str) {
    if arg.is_empty() {
        err("usage: ping <host|a.b.c.d>");
        return;
    }
    let Some(ip) = resolve(arg) else { return };

    put_str("PING ", PAL_GREY);
    put_line(arg, PAL_WHITE);
    let mut replies = 0u32;
    for seq in 1..=3u16 {
        let mut reply = None;
        // The first packet to a new destination cannot go out until ARP has
        // answered, so an attempt that fails to send is retried rather than
        // counted as a lost packet.
        for _ in 0..4 {
            if crate::net::icmp::request(ip, seq) {
                let start = now_ticks();
                while now_ticks().saturating_sub(start) < 100 {
                    crate::net::poll();
                    if let Some(r) = crate::net::icmp::take_reply() {
                        reply = Some(r);
                        break;
                    }
                }
                if reply.is_some() {
                    break;
                }
            } else {
                net_wait(20);                   // ARP in flight
            }
        }
        put_str("  seq ", PAL_GREY);
        print_dec(seq as u64);
        put_str("  ", PAL_GREY);
        match reply {
            Some((from, rtt)) => {
                replies += 1;
                print_ip(from);
                put_str("  ", PAL_GREY);
                print_dec(rtt as u64);
                put_line(" ms", PAL_WHITE);
            }
            None => put_line("no reply", PAL_RED),
        }
    }
    put_str("3 packets transmitted, ", PAL_GREY);
    print_dec(replies as u64);
    put_line(" received", PAL_GREY);
}

fn cmd_http(url: &str) {
    use crate::net::http;
    if url.is_empty() {
        err("usage: http <host[/path]>");
        return;
    }
    put_str("GET ", PAL_GREY);
    put_line(url, PAL_WHITE);
    if !http::start(url) {
        err(http::error());
        return;
    }
    let start = now_ticks();
    while http::in_progress() {
        crate::net::poll();
        if now_ticks().saturating_sub(start) > 3000 {
            err("gave up waiting (30 s)");
            return;
        }
    }
    if http::phase() != http::Phase::Done {
        err(http::error());
        return;
    }

    put_str("status ", PAL_GREY);
    print_dec(http::status_code() as u64);
    if http::is_redirect() {
        put_str("  (redirect)", PAL_YELLOW);
    }
    newline();
    put_str("body ", PAL_GREY);
    print_dec(http::body_len() as u64);
    put_line(" bytes", PAL_GREY);

    // Show the head of it.  The terminal buffer is 118 columns by 40 rows and
    // already full of the session, so this is a taste, not a viewer — the
    // browser window is the thing that displays a page.
    let mut buf = [0u8; 1024];
    let n = http::read_body(0, &mut buf);
    let shown = n.min(600);
    put_line("--- body ---", PAL_BLUE);
    for line in core::str::from_utf8(&buf[..shown]).unwrap_or("").lines().take(12) {
        put_line(line, PAL_WHITE);
    }
    put_line("--- end ---", PAL_BLUE);
}

fn cmd_arp(arg: &str) {
    match crate::net::arp::parse_ip(arg) {
        Some(ip) => {
            put_str("arp who-has ", PAL_GREY);
            print_ip(ip);
            newline();
            if crate::net::arp::request(ip) {
                ok("  request sent");
            } else {
                err("  send failed (no adapter)");
            }
        }
        None => err("usage: arp a.b.c.d"),
    }
}

fn put_hex_byte(b: u8) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    put_char(HEX[(b >> 4) as usize], PAL_WHITE);
    put_char(HEX[(b & 0xF) as usize], PAL_WHITE);
}
