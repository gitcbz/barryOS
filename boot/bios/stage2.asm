; =============================================================================
;  barryOS — BIOS Stage 2 Bootloader
; -----------------------------------------------------------------------------
;  Loaded by MBR at 0000:7E00h (16-bit real mode).  Final job is to land in
;  64-bit long mode and jump to the kernel flat binary at 0x00100000.
;
;  Flow:
;    [16-bit]  load kernel.bin (LBA 32..) to 0x00010000
;    [16-bit]  enable A20 (fast 0x92, then kbd fallback)
;    [16-bit]  load 32-bit GDT, enter 32-bit protected mode
;    [32-bit]  copy kernel 0x10000 → 0x100000 (rep movsd)
;    [32-bit]  build identity page tables (first 4 MiB)
;    [32-bit]  enable PAE, set CR3, set EFER.LME, enable paging → long mode
;    [32-bit]  load 64-bit GDT, far-jmp to 64-bit code segment
;    [64-bit]  set segments + stack, RDI=0 (BIOS: no BootInfo), jmp 0x100000
;
;  Self-developed; no Linux/GRUB code.
; =============================================================================
[bits 16]
[org 0x7E00]

KERNEL_DISK_LBA    equ 32            ; kernel.bin starts at LBA 32
KERNEL_SECTORS     equ 128           ; 64 KiB max kernel size for stage 1
KERNEL_BUF_SEG     equ 0x1000       ; buffer segment  → linear 0x10000
KERNEL_BUF_OFF     equ 0x0000
KERNEL_BUF_LINEAR  equ 0x00010000
KERNEL_FINAL       equ 0x00100000   ; kernel final physical address
STACK_TOP          equ 0x00200000   ; 1 MiB above kernel, grows down

; page table addresses
PML4_ADDR          equ 0x00070000
PDPT_ADDR          equ 0x00071000
PD_ADDR            equ 0x00072000

stage2_start:
    ; --- save boot drive ---
    mov [boot_drive], dl

    ; --- announce (teletype) ---
    mov si, msg_loading
    call print16

    ; --- load kernel.bin to 0x1000:0000 (= linear 0x10000) ---
    mov si, dap_kernel
    mov ah, 0x42
    mov dl, [boot_drive]
    int 0x13
    jc disk_err

    ; --- enable A20 (fast method via port 0x92) ---
    in  al, 0x92
    or  al, 0x02                      ; set A20 enable bit
    and al, 0xFE                      ; do NOT toggle bit 0 (system reset!)
    out 0x92, al

    ; --- verify A20 (optional, but cheap): if wraparound persists, kbd fallback ---
    call check_a20
    test al, al
    jnz .a20_ok
    call enable_a20_kbd               ; keyboard-controller fallback
.a20_ok:

    ; --- load 32-bit GDT and enter protected mode ---
    cli
    lgdt [gdt32_desc]

    mov eax, cr0
    or  eax, 1                        ; CR0.PE = 1
    mov cr0, eax

    ; far jump flushes prefetch + loads CS=0x08 (32-bit code)
    jmp 0x08:pm_entry

disk_err:
    mov si, msg_disk_err
    call print16
.hang: hlt
    jmp .hang

; -----------------------------------------------------------------------------
;  16-bit helpers
; -----------------------------------------------------------------------------
print16:
    lodsb
    test al, al
    jz .done
    mov ah, 0x0E
    mov bx, 0x0007
    int 0x10
    jmp print16
.done: ret

; check A20 enabled: returns AL=1 if enabled (no wraparound)
check_a20:
    push ds
    push es
    xor ax, ax
    mov es, ax
    mov ax, 0xFFFF
    mov ds, ax
    mov di, 0x0500
    mov si, 0x0510
    mov al, [es:di]
    mov cl, [ds:si]
    cmp al, cl
    mov al, 0
    jne .a20_on
    inc byte [es:di]
    mov cl, [ds:si]
    cmp al, [es:di]
    mov al, 1
    jne .a20_on
    xor al, al                        ; wraparound → A20 off
.a20_on:
    pop es
    pop ds
    ret

enable_a20_kbd:
    cli
    call .wait_kbd
    mov al, 0xAD                       ; disable keyboard
    out 0x64, al
    call .wait_kbd
    mov al, 0xD0                       ; read output port
    out 0x64, al
    call .wait_data
    in  al, 0x60
    push ax
    call .wait_kbd
    mov al, 0xD1                       ; write output port
    out 0x64, al
    call .wait_kbd
    pop ax
    or  al, 0x02                       ; set A20 bit
    out 0x60, al
    call .wait_kbd
    mov al, 0xAE                       ; re-enable keyboard
    out 0x64, al
    call .wait_kbd
    sti
    ret
.wait_kbd:
    in  al, 0x64
    test al, 0x02
    jnz .wait_kbd
    ret
.wait_data:
    in  al, 0x64
    test al, 0x01
    jz .wait_data
    ret

; -----------------------------------------------------------------------------
;  Data
; -----------------------------------------------------------------------------
align 4
dap_kernel:
    db 0x10, 0
    dw KERNEL_SECTORS
    dw KERNEL_BUF_OFF
    dw KERNEL_BUF_SEG
    dq KERNEL_DISK_LBA

boot_drive: db 0

msg_loading:    db "[barryOS] stage2: loading kernel...", 13, 10, 0
msg_disk_err:   db "[barryOS] disk read error", 0

; =============================================================================
;  32-bit GDT
; =============================================================================
align 8
gdt32_start:
    dq 0                              ; null
gdt32_code:                           ; 0x08 - 32-bit code, flat, 4 GiB
    dw 0xFFFF, 0x0000
    db 0x00, 0x9A, 0xCF, 0x00
gdt32_data:                           ; 0x10 - 32-bit data, flat, 4 GiB
    dw 0xFFFF, 0x0000
    db 0x00, 0x92, 0xCF, 0x00
gdt32_end:
gdt32_desc:
    dw gdt32_end - gdt32_start - 1
    dd gdt32_start

; =============================================================================
;  64-bit GDT
; =============================================================================
align 8
gdt64_start:
    dq 0                              ; null
gdt64_code:                           ; 0x08 - 64-bit code (L=1)
    dw 0xFFFF, 0x0000
    db 0x00, 0x9A, 0xAF, 0x00
gdt64_data:                           ; 0x10 - 64-bit data
    dw 0xFFFF, 0x0000
    db 0x00, 0x92, 0x00, 0x00
gdt64_end:
gdt64_desc:
    dw gdt64_end - gdt64_start - 1
    dq gdt64_start

; =============================================================================
;  32-bit protected mode entry
; =============================================================================
[bits 32]
pm_entry:
    mov ax, 0x10
    mov ds, ax
    mov es, ax
    mov fs, ax
    mov gs, ax
    mov ss, ax
    mov esp, 0x90000                   ; temp stack

    ; --- copy kernel 0x10000 → 0x100000 (KERNEL_SECTORS*512 bytes) ---
    mov esi, KERNEL_BUF_LINEAR
    mov edi, KERNEL_FINAL
    mov ecx, (KERNEL_SECTORS * 512) / 4
    rep movsd

    ; --- zero page tables (3 pages) ---
    mov edi, PML4_ADDR
    xor eax, eax
    mov ecx, (4096 * 3) / 4
    rep stosd

    ; --- PML4[0] -> PDPT ---
    mov dword [PML4_ADDR], PDPT_ADDR | 0x03
    ; --- PDPT[0] -> PD ---
    mov dword [PDPT_ADDR], PD_ADDR | 0x03
    ; --- PD[0] = 2 MiB page @ 0x000000, PD[1] = 2 MiB page @ 0x200000 ---
    mov dword [PD_ADDR + 0],  0x000000 | 0x83
    mov dword [PD_ADDR + 8],  0x200000 | 0x83

    ; --- enable PAE ---
    mov eax, cr4
    or  eax, 0x20                      ; CR4.PAE = 1
    mov cr4, eax

    ; --- CR3 = PML4 ---
    mov eax, PML4_ADDR
    mov cr3, eax

    ; --- set EFER.LME (long mode enable) ---
    mov ecx, 0xC0000080
    rdmsr
    or  eax, 0x100                     ; EFER.LME = 1
    wrmsr

    ; --- enable paging (CR0.PG=1) → long mode activates ---
    mov eax, cr0
    or  eax, 0x80000000
    mov cr0, eax

    ; --- load 64-bit GDT, far-jmp to 64-bit code segment ---
    lgdt [gdt64_desc]
    jmp 0x08:lm_entry

; =============================================================================
;  64-bit long mode entry
; =============================================================================
[bits 64]
lm_entry:
    mov ax, 0x10
    mov ds, ax
    mov es, ax
    mov fs, ax
    mov gs, ax
    mov ss, ax

    mov rsp, STACK_TOP                 ; kernel stack

    xor rdi, rdi                       ; BootInfo* = NULL (BIOS path)
    mov rsi, rdi                       ; unused

    jmp KERNEL_FINAL                   ; enter kernel _start

; pad to 16 KiB (will occupy LBA 1..31)
times 31*512 - ($-$$) db 0
