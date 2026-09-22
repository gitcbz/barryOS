//! In-memory RAM filesystem (tmpfs-style, no block device).
//!
//! Storage is one flat byte pool with a small free list.  Two things the
//! previous version got wrong:
//!
//!   * `alloc_data` handed out offset 0 for the very first file, but
//!     `read_file` treated offset 0 as "no data" — so the first file created
//!     was permanently unreadable.  The bump pointer now starts at 1.
//!
//!   * Rewriting a file reused its old offset with no capacity check, so
//!     growing a file ran straight over whatever was allocated after it.
//!     Each vnode now records how many bytes it reserved, and a write that
//!     does not fit reallocates.

use crate::serial;
use super::vfs;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// Data pool.  Sized so the editor can hold a few real files; this is .bss, so
/// it costs RAM but not kernel image size.
pub const DATA_POOL_SIZE: usize = 256 * 1024;

static mut DATA_POOL: [u8; DATA_POOL_SIZE] = [0; DATA_POOL_SIZE];

/// Bump pointer.  Starts at 1 so offset 0 can keep meaning "no allocation".
static NEXT_OFFSET: AtomicU64 = AtomicU64::new(1);

/// Freed blocks, first-fit.  No coalescing: fragments are tiny next to the
/// pool and a real allocator is not what this stage is about.
const FREE_MAX: usize = 32;
#[derive(Clone, Copy)]
struct FreeBlock {
    off: u64,
    size: u64,
}
static mut FREE: [FreeBlock; FREE_MAX] = [FreeBlock { off: 0, size: 0 }; FREE_MAX];
static FREE_COUNT: AtomicUsize = AtomicUsize::new(0);

fn data_pool_ptr() -> *mut u8 {
    unsafe { core::ptr::addr_of_mut!(DATA_POOL) as *mut u8 }
}

pub fn bytes_used() -> u64 {
    NEXT_OFFSET.load(Ordering::Relaxed)
}

pub fn bytes_free() -> usize {
    let mut total = DATA_POOL_SIZE as u64 - NEXT_OFFSET.load(Ordering::Relaxed);
    let base = unsafe { core::ptr::addr_of!(FREE) as *const FreeBlock };
    for i in 0..FREE_COUNT.load(Ordering::Relaxed) {
        total += unsafe { (*base.add(i)).size };
    }
    total as usize
}

/// Reserve `n` bytes.  Returns the offset, or 0 on failure.
fn alloc_data(n: u64) -> u64 {
    if n == 0 {
        return 0;
    }
    // First fit over the free list.
    let count = FREE_COUNT.load(Ordering::Relaxed);
    let base = unsafe { core::ptr::addr_of_mut!(FREE) as *mut FreeBlock };
    for i in 0..count {
        let b = unsafe { &mut *base.add(i) };
        if b.size >= n {
            let off = b.off;
            if b.size > n {
                b.off += n;
                b.size -= n;
            } else {
                // Hand back the whole block: drop it by swapping the last in.
                unsafe {
                    let last = count - 1;
                    *base.add(i) = *base.add(last);
                }
                FREE_COUNT.store(count - 1, Ordering::Relaxed);
            }
            return off;
        }
    }
    // Otherwise bump.
    let off = NEXT_OFFSET.fetch_add(n, Ordering::SeqCst);
    if off + n > DATA_POOL_SIZE as u64 {
        serial::print_str("[ramfs] data pool exhausted\n");
        return 0;
    }
    off
}

/// Release a block back to the free list.
fn free_data(off: u64, size: u64) {
    if off == 0 || size == 0 {
        return;
    }
    let count = FREE_COUNT.load(Ordering::Relaxed);
    if count >= FREE_MAX {
        return;                     // leaked; the pool is big enough not to care
    }
    let base = unsafe { core::ptr::addr_of_mut!(FREE) as *mut FreeBlock };
    unsafe {
        *base.add(count) = FreeBlock { off, size };
    }
    FREE_COUNT.store(count + 1, Ordering::Relaxed);
}

pub fn init() {
    serial::print_str("[ramfs] RAM filesystem initialized (");
    serial::print_hex(DATA_POOL_SIZE as u64);
    serial::print_str(" bytes data pool, free-list allocator)\n");
}

/// Write content to a file vnode, growing the allocation when needed.
pub fn write_file(vnode_id: u64, data: &[u8]) -> usize {
    if vfs::find_slot(vnode_id).is_none() {
        return 0;
    }
    let len = data.len().min(vfs::MAX_FILE_SIZE) as u64;
    let old_off = vfs::get_data_offset(vnode_id);
    let old_cap = vfs::get_capacity(vnode_id);

    if len == 0 {
        if old_cap > 0 {
            free_data(old_off, old_cap);
        }
        vfs::set_data_full(vnode_id, 0, 0, 0);
        return 0;
    }

    let offset = if old_cap >= len {
        // Fits in what we already reserved; reuse it in place.
        vfs::set_data(vnode_id, old_off, len);
        old_off
    } else {
        let off = alloc_data(len);
        if off == 0 {
            return 0;
        }
        if old_cap > 0 {
            free_data(old_off, old_cap);
        }
        vfs::set_data_full(vnode_id, off, len, len);
        off
    };

    unsafe {
        let dst = data_pool_ptr().add(offset as usize);
        for (i, &b) in data.iter().enumerate().take(len as usize) {
            core::ptr::write_volatile(dst.add(i), b);
        }
    }
    len as usize
}

/// Read content from a file vnode.  Returns bytes copied.
pub fn read_file(vnode_id: u64, buf: &mut [u8]) -> usize {
    let size = vfs::get_size(vnode_id);
    let offset = vfs::get_data_offset(vnode_id);
    if size == 0 || offset == 0 {
        return 0;
    }
    let len = (size as usize).min(buf.len());
    unsafe {
        let src = data_pool_ptr().add(offset as usize);
        for (i, slot) in buf.iter_mut().enumerate().take(len) {
            *slot = core::ptr::read_volatile(src.add(i));
        }
    }
    len
}

/// Read part of a file starting at `offset`.  Lets `cat` stream a file out in
/// chunks instead of needing a buffer as big as the file.
pub fn read_file_at(vnode_id: u64, offset: u64, buf: &mut [u8]) -> usize {
    let size = vfs::get_size(vnode_id);
    let base = vfs::get_data_offset(vnode_id);
    if base == 0 || offset >= size {
        return 0;
    }
    let avail = ((size - offset) as usize).min(buf.len());
    unsafe {
        let src = data_pool_ptr().add((base + offset) as usize);
        for (i, slot) in buf.iter_mut().enumerate().take(avail) {
            *slot = core::ptr::read_volatile(src.add(i));
        }
    }
    avail
}

/// Create a file in the root directory.
pub fn create_file(name: &str) -> u64 {
    vfs::create_file(vfs::ROOT_ID, name)
}

/// Create a file in a given directory.
pub fn create_file_in(parent: u64, name: &str) -> u64 {
    vfs::create_file(parent, name)
}

pub fn create_dir_in(parent: u64, name: &str) -> u64 {
    vfs::create_dir(parent, name)
}

pub fn create_file_with_content(name: &str, content: &[u8]) -> u64 {
    let id = create_file(name);
    if id == 0 {
        return 0;
    }
    let written = write_file(id, content);
    serial::print_str("[ramfs] created \"");
    serial::print_str(name);
    serial::print_str("\" id=");
    serial::print_hex(id);
    serial::print_str(" size=");
    serial::print_hex(written as u64);
    serial::print_str("\n");
    id
}

/// Delete a file (or empty directory), releasing its data pool bytes.
pub fn unlink(parent: u64, name: &str) -> bool {
    let id = vfs::lookup(parent, name);
    if id == 0 {
        return false;
    }
    let cap = vfs::get_capacity(id);
    let off = vfs::get_data_offset(id);
    if !vfs::unlink(parent, name) {
        return false;
    }
    if cap > 0 {
        free_data(off, cap);
    }
    serial::print_str("[ramfs] removed \"");
    serial::print_str(name);
    serial::print_str("\"\n");
    true
}

pub fn rename(parent: u64, old: &str, new: &str) -> bool {
    vfs::rename(parent, old, new)
}

/// Initial contents.  A small tree so the file manager has something to
/// navigate, plus an executable script and a read-only file so the permission
/// model has something to actually demonstrate.
pub fn create_test_files() {
    serial::print_str("[ramfs] creating sample files...\n");
    create_file_with_content("motd", b"barryOS v0.12\nself-developed x86_64 kernel\n");
    create_file_with_content("hello", b"echo hello world from barryOS\n");
    create_file_with_content("version", b"barryOS 0.12.0\nBooted on real UEFI/BIOS hardware\n");
    create_file_with_content("hostname", b"barryos\n");

    let docs = vfs::create_dir(vfs::ROOT_ID, "docs");
    if docs != 0 {
        create_file_with_content_in(docs, "readme",
            b"barryOS notes\n\nUse the file manager to browse,\nand the editor to change a file.\n");
        create_file_with_content_in(docs, "todo",
            b"- more drivers\n- network stack\n- ring 3\n");
    }
    let home = vfs::create_dir(vfs::ROOT_ID, "home");
    if home != 0 {
        create_file_with_content_in(home, "notes", b"hello\n");
    }

    // An executable: the shell interprets it, which is what "running a file"
    // means until there is a user mode to load real binaries into.
    let bin = vfs::create_dir(vfs::ROOT_ID, "bin");
    if bin != 0 {
        let id = create_file_with_content_in(bin, "hello",
            b"#!/bin/barryOS\necho hello from an executable script\npwd\nls\n");
        crate::fs::vfs::set_mode(id, crate::fs::perm::MODE_EXEC);
    }

    // Read-only, to show `chmod` and a denied write doing something.
    let ro = create_file_with_content("readonly",
        b"This file is read-only.\nTry: edit /readonly   (should be refused)\n     chmod 644 /readonly   (root only)\n");
    crate::fs::vfs::set_mode(ro, crate::fs::perm::MODE_RONLY);

    serial::print_str("[ramfs] sample tree ready\n");
}

fn create_file_with_content_in(parent: u64, name: &str, content: &[u8]) -> u64 {
    let id = create_file_in(parent, name);
    if id == 0 {
        return 0;
    }
    write_file(id, content);
    id
}
