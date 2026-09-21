//! File handle + file operations (open, read, close).
//! Uses raw pointer VFS API.

use crate::serial;
use super::{vfs, ramfs};

pub const MAX_HANDLES: usize = 16;

#[derive(Clone, Copy)]
pub struct FileHandle {
    pub vnode_id: u64,
    pub offset: u64,
    pub in_use: bool,
}

static mut HANDLES: [FileHandle; MAX_HANDLES] = [FileHandle {
    vnode_id: 0, offset: 0, in_use: false,
}; MAX_HANDLES];

fn handles_ptr() -> *mut FileHandle {
    unsafe { core::ptr::addr_of_mut!(HANDLES) as *mut FileHandle }
}

pub fn open(path: &str) -> i64 {
    let name = if path.starts_with('/') { &path[1..] } else { path };
    let vnode_id = vfs::lookup(vfs::ROOT_ID, name);
    if vnode_id == 0 {
        serial::print_str("[file] open(\"");
        serial::print_str(path);
        serial::print_str("\"): not found\n");
        return -1;
    }

    let hp = handles_ptr();
    for i in 0..MAX_HANDLES {
        unsafe {
            if !(*hp.add(i)).in_use {
                (*hp.add(i)).vnode_id = vnode_id;
                (*hp.add(i)).offset = 0;
                (*hp.add(i)).in_use = true;
                serial::print_str("[file] open(\"");
                serial::print_str(path);
                serial::print_str("\") -> fd=");
                serial::print_hex(i as u64);
                serial::print_str("\n");
                return i as i64;
            }
        }
    }
    serial::print_str("[file] open: too many open files\n");
    -1
}

pub fn read(fd: i64, buf: &mut [u8]) -> usize {
    let hp = handles_ptr();
    let idx = fd as usize;
    if idx >= MAX_HANDLES { return 0; }
    let vid = unsafe {
        if !(*hp.add(idx)).in_use { return 0; }
        (*hp.add(idx)).vnode_id
    };
    let n = ramfs::read_file(vid, buf);
    unsafe { (*hp.add(idx)).offset += n as u64; }
    n
}

pub fn close(fd: i64) {
    let hp = handles_ptr();
    let idx = fd as usize;
    if idx < MAX_HANDLES {
        unsafe {
            (*hp.add(idx)).in_use = false;
            (*hp.add(idx)).offset = 0;
        }
    }
}

pub fn smoke_test() {
    // Test 1: open + read /hello (has content, size=0x2D)
    serial::print_str("[file] test 1: open + read /hello\n");
    let fd = open("/hello");
    if fd < 0 {
        serial::print_str("[file] test 1: FAIL (open failed)\n");
        return;
    }
    let mut buf = [0u8; 128];
    let n = read(fd, &mut buf);
    serial::print_str("[file] test 1: read ");
    serial::print_hex(n as u64);
    serial::print_str(" bytes\n");
    close(fd);

    // Test 2: open nonexistent file
    serial::print_str("[file] test 2: open /nonexistent\n");
    let fd2 = open("/nonexistent");
    if fd2 < 0 {
        serial::print_str("[file] test 2: OK (returned -1)\n");
    } else {
        serial::print_str("[file] test 2: FAIL\n");
        close(fd2);
    }

    serial::print_str("[file] smoke test complete\n");
}
