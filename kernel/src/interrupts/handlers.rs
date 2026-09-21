//! Raw assembly interrupt entry stubs.
//!
//! Each stub pushes a fake error code (if the CPU didn't push one), then
//! saves all general-purpose registers in a fixed order, calls the Rust
//! handler with (vector, &Registers), and restores registers + iretq.
//!
//! We use `naked_asm` to generate one stub per vector via a macro.

use core::arch::naked_asm;

/// Saved register state passed to Rust handlers.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Registers {
    pub r15: u64, pub r14: u64, pub r13: u64, pub r12: u64,
    pub r11: u64, pub r10: u64, pub r9: u64,  pub r8: u64,
    pub rdi: u64, pub rsi: u64, pub rbp: u64, pub rdx: u64,
    pub rcx: u64, pub rbx: u64, pub rax: u64,
    pub int_no: u64,  // vector number
    pub err_code: u64, // error code (0 if CPU pushed none)
}

extern "C" {
    fn exception_handler(regs: &Registers);
    fn irq_handler(regs: &Registers);
}

/// Macro for exceptions where the CPU does NOT push an error code.
macro_rules! exception_stub_no_err {
    ($name:ident, $vector:expr) => {
        #[unsafe(naked)]
        #[no_mangle]
        pub unsafe extern "C" fn $name() -> () {
            naked_asm!(
                "push 0",           // fake error code
                "push {vec}",
                "push rax",
                "push rbx",
                "push rcx",
                "push rdx",
                "push rbp",
                "push rsi",
                "push rdi",
                "push r8",
                "push r9",
                "push r10",
                "push r11",
                "push r12",
                "push r13",
                "push r14",
                "push r15",
                "mov rdi, rsp",
                "call {handler}",
                "pop r15",
                "pop r14",
                "pop r13",
                "pop r12",
                "pop r11",
                "pop r10",
                "pop r9",
                "pop r8",
                "pop rdi",
                "pop rsi",
                "pop rbp",
                "pop rdx",
                "pop rcx",
                "pop rbx",
                "pop rax",
                "add rsp, 16",
                "iretq",
                vec = const $vector,
                handler = sym exception_handler,
            );
        }
    };
}

/// Macro for exceptions where the CPU DOES push an error code.
macro_rules! exception_stub_err {
    ($name:ident, $vector:expr) => {
        #[unsafe(naked)]
        #[no_mangle]
        pub unsafe extern "C" fn $name() -> () {
            // CPU has already pushed the error code — no fake push needed.
            naked_asm!(
                "push {vec}",
                "push rax",
                "push rbx",
                "push rcx",
                "push rdx",
                "push rbp",
                "push rsi",
                "push rdi",
                "push r8",
                "push r9",
                "push r10",
                "push r11",
                "push r12",
                "push r13",
                "push r14",
                "push r15",
                "mov rdi, rsp",
                "call {handler}",
                "pop r15",
                "pop r14",
                "pop r13",
                "pop r12",
                "pop r11",
                "pop r10",
                "pop r9",
                "pop r8",
                "pop rdi",
                "pop rsi",
                "pop rbp",
                "pop rdx",
                "pop rcx",
                "pop rbx",
                "pop rax",
                "add rsp, 16",
                "iretq",
                vec = const $vector,
                handler = sym exception_handler,
            );
        }
    };
}

/// Macro to generate a naked stub that pushes err_code if needed, saves regs,
/// and calls the right Rust function.
macro_rules! exception_stub {
    ($name:ident, $vector:expr, $has_error:expr) => { };
}

/// IRQ stubs (no error code pushed by CPU).
macro_rules! irq_stub {
    ($name:ident, $vector:expr) => {
        #[unsafe(naked)]
        #[no_mangle]
        pub unsafe extern "C" fn $name() -> () {
            naked_asm!(
                "push 0",          // fake error code
                "push {vec}",
                "push rax",
                "push rbx",
                "push rcx",
                "push rdx",
                "push rbp",
                "push rsi",
                "push rdi",
                "push r8",
                "push r9",
                "push r10",
                "push r11",
                "push r12",
                "push r13",
                "push r14",
                "push r15",
                "mov rdi, rsp",
                "call {handler}",
                "pop r15",
                "pop r14",
                "pop r13",
                "pop r12",
                "pop r11",
                "pop r10",
                "pop r9",
                "pop r8",
                "pop rdi",
                "pop rsi",
                "pop rbp",
                "pop rdx",
                "pop rcx",
                "pop rbx",
                "pop rax",
                "add rsp, 16",
                "iretq",
                vec = const $vector,
                handler = sym irq_handler,
            );
        }
    };
}

// ---------------------------------------------------------------------------
//  CPU exception stubs (vectors 0..31)
// ---------------------------------------------------------------------------
// Vector | # | has_error
exception_stub_no_err!(de_handler, 0);  // #DE divide error
exception_stub_no_err!(db_handler, 1);  // #DB debug
exception_stub_no_err!(nmi_handler, 2);  // NMI
exception_stub_no_err!(bp_handler, 3);  // #BP breakpoint
exception_stub_no_err!(of_handler, 4);  // #OF overflow
exception_stub_no_err!(br_handler, 5);  // #BR bound range
exception_stub_no_err!(ud_handler, 6);  // #UD invalid opcode
exception_stub_no_err!(nm_handler, 7);  // #NM device not available
exception_stub_err!(df_handler, 8);  // #DF double fault (has error code)
exception_stub_no_err!(xo_handler, 9);  // #MX segment overrun
exception_stub_err!(ts_handler, 10);  // #TS invalid TSS
exception_stub_err!(np_handler, 11);  // #NP segment not present
exception_stub_err!(ss_handler, 12);  // #SS stack fault
exception_stub_err!(gp_handler, 13);  // #GP general protection
exception_stub_err!(pf_handler, 14);  // #PF page fault
exception_stub_no_err!(xf_handler, 15);  // reserved
exception_stub_no_err!(mf_handler, 16);  // #MF x87 FPE
exception_stub_no_err!(ac_handler, 17);  // #AC alignment check
exception_stub_no_err!(mc_handler, 18);  // #MC machine check
exception_stub_no_err!(xm_handler, 19);  // #XM SIMD FPE
exception_stub_no_err!(ve_handler, 20);  // #VE virtualization
exception_stub_no_err!(reserved_handler, 21); // 21..31 reserved

// ---------------------------------------------------------------------------
//  IRQ stubs (vectors 32..47)
// ---------------------------------------------------------------------------
irq_stub!(irq0_timer,    32);
irq_stub!(irq1_keyboard,  33);
irq_stub!(irq2_cascade,  34);
irq_stub!(irq3_com2,     35);
irq_stub!(irq4_com1,     36);
irq_stub!(irq5_lpt2,     37);
irq_stub!(irq6_floppy,   38);
irq_stub!(irq7_lpt1,     39);
irq_stub!(irq8_rtc,      40);
irq_stub!(irq9_acpi,     41);
irq_stub!(irq10_pci,     42);
irq_stub!(irq11_pci,     43);
irq_stub!(irq12_mouse,   44);
irq_stub!(irq13_fpu,     45);
irq_stub!(irq14_ata,     46);
irq_stub!(irq15_ata,     47);

// Spurious handler (vectors 48..255).
#[unsafe(naked)]
#[no_mangle]
pub unsafe extern "C" fn spurious_handler() -> () {
    naked_asm!(
        "push 0",
        "push 0xff",
        "push rax",
        "mov rdi, rsp",
        "call {handler}",
        "pop rax",
        "add rsp, 16",
        "iretq",
        handler = sym irq_handler,
    );
}

/// Force the linker to keep all the handler stubs.  Without this reference,
/// `--gc-sections` removes them (they're only referenced via the IDT, which
/// the compiler can't see), and the IDT entries end up pointing at 0x0.
#[used]
pub static KEEP_HANDLERS: [unsafe extern "C" fn() -> (); 5] = [
    de_handler, df_handler, gp_handler, pf_handler, irq0_timer,
];
