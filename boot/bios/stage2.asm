; =============================================================================
;  barryOS — BIOS Stage 2 Bootloader
; -----------------------------------------------------------------------------
;  Loaded by MBR at 0000:7E00h (16-bit real mode).  Final job is to land in
;  64-bit long mode and jump to the kernel flat binary at 0x00100000.
;
;  Flow:
;    [16-bit]  load kernel.bin (LBA 40..) to 0x00010000
;    [16-bit]  VBE: set a 32-bpp linear framebuffer, fill a BootInfo block
;    [16-bit]  enable A20 (fast 0x92, then kbd fallback)
;    [16-bit]  load 32-bit GDT, enter 32-bit protected mode
;    [32-bit]  copy kernel 0x10000 → 0x100000 (rep movsd)
;    [32-bit]  build identity page tables (first 4 MiB)
;    [32-bit]  enable PAE, set CR3, set EFER.LME, enable paging → long mode
;    [32-bit]  load 64-bit GDT, far-jmp to 64-bit code segment
;    [64-bit]  set segments + stack, RDI=&BootInfo, jmp 0x100000
;
;  Self-developed; no Linux/GRUB code.
; =============================================================================
[bits 16]
[org 0x7E00]

; stage2 occupies LBA 1..40 inclusive (40 sectors), so the kernel starts at 41.
KERNEL_DISK_LBA    equ 41            ; kernel.bin starts at LBA 41
KERNEL_FINAL       equ 0x00100000   ; kernel final physical address

; Streaming loader.  The kernel is read through a 32 KiB staging buffer that
; lives in conventional memory (the BIOS can only write below 1 MiB), and each
; batch is then copied to KERNEL_FINAL in 32-bit protected mode.  That is what
; lets the kernel be far larger than conventional memory -- the previous
; version had to fit the whole image below 1 MiB, which capped it at ~450 KiB.
STAGING_SEG        equ 0x1000       ; staging buffer segment → linear 0x10000
STAGING_OFF        equ 0x0000
STAGING_LINEAR     equ 0x00010000
BATCH_SECTORS      equ 64           ; 32 KiB per BIOS call
BATCH_BYTES        equ BATCH_SECTORS * 512

; The kernel image starts here and the stack grows down from here, so the two
; together must fit in between.  16 MiB leaves room for an 8 MiB kernel with
; 8 MiB of stack.
STACK_TOP          equ 0x01000000

; Page table addresses.  Above the staging buffer, below the video window.
PML4_ADDR          equ 0x00080000
PDPT_ADDR          equ 0x00081000
PD_ADDR            equ 0x00082000

; BootInfo handed to the kernel in RDI (matches kernel/src/bootinfo.rs).
BOOTINFO_ADDR      equ 0x00006000
BOOTINFO_MAGIC     equ 0x0000534F52524142   ; 'BARROS' + two NULs

stage2_start:
    ; Two bytes of jump over the build-time header, two of padding, then the
    ; kernel size.  The Makefile patches offset 4 with the real byte count,
    ; which frees the loader from a compile-time sector budget entirely.
    jmp short real_start
    times 2 db 0
kernel_size_bytes: dd 0

real_start:
    ; --- save boot drive ---
    mov [boot_drive], dl

    ; --- give ourselves a real stack (the MBR's leaves only ~2 KiB) ---
    cli
    xor ax, ax
    mov ss, ax
    mov sp, 0x5FFE                     ; just under BOOTINFO_ADDR
    sti

    ; --- announce (teletype) ---
    mov si, msg_loading
    call print16

    ; --- install or just start? ---
    ; Shown before VBE takes the display: BIOS teletype output does not work
    ; once a graphics mode is set, and the splash needs a real framebuffer.
    call boot_menu

    ; --- VBE: set up the framebuffer before anything else touches the screen ---
    call vbe_setup

    ; --- stream the kernel to KERNEL_FINAL -------------------------------
    ; Each pass reads the next batch into the staging buffer — the only place
    ; the BIOS can be asked to write, since its buffer address is a 16-bit
    ; segment:offset — and then copies it to its final home in 32-bit
    ; protected mode.  EBX carries the byte offset into the image.
    xor ebx, ebx
.stream:
    mov eax, [kernel_size_bytes]
    test eax, eax
    jz no_size                          ; header not patched: refuse to guess
    cmp ebx, eax
    jae .done

    sub eax, ebx                        ; bytes still to read
    add eax, 511
    shr eax, 9                          ; → sectors still to read
    mov ecx, BATCH_SECTORS
    cmp eax, ecx
    jae .counted
    mov ecx, eax                        ; short final batch
.counted:
    mov [batch_sectors], cx

    mov eax, ebx
    shr eax, 9
    add eax, KERNEL_DISK_LBA            ; LBA of this batch

    mov word [dap_kernel + 2], cx
    mov word [dap_kernel + 4], STAGING_OFF
    mov word [dap_kernel + 6], STAGING_SEG
    mov dword [dap_kernel + 8], eax
    mov dword [dap_kernel + 12], 0
    mov si, dap_kernel
    mov ah, 0x42
    mov dl, [boot_drive]
    int 0x13
    jc disk_err

    movzx eax, word [batch_sectors]
    shl eax, 9
    mov [batch_bytes], eax

    call copy_batch                     ; PM: staging → KERNEL_FINAL + ebx

    add ebx, [batch_bytes]
    jmp .stream
.done:

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

; The build-time header still says zero bytes: something rebuilt stage2.bin
; without going through the Makefile's patch step.  Refusing is better than
; loading a guessed number of sectors.
no_size:
    mov si, msg_no_size
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

; -----------------------------------------------------------------------------
;  boot_menu — ask whether to install or to start.
;
;  Runs in text mode, before VBE takes the display over: BIOS teletype output
;  stops working once a graphics mode is set, and the splash needs the real
;  framebuffer.
;
;  Times out to "just start", so an unattended boot (a test harness, say) does
;  not sit at a menu forever.  The BIOS keeps an 18.2 Hz tick counter at
;  0040:006C, which is the only clock available this early.
; -----------------------------------------------------------------------------
MENU_TIMEOUT equ 91                     ; ticks, ≈5 seconds

boot_menu:
    mov si, msg_menu
    call print16

    push es
    xor ax, ax
    mov es, ax
    mov eax, [es:0x6C]
    mov [menu_ticks], eax
    pop es

.wait:
    mov ah, 0x01
    int 0x16                            ; peek: is a key waiting?
    jnz .key

    push es
    xor ax, ax
    mov es, ax
    mov eax, [es:0x6C]
    sub eax, [menu_ticks]
    pop es
    cmp eax, MENU_TIMEOUT
    jb .wait
    mov si, msg_timeout
    call print16
    ret                                 ; default: just start

.key:
    mov ah, 0x00
    int 0x16                            ; consume the key
    cmp al, '1'
    je .install
    cmp al, '2'
    je .start
    cmp al, 13                          ; Enter
    je .start
    jmp .wait

.install:
    mov byte [boot_flags], 1
    mov si, msg_chosen_install
    call print16
    ret

.start:
    mov byte [boot_flags], 0
    mov si, msg_chosen_start
    call print16
    ret

; -----------------------------------------------------------------------------
;  VBE: set a 32-bpp linear-framebuffer mode and fill in the BootInfo block.
;
;  The kernel used to hardcode the QEMU Bochs VBE framebuffer at 0xE0000000.
;  That address means nothing on real hardware or under VMware, so the BIOS
;  path drew into whatever happened to live there and showed nothing.  Ask the
;  BIOS where the framebuffer actually is instead.
;
;  Clobbers ax/bx/cx/dx/si/di; DS and ES are restored around every BIOS call
;  because `int 10h` is not required to preserve them.
; -----------------------------------------------------------------------------
vbe_setup:
    ; --- 4F00: controller info, to confirm VBE 2.0+ (PhysBasePtr needs it) ---
    mov ax, 0x4F00
    mov di, vbe_controller
    int 0x10
    push ds
    push es
    xor dx, dx
    mov ds, dx
    mov es, dx
    cmp ax, 0x004F
    jne .fail_pop
    cmp dword [vbe_controller], 0x41534556     ; "VESA" as a little-endian dword
    jne .fail_pop
    cmp word [vbe_controller + 4], 0x0200      ; VbeVersion >= 2.0
    jb .fail_pop
    pop es
    pop ds

    ; --- walk the candidate list, best resolution first ---
    mov si, vbe_mode_list
.next:
    mov cx, [si]
    cmp cx, 0xFFFF                             ; end of list
    je .fail
    add si, 2

    mov ax, 0x4F01                             ; get mode info
    mov di, vbe_modeinfo
    int 0x10
    push ds
    push es
    xor dx, dx
    mov ds, dx
    mov es, dx
    cmp ax, 0x004F
    jne .skip_pop
    test word [vbe_modeinfo + 0x00], 0x0080    ; ModeAttributes: linear FB
    jz .skip_pop
    cmp byte [vbe_modeinfo + 0x19], 32         ; BitsPerPixel
    jne .skip_pop
    cmp byte [vbe_modeinfo + 0x1B], 6          ; MemoryModel: direct colour
    jne .skip_pop
    cmp dword [vbe_modeinfo + 0x28], 0         ; PhysBasePtr
    je .skip_pop
    pop es
    pop ds

    ; --- 4F02: set it, with the linear-framebuffer bit ---
    mov bx, cx
    or bx, 0x4000
    mov ax, 0x4F02
    int 0x10
    push ds
    push es
    xor dx, dx
    mov ds, dx
    mov es, dx
    cmp ax, 0x004F
    jne .skip_pop
    pop es
    pop ds

    call fill_bootinfo
    mov si, msg_vbe_ok
    call print16
    ret

.skip_pop:
    pop es
    pop ds
    jmp .next

.fail_pop:
    pop es
    pop ds
.fail:
    ; Leave the BootInfo magic unset — the kernel will notice and warn.
    mov si, msg_no_vbe
    call print16
    ret

; --- copy the mode we just set into the BootInfo block at BOOTINFO_ADDR ---
fill_bootinfo:
    push es
    xor ax, ax
    mov es, ax
    mov di, BOOTINFO_ADDR
    mov cx, 80 / 2                             ; zero the 80-byte block
    xor ax, ax
    rep stosw
    mov di, BOOTINFO_ADDR

    ; magic 'BARROS' + two NULs, as a little-endian u64
    mov dword [di + 0], 0x52524142             ; "BARR"
    mov dword [di + 4], 0x0000534F             ; "OS\0\0"

    ; framebuffer physical address (u64)
    mov eax, [vbe_modeinfo + 0x28]
    mov [di + 8], eax
    mov dword [di + 12], 0

    ; framebuffer size = bytes-per-scanline * height (u64)
    movzx eax, word [vbe_modeinfo + 0x10]
    movzx ecx, word [vbe_modeinfo + 0x14]
    imul eax, ecx
    mov [di + 16], eax
    mov dword [di + 20], 0

    ; width, height (u32 each)
    movzx eax, word [vbe_modeinfo + 0x12]
    mov [di + 24], eax
    movzx eax, word [vbe_modeinfo + 0x14]
    mov [di + 28], eax

    ; pixels per scanline = bytes-per-scanline / 4
    movzx eax, word [vbe_modeinfo + 0x10]
    shr eax, 2
    mov [di + 32], eax

    ; pixel format, in GOP's numbering: 0 = red in the low byte (RGBX),
    ; 1 = blue in the low byte (BGRX).  RedFieldPosition tells us which.
    xor ecx, ecx
    cmp byte [vbe_modeinfo + 0x20], 0
    je .fmt
    mov ecx, 1
.fmt:
    mov [di + 36], ecx

    ; What the boot menu was told to do.  Appended after the original layout,
    ; so everything above kept its offset.
    movzx eax, byte [boot_flags]
    mov [di + 72], eax

    ; memmap fields stay zero: the BIOS path has no E820 yet, and the kernel
    ; falls back to a synthetic map when memmap == 0.
    pop es
    ret

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
;  copy_batch — protected mode staging → final address
;
;  Entered in real mode with EBX = byte offset into the kernel image.  Copies
;  `batch_bytes` from the staging buffer to KERNEL_FINAL + EBX, then drops back
;  to real mode for the next BIOS read.
;
;  The stack pointer has to be preserved explicitly: returning to real mode
;  means reloading SS, and SS:SP must be set together or an interrupt in
;  between uses a segment with the wrong base.
; -----------------------------------------------------------------------------
copy_batch:
    mov [saved_ebx], ebx
    mov [saved_sp], sp
    cli
    lgdt [gdt32_desc]

    mov eax, cr0
    or  eax, 1                          ; CR0.PE
    mov cr0, eax
    jmp 0x08:cpm_entry                  ; 32-bit CS

[bits 32]
cpm_entry:
    mov ax, 0x10
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov fs, ax
    mov gs, ax

    mov esi, STAGING_LINEAR
    mov edi, KERNEL_FINAL
    add edi, ebx
    mov ecx, [batch_bytes]
    shr ecx, 2                          ; dwords
    rep movsd

    ; --- back to real mode, keeping DS/ES base 0 ---
    mov eax, cr0
    and eax, 0xFFFFFFFE
    mov cr0, eax
    jmp 0:crm_entry

[bits 16]
crm_entry:
    cli
    xor ax, ax
    mov ss, ax
    mov sp, [saved_sp]
    mov ds, ax
    mov es, ax
    sti
    mov ebx, [saved_ebx]
    ret

; -----------------------------------------------------------------------------
;  Data
; -----------------------------------------------------------------------------
align 4
dap_kernel:
    db 0x10, 0
    dw 0                    ; sector count — written per batch
    dw STAGING_OFF
    dw STAGING_SEG
    dq KERNEL_DISK_LBA      ; LBA — written per batch

boot_drive: db 0
; 1 = the user chose to install, 0 = just start.  Handed to the kernel in the
; BootInfo so it knows which to do.
boot_flags: db 0
; BIOS tick count when the boot menu appeared, for its timeout.
menu_ticks: dd 0

; Streaming-loader state.  Kept in memory rather than on the stack because the
; stack pointer is reloaded when we drop out of protected mode.
batch_sectors: dw 0
batch_bytes:   dd 0
saved_ebx:     dd 0
saved_sp:      dw 0

msg_loading:    db 13, 10, "[barryOS] stage2: loading kernel...", 13, 10, 0
msg_disk_err:   db "[barryOS] disk read error", 0
msg_no_size:    db "[barryOS] stage2 header not patched (no kernel size)", 0
msg_menu:       db 13, 10
                db "  barryOS", 13, 10
                db "  --------", 13, 10
                db "   [1] Install barryOS onto a hard disk", 13, 10
                db "   [2] Start from this medium", 13, 10
                db "  choice: ", 0
msg_chosen_install: db "1 - install", 13, 10, 13, 10, 0
msg_chosen_start:   db "2 - start", 13, 10, 13, 10, 0
msg_timeout:        db "no key pressed, starting", 13, 10, 13, 10, 0
msg_vbe_ok:     db "[barryOS] VBE mode set", 13, 10, 0
msg_no_vbe:     db "[barryOS] no usable VBE mode", 13, 10, 0

; VBE scratch buffers.  vbe_controller is the 512-byte VBE_INFO_BLOCK, and
; vbe_modeinfo the 256-byte MODE_INFO_BLOCK; both are written directly by the
; BIOS, so they must be in flat real-mode addressable memory (DS=0, ES=0).
align 4
vbe_mode_list:
    dw 0x0118                      ; 1024x768x32
    dw 0x0115                      ;  800x600x32
    dw 0x0112                      ;  640x480x32
    dw 0xFFFF                      ; end

align 4
vbe_controller: times 512 db 0
align 4
vbe_modeinfo:   times 256 db 0

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

    ; The kernel is already in place: the streaming loop copied each batch
    ; straight to KERNEL_FINAL as it read it.

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

    ; BootInfo built by vbe_setup().  If VBE failed the magic is unset and the
    ; kernel falls back on its own, so this is safe to pass unconditionally.
    mov edi, BOOTINFO_ADDR

    jmp KERNEL_FINAL                   ; enter kernel _start

; pad to 40 sectors (20 KiB) — occupies LBA 1..40, kernel starts at LBA 40
times 40*512 - ($-$$) db 0
