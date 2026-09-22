//! Build-time embedded blobs: the boot chain, and the test program.
//!
//! Generated into `build/bootimg.rs` by `scripts/gen-bootimg.py`.  The boot
//! chain is carried inside the kernel so the installer can write it to a
//! target disk whatever medium it booted from; a CD would need an ATAPI driver
//! we do not have.  The test executable is carried the same way, so there is
//! something to run through the PE loader without needing a disk first.
//!
//! rustc records included files in its dep-info, so cargo rebuilds when the
//! generated file changes.

include!(concat!(env!("CARGO_MANIFEST_DIR"), "/../build/bootimg.rs"));
