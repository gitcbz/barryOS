//! In-memory RAM filesystem (tmpfs-style).
//! Uses raw pointer VFS API to avoid reference UB.

use crate::serial;
use super::vfs;
use core::sync::atomic::AtomicU64;

pub const DATA_POOL_SIZE: usize = 8 * 1024;
static mut DATA_POOL: [u8; DATA_POOL_SIZE] = [0; DATA_POOL_SIZE];
static NEXT_OFFSET: AtomicU64 = AtomicU64::new(0);

fn data_pool_ptr() -> *mut u8 {
    unsafe { core::ptr::addr_of_mut!(DATA_POOL) as *mut u8 }
}

pub fn alloc_data(n: u64) -> u64 {
    let off = NEXT_OFFSET.fetch_add(n, core::sync::atomic::Ordering::SeqCst);
    if off + n > DATA_POOL_SIZE as u64 {
        serial::print_str("[ramfs] data pool full\n");
        return 0;
    }
    off
}

pub fn init() {
    serial::print_str("[ramfs] RAM filesystem initialized (");
    serial::print_hex(DATA_POOL_SIZE as u64);
    serial::print_str(" bytes data pool)\n");
}

/// Write content to a file vnode.
pub fn write_file(vnode_id: u64, data: &[u8]) -> usize {
    let slot = match vfs::find_slot(vnode_id) {
        Some(s) => s,
        None => return 0,
    };

    let len = data.len().min(vfs::MAX_FILE_SIZE);

    let offset = if vfs::get_data_offset(vnode_id) == 0 && vfs::get_size(vnode_id) == 0 {
        let off = alloc_data(len as u64);
        if off == 0 { return 0; }
        vfs::set_data(vnode_id, off, len as u64);
        off
    } else {
        let off = vfs::get_data_offset(vnode_id);
        vfs::set_data(vnode_id, off, len as u64);
        off
    };

    unsafe {
        let dst = data_pool_ptr().add(offset as usize);
        for i in 0..len {
            core::ptr::write_volatile(dst.add(i), data[i]);
        }
    }
    len
}

/// Read content from a file vnode.
pub fn read_file(vnode_id: u64, buf: &mut [u8]) -> usize {
    let size = vfs::get_size(vnode_id);
    let offset = vfs::get_data_offset(vnode_id);
    if size == 0 || offset == 0 { return 0; }

    let len = (size as usize).min(buf.len());
    unsafe {
        let src = data_pool_ptr().add(offset as usize);
        for i in 0..len {
            buf[i] = core::ptr::read_volatile(src.add(i));
        }
    }
    len
}

pub fn create_file(name: &str) -> u64 {
    vfs::create_file(vfs::ROOT_ID, name)
}

pub fn create_file_with_content(name: &str, content: &[u8]) -> u64 {
    let id = create_file(name);
    if id == 0 { return 0; }
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

pub fn create_test_files() {
    serial::print_str("[ramfs] creating motd...\n");
    create_file_with_content("motd", b"barryOS v0.5\nself-developed x86_64 kernel\nStage 5: VFS + RAM filesystem\n");

    serial::print_str("[ramfs] creating hello...\n");
    create_file_with_content("hello", b"#!/bin/barryOS\necho hello world from barryOS\n");

    serial::print_str("[ramfs] creating version...\n");
    create_file_with_content("version", b"barryOS 0.5.0\nStage 5 VFS+RAMfs\nBooted OK\n");

    serial::print_str("[ramfs] creating hostname...\n");
    create_file_with_content("hostname", b"barryos\n");
}
