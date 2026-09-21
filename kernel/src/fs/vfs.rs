//! Virtual File System (VFS) abstraction.
//!
//! Uses raw pointers throughout to avoid Rust 2024 `static mut` reference UB.

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
pub const MAX_FILE_SIZE: usize = 4096;

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
    pub child_ids: [u64; 16],
    pub child_count: usize,
    pub data_offset: u64,
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
            child_ids: [0; 16],
            child_count: 0,
            data_offset: 0,
        }
    }
}

pub const MAX_VNODES: usize = 64;

/// Static vnode table (accessed ONLY via raw pointers).
static mut VNODES: [Vnode; MAX_VNODES] = [Vnode::empty(); MAX_VNODES];

static NEXT_ID: AtomicU64 = AtomicU64::new(2);  // start at 2 (root = 1)
pub const ROOT_ID: u64 = 1;

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
    let base = vnodes_ptr();
    for i in 0..MAX_VNODES {
        if unsafe { (*base.add(i)).id } == id {
            return Some(i);
        }
    }
    None
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

/// Initialize the VFS — create the root directory vnode.
pub fn init() {
    if let Some(slot) = alloc_slot() {
        let vn = vnode_at(slot);
        unsafe {
            (*vn).id = ROOT_ID;
            (*vn).vtype = VnodeType::Dir;
            set_name_raw(vn, "/");
            (*vn).parent_id = 0;
        }
        serial::print_str("[vfs] root vnode created: id=1 type=Dir name=/\n");
    }
    serial::print_str("[vfs] VFS initialized (");
    serial::print_hex(MAX_VNODES as u64);
    serial::print_str(" vnode slots)\n");
}

/// Create a file vnode under the given parent directory.
/// Returns the new vnode's ID, or 0 on failure.
pub fn create_file(parent_id: u64, name: &str) -> u64 {
    let id = alloc_id();
    let slot = match alloc_slot() {
        Some(s) => s,
        None => {
            serial::print_str("[vfs] vnode table full\n");
            return 0;
        }
    };

    let vn = vnode_at(slot);
    unsafe {
        (*vn).id = id;
        (*vn).vtype = VnodeType::File;
        set_name_raw(vn, name);
        (*vn).parent_id = parent_id;
        (*vn).size = 0;
        (*vn).child_count = 0;
        (*vn).data_offset = 0;
    }

    // Add to parent's child list.
    if let Some(pslot) = find_slot(parent_id) {
        let parent = vnode_at(pslot);
        unsafe {
            if (*parent).child_count < 16 {
                (*parent).child_ids[(*parent).child_count] = id;
                (*parent).child_count += 1;
            }
        }
    }

    id
}

/// Look up a name in a directory. Returns the child vnode ID or 0.
pub fn lookup(parent_id: u64, name: &str) -> u64 {
    let pslot = match find_slot(parent_id) {
        Some(s) => s,
        None => return 0,
    };
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
                // Byte-by-byte comparison (avoid memcmp).
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

/// Get a vnode's size by ID.
pub fn get_size(id: u64) -> u64 {
    if let Some(slot) = find_slot(id) {
        let vn = vnode_at(slot);
        unsafe { (*vn).size }
    } else {
        0
    }
}

/// Get a vnode's data offset by ID.
pub fn get_data_offset(id: u64) -> u64 {
    if let Some(slot) = find_slot(id) {
        let vn = vnode_at(slot);
        unsafe { (*vn).data_offset }
    } else {
        0
    }
}

/// Set a vnode's data offset + size.
pub fn set_data(id: u64, offset: u64, size: u64) {
    if let Some(slot) = find_slot(id) {
        let vn = vnode_at(slot);
        unsafe {
            (*vn).data_offset = offset;
            (*vn).size = size;
        }
    }
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

/// List root directory contents.
pub fn ls_root() {
    let rslot = match find_slot(ROOT_ID) {
        Some(s) => s,
        None => return,
    };
    let root = vnode_at(rslot);
    unsafe {
        serial::print_str("[ramfs] root directory (");
        serial::print_hex((*root).child_count as u64);
        serial::print_str(" entries):\n");
        for i in 0..(*root).child_count {
            let cid = (*root).child_ids[i];
            if let Some(cslot) = find_slot(cid) {
                let child = vnode_at(cslot);
                let ty = match (*child).vtype {
                    VnodeType::File => "FILE",
                    VnodeType::Dir => "DIR ",
                    _ => "???",
                };
                serial::print_str("  ");
                serial::print_str(ty);
                serial::print_str(" ");
                let name_ptr = core::ptr::addr_of!((*child).name) as *const u8;
                let name_len = (*child).name_len;
                let name_slice = core::slice::from_raw_parts(name_ptr, name_len);
                let name = core::str::from_utf8(name_slice).unwrap_or("?");
                serial::print_str(name);
                serial::print_str(" (");
                serial::print_hex((*child).size);
                serial::print_str(" bytes)\n");
            }
        }
    }
}
