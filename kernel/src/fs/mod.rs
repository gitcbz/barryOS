//! barryOS kernel — Stage 5 filesystem subsystem.
//!
//! Modules:
//! - `vfs`:       Virtual File System abstraction (Vnode, FileOps trait).
//! - `ramfs`:     In-memory RAM filesystem (tmpfs-style, no block device).
//! - `file`:      File handle + open/read/write/close/ls operations.

pub mod vfs;
pub mod ramfs;
pub mod file;

use core::sync::atomic::{AtomicBool, Ordering};
use crate::serial;

pub static INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Initialize the filesystem subsystem:
///   1. VFS root vnode.
///   2. RAM filesystem with test files.
///   3. File operations smoke test.
pub fn init() {
    serial::print_str("[fs] step 1: init VFS root\n");
    vfs::init();

    serial::print_str("[fs] step 2: create RAM filesystem\n");
    ramfs::init();

    serial::print_str("[fs] step 3: populate test files\n");
    ramfs::create_test_files();

    serial::print_str("[fs] step 4: list root directory\n");
    vfs::ls_root();

    serial::print_str("[fs] step 5: file read test\n");
    file::smoke_test();

    INITIALIZED.store(true, Ordering::Release);
    serial::print_str("[fs] filesystem subsystem online\n");
}
