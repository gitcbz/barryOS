//! Virtual File System (VFS) abstraction.
//!
//! A flat table of vnodes, each with a parent link and a small array of child
//! ids.  That is enough for a single-level-deep tree of ordinary directories
//! and keeps everything allocation-free.
//!
//! Uses raw pointers throughout, and `static mut` with `addr_of_mut!` for the
//! table: writing through a pointer derived from an *immutable* static lets
//! the optimiser delete the stores (that is exactly how the GDT silently lost
//! its TSS descriptor).

use crate::serial;
use core::sync::atomic::{AtomicU64, Ordering};

/// Vnode type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum VnodeType {
    File = 0,
    Dir = 1,
    Symlink = 2,
}

pub const MAX_NAME: usize = 31;
/// Largest single file.  Comfortably larger than the editor's buffer.
pub const MAX_FILE_SIZE: usize = 16384;
/// Most children a directory can hold.
pub const MAX_CHILDREN: usize = 16;

/// A virtual node — represents a file or directory.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct Vnode {
    pub id: u64,
    pub vtype: VnodeType,
    pub _pad1: u32,               // align to 8
    pub name: [u8; MAX_NAME + 1],  // 32 bytes
    pub name_len: usize,
    pub size: u64,
    pub parent_id: u64,
    pub child_ids: [u64; MAX_CHILDREN],
    pub child_count: usize,
    pub data_offset: u64,
    /// How many bytes the data pool actually reserved.  `size` is how much of
    /// that is in use; without this, rewriting a file with more content
    /// reuses the old offset and runs over whatever follows it.
    pub data_cap: u64,
    /// Owning user and group.
    pub uid: u32,
    pub gid: u32,
    /// Permission bits: owner/group/other, three bits each (rwx).
    pub mode: u16,
    pub _pad2: u16,
}

impl Vnode {
    pub const fn empty() -> Self {
        Self {
            id: 0,
            vtype: VnodeType::File,
            _pad1: 0,
            name: [0; MAX_NAME + 1],
            name_len: 0,
            size: 0,
            parent_id: 0,
            child_ids: [0; MAX_CHILDREN],
            child_count: 0,
            data_offset: 0,
            data_cap: 0,
            uid: 0,
            gid: 0,
            mode: 0,
            _pad2: 0,
        }
    }
}

pub const MAX_VNODES: usize = 64;

/// Static vnode table (accessed ONLY via raw pointers).
static mut VNODES: [Vnode; MAX_VNODES] = [Vnode::empty(); MAX_VNODES];

static NEXT_ID: AtomicU64 = AtomicU64::new(2);  // start at 2 (root = 1)
pub const ROOT_ID: u64 = 1;

/// One directory entry, for listing.
#[derive(Clone, Copy)]
pub struct DirEntry {
    pub id: u64,
    pub vtype: VnodeType,
    pub size: u64,
    pub name: [u8; MAX_NAME + 1],
    pub name_len: usize,
}

impl DirEntry {
    pub const fn empty() -> Self {
        Self { id: 0, vtype: VnodeType::File, size: 0, name: [0; MAX_NAME + 1], name_len: 0 }
    }
    pub fn name_str(&self) -> &str {
        core::str::from_utf8(&self.name[..self.name_len]).unwrap_or("?")
    }
}

/// Raw pointer to the vnode table.
fn vnodes_ptr() -> *mut Vnode {
    unsafe { core::ptr::addr_of_mut!(VNODES) as *mut Vnode }
}

/// Allocate a new vnode ID.
pub fn alloc_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::SeqCst)
}

/// Find a free vnode slot. Returns index or None.
pub fn alloc_slot() -> Option<usize> {
    let base = vnodes_ptr();
    for i in 0..MAX_VNODES {
        if unsafe { (*base.add(i)).id } == 0 {
            return Some(i);
        }
    }
    None
}

/// Get a raw pointer to a vnode by slot index.
fn vnode_at(idx: usize) -> *mut Vnode {
    unsafe { vnodes_ptr().add(idx) }
}

/// Find a vnode by ID. Returns slot index or None.
pub fn find_slot(id: u64) -> Option<usize> {
    if id == 0 {
        return None;
    }
    let base = vnodes_ptr();
    for i in 0..MAX_VNODES {
        if unsafe { (*base.add(i)).id } == id {
            return Some(i);
        }
    }
    None
}

pub fn exists(id: u64) -> bool {
    find_slot(id).is_some()
}

/// Set the name on a vnode (volatile byte-by-byte, no memcpy).
unsafe fn set_name_raw(vn: *mut Vnode, name: &str) {
    let b = name.as_bytes();
    let n = b.len().min(MAX_NAME);
    let name_ptr = core::ptr::addr_of_mut!((*vn).name) as *mut u8;
    for i in 0..n {
        core::ptr::write_volatile(name_ptr.add(i), b[i]);
    }
    core::ptr::write_volatile(name_ptr.add(n), 0);
    (*vn).name_len = n;
}

/// Copy a vnode's name into `out`; returns its length.
pub fn get_name(id: u64, out: &mut [u8]) -> usize {
    let Some(slot) = find_slot(id) else { return 0 };
    let vn = vnode_at(slot);
    let n = unsafe { (*vn).name_len }.min(out.len());
    let p = unsafe { core::ptr::addr_of!((*vn).name) as *const u8 };
    for (i, slot) in out.iter_mut().enumerate().take(n) {
        *slot = unsafe { core::ptr::read_volatile(p.add(i)) };
    }
    n
}

/// Ownership and permission bits of a vnode.
#[derive(Clone, Copy)]
pub struct Perm {
    pub uid: u32,
    pub gid: u32,
    pub mode: u16,
}

pub fn get_perm(id: u64) -> Option<Perm> {
    find_slot(id).map(|s| {
        let vn = vnode_at(s);
        unsafe { Perm { uid: (*vn).uid, gid: (*vn).gid, mode: (*vn).mode } }
    })
}

pub fn set_mode(id: u64, mode: u16) -> bool {
    match find_slot(id) {
        Some(s) => {
            unsafe { (*vnode_at(s)).mode = mode };
            true
        }
        None => false,
    }
}

pub fn set_owner(id: u64, uid: u32, gid: u32) -> bool {
    match find_slot(id) {
        Some(s) => {
            unsafe {
                (*vnode_at(s)).uid = uid;
                (*vnode_at(s)).gid = gid;
            }
            true
        }
        None => false,
    }
}

/// Get a vnode's name into `out` and return it as a `&str` over that buffer.
pub fn name_into<'a>(id: u64, out: &'a mut [u8]) -> &'a str {
    let n = get_name(id, out);
    core::str::from_utf8(&out[..n]).unwrap_or("?")
}

pub fn get_type(id: u64) -> Option<VnodeType> {
    find_slot(id).map(|s| unsafe { (*vnode_at(s)).vtype })
}

pub fn get_size(id: u64) -> u64 {
    find_slot(id).map(|s| unsafe { (*vnode_at(s)).size }).unwrap_or(0)
}

pub fn get_data_offset(id: u64) -> u64 {
    find_slot(id).map(|s| unsafe { (*vnode_at(s)).data_offset }).unwrap_or(0)
}

pub fn get_capacity(id: u64) -> u64 {
    find_slot(id).map(|s| unsafe { (*vnode_at(s)).data_cap }).unwrap_or(0)
}

pub fn get_parent(id: u64) -> u64 {
    find_slot(id).map(|s| unsafe { (*vnode_at(s)).parent_id }).unwrap_or(0)
}

pub fn child_count(id: u64) -> usize {
    find_slot(id).map(|s| unsafe { (*vnode_at(s)).child_count }).unwrap_or(0)
}

/// Set a vnode's data offset + size (capacity left unchanged).
pub fn set_data(id: u64, offset: u64, size: u64) {
    if let Some(slot) = find_slot(id) {
        let vn = vnode_at(slot);
        unsafe {
            (*vn).data_offset = offset;
            (*vn).size = size;
        }
    }
}

/// Set offset, size and reserved capacity together.
pub fn set_data_full(id: u64, offset: u64, size: u64, cap: u64) {
    if let Some(slot) = find_slot(id) {
        let vn = vnode_at(slot);
        unsafe {
            (*vn).data_offset = offset;
            (*vn).size = size;
            (*vn).data_cap = cap;
        }
    }
}

/// Initialize the VFS — create the root directory vnode.
pub fn init() {
    if let Some(slot) = alloc_slot() {
        let vn = vnode_at(slot);
        unsafe {
            (*vn).id = ROOT_ID;
            (*vn).vtype = VnodeType::Dir;
            set_name_raw(vn, "/");
            (*vn).parent_id = 0;
            // The root belongs to root, and is world-readable/traversable.
            (*vn).uid = crate::fs::perm::UID_ROOT;
            (*vn).gid = crate::fs::perm::GID_ROOT;
            (*vn).mode = crate::fs::perm::MODE_DIR;
        }
        serial::print_str("[vfs] root vnode created: id=1 type=Dir name=/\n");
    }
    serial::print_str("[vfs] VFS initialized (");
    serial::print_hex(MAX_VNODES as u64);
    serial::print_str(" vnode slots, ");
    serial::print_hex(MAX_CHILDREN as u64);
    serial::print_str(" children per directory)\n");
}

/// Clear the vnode table and recreate the root.
///
/// Used when mounting a filesystem from disk: the whole tree is rebuilt from
/// the inode table, so whatever was in memory has to go first.
pub fn reset() {
    let base = vnodes_ptr();
    for i in 0..MAX_VNODES {
        unsafe { core::ptr::write_volatile(base.add(i), Vnode::empty()) };
    }
    NEXT_ID.store(2, Ordering::SeqCst);

    if let Some(slot) = alloc_slot() {
        let vn = vnode_at(slot);
        unsafe {
            (*vn).id = ROOT_ID;
            (*vn).vtype = VnodeType::Dir;
            set_name_raw(vn, "/");
            (*vn).parent_id = 0;
            (*vn).uid = crate::fs::perm::UID_ROOT;
            (*vn).gid = crate::fs::perm::GID_ROOT;
            (*vn).mode = crate::fs::perm::MODE_DIR;
        }
    }
}

/// Create a file vnode under the given parent directory.
/// Returns the new vnode's ID, or 0 on failure.
pub fn create_file(parent_id: u64, name: &str) -> u64 {
    create_node(parent_id, name, VnodeType::File)
}

/// Create a directory vnode under the given parent.
pub fn create_dir(parent_id: u64, name: &str) -> u64 {
    create_node(parent_id, name, VnodeType::Dir)
}

fn create_node(parent_id: u64, name: &str, vtype: VnodeType) -> u64 {
    if name.is_empty() || name.len() > MAX_NAME {
        return 0;
    }
    // Refuse duplicates: a directory with two identical names is unusable.
    if lookup(parent_id, name) != 0 {
        serial::print_str("[vfs] name already exists: ");
        serial::print_str(name);
        serial::print_str("\n");
        return 0;
    }
    if child_count(parent_id) >= MAX_CHILDREN {
        serial::print_str("[vfs] directory full\n");
        return 0;
    }

    let id = alloc_id();
    let slot = match alloc_slot() {
        Some(s) => s,
        None => {
            serial::print_str("[vfs] vnode table full\n");
            return 0;
        }
    };

    // New nodes belong to whoever created them, like any real filesystem.
    let (uid, gid) = crate::fs::perm::current_ids();
    let mode = match vtype {
        VnodeType::Dir => crate::fs::perm::MODE_DIR,
        _ => crate::fs::perm::MODE_FILE,
    };

    let vn = vnode_at(slot);
    unsafe {
        let blank = Vnode { id, vtype, ..Vnode::empty() };
        core::ptr::write_volatile(vn, blank);
        set_name_raw(vn, name);
        (*vn).parent_id = parent_id;
        (*vn).uid = uid;
        (*vn).gid = gid;
        (*vn).mode = mode;
    }

    link_child(parent_id, id);
    id
}

/// Append a child id to a directory's list.
fn link_child(parent_id: u64, child_id: u64) {
    if let Some(pslot) = find_slot(parent_id) {
        let parent = vnode_at(pslot);
        unsafe {
            if (*parent).child_count < MAX_CHILDREN {
                let n = (*parent).child_count;
                (*parent).child_ids[n] = child_id;
                (*parent).child_count = n + 1;
            }
        }
    }
}

/// Remove a child id from a directory's list, preserving the order of the rest.
fn unlink_child(parent_id: u64, child_id: u64) -> bool {
    let Some(pslot) = find_slot(parent_id) else { return false };
    let parent = vnode_at(pslot);
    unsafe {
        let n = (*parent).child_count;
        for i in 0..n {
            if (*parent).child_ids[i] == child_id {
                for j in i..n - 1 {
                    (*parent).child_ids[j] = (*parent).child_ids[j + 1];
                }
                (*parent).child_count = n - 1;
                return true;
            }
        }
    }
    false
}

/// Look up a name in a directory. Returns the child vnode ID or 0.
pub fn lookup(parent_id: u64, name: &str) -> u64 {
    let Some(pslot) = find_slot(parent_id) else { return 0 };
    let parent = vnode_at(pslot);
    let name_bytes = name.as_bytes();
    unsafe {
        for i in 0..(*parent).child_count {
            let cid = (*parent).child_ids[i];
            if let Some(cslot) = find_slot(cid) {
                let child = vnode_at(cslot);
                let cnlen = (*child).name_len;
                if cnlen != name_bytes.len() {
                    continue;
                }
                let cname_ptr = core::ptr::addr_of!((*child).name) as *const u8;
                let mut match_ok = true;
                for j in 0..cnlen {
                    if core::ptr::read_volatile(cname_ptr.add(j)) != name_bytes[j] {
                        match_ok = false;
                        break;
                    }
                }
                if match_ok {
                    return cid;
                }
            }
        }
    }
    0
}

/// List a directory's children.  Returns how many were written.
pub fn read_dir(id: u64, out: &mut [DirEntry]) -> usize {
    let Some(slot) = find_slot(id) else { return 0 };
    let dir = vnode_at(slot);
    let mut n = 0usize;
    unsafe {
        for i in 0..(*dir).child_count {
            if n >= out.len() {
                break;
            }
            let cid = (*dir).child_ids[i];
            let Some(cslot) = find_slot(cid) else { continue };
            let child = vnode_at(cslot);
            let e = &mut out[n];
            e.id = (*child).id;
            e.vtype = (*child).vtype;
            e.size = (*child).size;
            e.name_len = (*child).name_len.min(MAX_NAME);
            let p = core::ptr::addr_of!((*child).name) as *const u8;
            for j in 0..e.name_len {
                e.name[j] = core::ptr::read_volatile(p.add(j));
            }
            e.name[e.name_len] = 0;
            n += 1;
        }
    }
    n
}

/// Delete a file, or an empty directory.  Returns false if it does not exist
/// or the directory is not empty.
///
/// The data pool is a bump allocator, so the bytes are not reclaimed here;
/// `ramfs::unlink` releases them back to the pool's free list.
pub fn unlink(parent_id: u64, name: &str) -> bool {
    let id = lookup(parent_id, name);
    if id == 0 {
        return false;
    }
    if get_type(id) == Some(VnodeType::Dir) && child_count(id) > 0 {
        serial::print_str("[vfs] directory not empty\n");
        return false;
    }
    if !unlink_child(parent_id, id) {
        return false;
    }
    if let Some(slot) = find_slot(id) {
        unsafe {
            core::ptr::write_volatile(vnode_at(slot), Vnode::empty());
        }
    }
    true
}

/// Rename a child of `parent_id`.
pub fn rename(parent_id: u64, old: &str, new: &str) -> bool {
    if new.is_empty() || new.len() > MAX_NAME {
        return false;
    }
    let id = lookup(parent_id, old);
    if id == 0 {
        return false;
    }
    let other = lookup(parent_id, new);
    if other != 0 && other != id {
        return false;                       // name taken
    }
    if let Some(slot) = find_slot(id) {
        unsafe { set_name_raw(vnode_at(slot), new) };
        return true;
    }
    false
}

/// Resolve an absolute path to a vnode id.  Returns 0 if any component is
/// missing.  `..` and `.` are understood; `/` alone is the root.
pub fn resolve(path: &str) -> u64 {
    let mut cur = ROOT_ID;
    for part in path.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            let p = get_parent(cur);
            if p != 0 {
                cur = p;
            }
            continue;
        }
        let next = lookup(cur, part);
        if next == 0 {
            return 0;
        }
        cur = next;
    }
    cur
}

/// Resolve `path` relative to `cwd`; an absolute path starts at the root.
/// Returns 0 if any component is missing.
pub fn resolve_from(cwd: u64, path: &str) -> u64 {
    let mut cur = if path.starts_with('/') { ROOT_ID } else { cwd };
    for part in path.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            let p = get_parent(cur);
            if p != 0 {
                cur = p;
            }
            continue;
        }
        let next = lookup(cur, part);
        if next == 0 {
            return 0;
        }
        cur = next;
    }
    cur
}

/// Split a path into (parent directory, final component).  The parent is
/// resolved relative to `cwd`; returns None if the parent does not exist.
pub fn split_parent(cwd: u64, path: &str) -> Option<(u64, [u8; MAX_NAME + 1], usize)> {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return None;
    }
    let (dir_part, name) = match trimmed.rfind('/') {
        Some(i) => (&trimmed[..i], &trimmed[i + 1..]),
        None => ("", trimmed),
    };
    if name.is_empty() || name.len() > MAX_NAME {
        return None;
    }
    let parent = if dir_part.is_empty() {
        cwd
    } else {
        let id = resolve_from(cwd, dir_part);
        if id == 0 {
            return None;
        }
        id
    };
    if get_type(parent) != Some(VnodeType::Dir) {
        return None;
    }
    let mut buf = [0u8; MAX_NAME + 1];
    let n = name.len();
    buf[..n].copy_from_slice(name.as_bytes());
    Some((parent, buf, n))
}

/// Build the absolute path of a vnode into `out` (e.g. "/home/notes").
/// Returns the length written.
pub fn path_of(id: u64, out: &mut [u8]) -> usize {
    // Walk up collecting ids, then emit them root-first.
    let mut chain = [0u64; 16];
    let mut depth = 0usize;
    let mut cur = id;
    while cur != 0 && cur != ROOT_ID && depth < chain.len() {
        chain[depth] = cur;
        depth += 1;
        cur = get_parent(cur);
    }

    let mut n = 0usize;
    if depth == 0 {
        if !out.is_empty() {
            out[0] = b'/';
            n = 1;
        }
        return n;
    }
    for i in (0..depth).rev() {
        if n >= out.len() {
            break;
        }
        out[n] = b'/';
        n += 1;
        let mut name = [0u8; MAX_NAME + 1];
        let len = get_name(chain[i], &mut name);
        for &b in name.iter().take(len) {
            if n >= out.len() {
                break;
            }
            out[n] = b;
            n += 1;
        }
    }
    n
}

/// Print all vnodes.
pub fn print_table() {
    let base = vnodes_ptr();
    serial::print_str("[vfs] vnode table:\n");
    let mut count = 0;
    for i in 0..MAX_VNODES {
        unsafe {
            let vn = &*base.add(i);
            if vn.id != 0 {
                count += 1;
                let ty = match vn.vtype {
                    VnodeType::File => "FILE",
                    VnodeType::Dir => "DIR ",
                    _ => "LINK",
                };
                serial::print_str("  id=");
                serial::print_hex(vn.id);
                serial::print_str(" [");
                serial::print_str(ty);
                serial::print_str("] ");
                let name = core::str::from_utf8(&vn.name[..vn.name_len]).unwrap_or("?");
                serial::print_str(name);
                serial::print_str(" size=");
                serial::print_hex(vn.size);
                serial::print_str("\n");
            }
        }
    }
    serial::print_str("[vfs] total: ");
    serial::print_hex(count as u64);
    serial::print_str(" vnodes\n");
}

/// List root directory contents (boot diagnostics).
pub fn ls_root() {
    let mut entries = [DirEntry::empty(); MAX_CHILDREN];
    let n = read_dir(ROOT_ID, &mut entries);
    serial::print_str("[ramfs] root directory (");
    serial::print_hex(n as u64);
    serial::print_str(" entries):\n");
    for e in entries.iter().take(n) {
        let ty = match e.vtype {
            VnodeType::Dir => "DIR ",
            _ => "FILE",
        };
        serial::print_str("  ");
        serial::print_str(ty);
        serial::print_str(" ");
        serial::print_str(e.name_str());
        serial::print_str(" (");
        serial::print_hex(e.size);
        serial::print_str(" bytes)\n");
    }
}
