//! barryOS kernel — Stage 2 entry point.
//!
//! Loaded at physical 0x00100000 by either:
//!   * the BIOS stage-2 bootloader (real -> protected -> long mode), or
//!   * the UEFI loader (`boot/uefi/efi_main.c`).
//!
//! Both arrive in 64-bit long mode with interrupts off, a valid stack at
//! 0x00200000, and RDI = pointer to a `BootInfo` struct (NULL on BIOS).
//!
//! Stage 2 goal: boot to `barryOS booted`, then initialize the memory
//! subsystem (frame allocator, paging, heap) and prove it works.

#![no_std]
#![no_main]

// Enable the `alloc` crate so Vec/Box work in no_std.
extern crate alloc;

mod vga;
mod serial;
mod sha256;
mod panic;
mod bootimg;
mod bootinfo;
mod mem;
mod interrupts;
mod proc;
mod fs;
mod dev;
mod wm;
mod apps;
mod compat;
mod vmware;
mod net;

use core::sync::atomic::Ordering;

/// Stack top provided by both boot paths (must match `STACK_TOP` in
/// `boot/bios/stage2.asm`).
///
/// The kernel image sits at 0x100000 and the stack grows down from here, so
/// the two share this range: it is what limits how large the kernel can get.
const STACK_TOP: usize = 0x0100_0000;   // 16 MiB

/// Naked entry. Placed in `.text.entry` so the linker script puts it first.
/// RDI holds the BootInfo pointer (NULL on BIOS).
#[unsafe(naked)]
#[link_section = ".text.entry"]
#[no_mangle]
pub unsafe extern "C" fn _start() -> ! {
    core::arch::naked_asm!(
        "cli",
        "mov rsp, {stk}",
        "xor rbp, rbp",
        "and rsp, 0xFFFFFFFFFFFFFFF0",
        "call {main}",
        "cli",
        "2:",
        "hlt",
        "jmp 2b",
        stk = const STACK_TOP,
        main = sym rust_main,
    );
}

/// Rust-level entry.  `boot_info` is RDI from the bootloader
/// (0 on BIOS, pointer to BootInfo on UEFI).
#[no_mangle]
pub unsafe extern "C" fn rust_main(boot_info: usize) -> ! {
    bootinfo::BOOT_INFO.store(boot_info as u64, Ordering::Relaxed);

    // Zero the .bss section.
    zero_bss();

    // Initialize output devices.
    serial::init();
    vga::init();

    // The BIOS stage2 also passes a BootInfo now (it carries the VBE
    // framebuffer it set up), so a non-null pointer no longer means UEFI.
    let is_uefi = mem::memmap::is_uefi_boot(boot_info);
    let boot_kind = if is_uefi { "UEFI" } else { "BIOS" };

    serial::print_str("\n");
    serial::print_str("========================================\n");
    serial::print_str("  barryOS - self-developed x86_64 kernel\n");
    serial::print_str("========================================\n");
    serial::print_str("[boot] path: ");
    serial::print_str(boot_kind);
    serial::print_str("\n");
    serial::print_str("[boot] kernel entry @ 0x");
    serial::print_hex(KERNEL_LOAD as u64);
    serial::print_str("\n");
    serial::print_str("barryOS booted\n");
    serial::print_str("[stage2] initializing memory subsystem...\n");

    // Initialize the memory subsystem (frame allocator + paging + heap).
    mem::init(boot_info);
    serial::print_str("[stage2] memory subsystem online.\n");

    // Display first, so every stage from here on can report itself on screen
    // rather than only on the serial port.  The backbuffer needs the frame
    // allocator, which is why this comes after `mem::init` and not before.
    serial::print_str("[boot] bringing up the display\n");
    dev::framebuffer::init(boot_info);
    dev::framebuffer::init_backbuffer();
    wm::boot::begin();

    // Each stage paints its label before doing the work, so a hang shows
    // exactly where it happened instead of leaving a blank screen.
    wm::boot::stage(1, "Starting interrupts...");
    interrupts::irq::init_pit();
    interrupts::init();

    wm::boot::stage(2, "Starting the scheduler...");
    proc::thread::set_frame_allocator(mem::frame_allocator());
    proc::init();

    // Device drivers before the filesystem: the filesystem mounts itself from
    // an ATA disk when one carries an installed system, so the disk has to be
    // probed first.
    wm::boot::stage(3, "Loading device drivers...");
    dev::init();

    wm::boot::stage(4, "Mounting the filesystem...");
    fs::init();

    wm::boot::stage(5, "Probing the network...");
    net::init();

    wm::boot::stage(6, "Loading compatibility layers...");
    compat::init();

    wm::boot::stage(7, "Checking for VMware...");
    vmware::init();

    wm::boot::stage(8, "Starting the window manager...");
    wm::init();

    wm::boot::stage(9, "Loading applications...");
    apps::init();

    wm::boot::stage(10, "Compositing the desktop...");
    wm::desktop::render();

    wm::boot::stage(11, "Running self-test...");
    apps::run_selftest();

    wm::boot::stage(12, "Starting up...");
    wm::boot::finish();

    // Diagnostics, serial only.
    proc::process::print_table();
    proc::scheduler::print_stats();
    fs::vfs::print_table();

    // The boot menu's answer decides which of two very different systems we
    // become: an installer, or a desktop.
    let install_mode =
        mem::memmap::boot_flags(boot_info) & mem::memmap::BOOT_FLAG_INSTALL != 0;

    if install_mode {
        serial::print_str("[boot] boot menu asked for an install\n");
        // No login: the machine being installed onto has no accounts yet, and
        // its disk is about to be overwritten regardless.
        apps::installer::open();
        wm::desktop::render();
        dev::framebuffer::flip();
        input_loop();
    }

    // Login.  Nothing on the desktop is reachable until someone authenticates,
    // and the uid it returns is what the whole session then runs as.
    serial::print_str("[boot] handing over to the login screen\n");
    let uid = wm::login::run();
    fs::perm::set_current_uid(uid);
    serial::print_str("[ok] session started\n");

    // Composite as the logged-in user and hand over to the input loop.
    wm::desktop::render();
    dev::framebuffer::flip();

    // VGA text-mode summary.  Only meaningful on the BIOS path: under UEFI the
    // display is the GOP framebuffer, 0xB8000 is not what anyone is looking at,
    // and on some firmware it is not mapped at all.
    if !is_uefi {
        vga::clear();
        vga::print_str("barryOS booted [Stage 11]\n");
        vga::print_str("self-developed x86_64 kernel\n");
        vga::print_str("[boot] path: ");
        vga::print_str(boot_kind);
        vga::print_str("\n");
        vga::print_str("[mem] frame alloc + paging + heap OK\n");
        vga::print_str("[irq] IDT + PIC + PIT OK\n");
        vga::print_str("[proc] PCB + scheduler + syscall OK\n");
        vga::print_str("[fs] VFS + RAMfs OK\n");
        vga::print_str("[dev] framebuffer + keyboard OK\n");
        vga::print_str("[wm] windows + font + dock OK\n");
        vga::print_str("[apps] terminal + files + sysinfo OK\n");
        vga::print_str("[compat] deb + rpm + appimage + pe + win32 OK\n");
        vga::print_str("[vmware] SVGA + backdoor + balloon OK\n");
    }

    input_loop();
}

/// Physical load address of the kernel (must match `linker.ld`).
const KERNEL_LOAD: usize = 0x0010_0000;

/// Zero the kernel's .bss (linker script exposes `__bss_start` / `__bss_end`).
unsafe fn zero_bss() {
    extern "C" {
        static mut __bss_start: u8;
        static mut __bss_end: u8;
    }
    let start = core::ptr::addr_of_mut!(__bss_start) as *mut u8;
    let end = core::ptr::addr_of_mut!(__bss_end) as *mut u8;
    let mut p = start;
    while p < end {
        p.write_volatile(0);
        p = p.add(1);
    }
}

/// Idle forever, repainting the pointer whenever it moves.
///
/// The old version was `cli; hlt`: a vCPU that could never be woken again,
/// which VMware reports as "the guest operating system has disabled the CPU".
/// Leaving interrupts enabled lets the 100 Hz PIT wake us each tick — and
/// that tick is what gives input a place to be turned into pixels, since
/// there is no compositor thread and the scheduler never preempts us.
unsafe fn input_loop() -> ! {
    serial::print_str("[wm] input loop running (keyboard + mouse)\n");
    dev::mouse::redraw();

    loop {
        let mut need_composite = false;

        // Network receive.  The adapter's interrupt source is masked, so the
        // ring is drained here; this runs on every timer tick.
        net::poll();

        // Work that would otherwise block the compositor — the installer
        // pushing megabytes through PIO — runs here, between frames, so the
        // progress bar it just painted is already on screen.
        if apps::tick() {
            need_composite = true;
        }

        // Keyboard goes to whichever window has focus.  Routing on the
        // focused id — rather than always to the terminal — is what lets the
        // file manager's name prompt and the editor both receive typing.
        let focused = wm::window::topmost_id();
        while let Some(key) = dev::keyboard::poll() {
            if apps::handle_key(focused, key) {
                need_composite = true;
            }
        }

        // Mouse: the shell turns clicks and drags into window-manager actions
        // and tells us whether anything on screen changed.
        let mouse_news = dev::mouse::take_dirty();
        if mouse_news && wm::shell::on_mouse() {
            need_composite = true;
        }

        if need_composite {
            // The pointer is part of the composed image, so it comes off
            // before the desktop is repainted and goes back on after.
            dev::mouse::hide();
            wm::desktop::render();
        }
        // Present once, with the pointer already in place.  Composing into the
        // backbuffer and flipping here is what stops a drag from flickering:
        // the display only ever shows finished frames.
        if mouse_news || need_composite {
            dev::mouse::redraw();
            dev::framebuffer::flip();
        }

        core::arch::asm!("hlt", options(nostack, nomem, preserves_flags));
    }
}
