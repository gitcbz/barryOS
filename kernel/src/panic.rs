//! Panic handler: dump message to serial + VGA, then halt.

use core::panic::PanicInfo;

#[panic_handler]
pub fn panic_handler(info: &PanicInfo) -> ! {
    // Serial first — most reliable for headless verification.
    crate::serial::print_str("\n!!! barryOS PANIC !!!\n");
    if let Some(loc) = info.location() {
        crate::serial::print_str("  at ");
        crate::serial::print_str(loc.file());
        crate::serial::print_str(":");
        crate::serial::print_hex(loc.line() as u64);
        crate::serial::print_str("\n");
    }
    if let Some(m) = info.message().as_str() {
        crate::serial::print_str("  msg: ");
        crate::serial::print_str(m);
        crate::serial::print_str("\n");
    }
    crate::serial::print_str("halting.\n");

    // VGA mirror.
    crate::vga::print_str("\n!!! barryOS PANIC !!!\n");
    if let Some(m) = info.message().as_str() {
        crate::vga::print_str(m);
        crate::vga::print_str("\n");
    }

    loop {
        unsafe {
            core::arch::asm!("cli; hlt", options(nostack, nomem, preserves_flags));
        }
    }
}
