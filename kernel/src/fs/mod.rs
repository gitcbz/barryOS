//! barryOS kernel — filesystem subsystem.
//!
//! Modules:
//! - `vfs`:    Virtual File System (vnodes, directories, path resolution).
//! - `ramfs`:  In-memory storage (byte pool with a free list).
//! - `file`:   File handle + open/read/close.
//! - `perm`:   Users, groups, permissions and password checking.
//! - `diskfs`: barryFS — moving whole filesystems to and from a hard disk.
//!
//! The working filesystem is always the RAM one.  When an installed system is
//! found on a disk it is loaded over the top at boot, and `sync` writes it
//! back; that is what makes files survive a reboot.

pub mod vfs;
pub mod ramfs;
pub mod file;
pub mod perm;
pub mod diskfs;

use core::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use crate::dev::ata;
use crate::serial;

pub static INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Which disk the filesystem was mounted from, or -1 for the built-in tree.
static MOUNT_DRIVE: AtomicI32 = AtomicI32::new(-1);

pub fn mount_drive() -> Option<usize> {
    let d = MOUNT_DRIVE.load(Ordering::Relaxed);
    if d < 0 { None } else { Some(d as usize) }
}

/// Write the in-memory filesystem back to the disk it came from.
pub fn sync() -> bool {
    match mount_drive() {
        Some(d) => diskfs::save(d),
        None => false,
    }
}

/// Initialize the filesystem subsystem:
///   1. VFS root, plus `/etc/passwd` and `/etc/shadow` for a fresh system.
///   2. The built-in sample tree.
///   3. If a disk carries an installed system, mount that instead — it wins,
///      because that is what the user asked to boot.
///   4. Load the account list from whichever tree we ended up with.
pub fn init() {
    serial::print_str("[fs] step 1: init VFS root\n");
    vfs::init();

    serial::print_str("[fs] step 2: install account files\n");
    perm::install_defaults();

    serial::print_str("[fs] step 3: create RAM filesystem\n");
    ramfs::init();

    serial::print_str("[fs] step 4: populate sample files\n");
    ramfs::create_test_files();

    serial::print_str("[fs] step 5: look for an installed system\n");
    mount_from_disk();

    serial::print_str("[fs] step 6: load accounts\n");
    perm::init();

    serial::print_str("[fs] step 7: list root directory\n");
    vfs::ls_root();

    serial::print_str("[fs] step 8: file read test\n");
    file::smoke_test();

    INITIALIZED.store(true, Ordering::Release);
    serial::print_str("[fs] filesystem subsystem online\n");
}

/// Look for a barryFS on any attached disk and mount the first one found.
fn mount_from_disk() -> Option<usize> {
    for i in 0..ata::count() {
        if !diskfs::is_present(i) {
            continue;
        }
        // Loading replaces the whole in-memory tree, including the sample
        // files and the account files made a moment ago — which is correct:
        // the installed system's own /etc is what should be in effect.
        if diskfs::load(i) {
            MOUNT_DRIVE.store(i as i32, Ordering::Release);
            serial::print_str("[fs] mounted the installed system from drive ");
            serial::print_hex(i as u64);
            serial::print_str("\n");
            return Some(i);
        }
    }
    serial::print_str("[fs] no installed system found; using the built-in tree\n");
    None
}
