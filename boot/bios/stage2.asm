; =============================================================================
;  barryOS — BIOS Stage 2 Bootloader
; -----------------------------------------------------------------------------
;  Loaded by MBR at 0000:7E00h (16-bit real mode).  Final job is to land in
;  64-bit long mode and jump to the kernel flat binary at 0x00100000.
;
;  Flow:
;    [16-bit]  stage the compressed kernel (LBA 41..) in conventional memory
;    [16-bit]  VBE: set a 32-bpp linear framebuffer, fill a BootInfo block
;    [16-bit]  enable A20 (fast 0x92, then kbd fallback)
;    [16-bit]  load 32-bit GDT, enter 32-bit protected mode
;    [32-bit]  decompress it to 0x100000 (LZ4 block)
;    [32-bit]  build identity page tables (first 256 MiB)
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

; The kernel's compressed image is staged in conventional memory -- the BIOS
; can only write to a 16-bit segment:offset -- and decompressed to
; KERNEL_FINAL by the single protected-mode switch at the end of the load.
; Everything from 0x10000 up to the EBDA is ours to use, so the ceiling on the
; image that is *staged* is KERNEL_STAGE_MAX.  That is why it is compressed:
; the window cannot grow, and the kernel did.
STAGING_SEG        equ 0x1000       ; staging area segment → linear 0x10000
STAGING_OFF        equ 0x0000
STAGING_LINEAR     equ 0x00010000
STAGING_LIMIT      equ 0x0009F000   ; top of the staging area, just under video RAM
KERNEL_STAGE_MAX   equ STAGING_LIMIT - STAGING_LINEAR
BATCH_SECTORS      equ 64           ; 32 KiB per BIOS call
BATCH_BYTES        equ BATCH_SECTORS * 512

; The kernel image starts here and the stack grows down from here, so the two
; together must fit in between.  16 MiB leaves room for an 8 MiB kernel with
; 8 MiB of stack -- although the staging area below 1 MiB caps the image well
; before that; see KERNEL_STAGE_MAX.
STACK_TOP          equ 0x01000000

; Page table addresses.  Above the staging buffer, below the video window.
PML4_ADDR          equ 0x00080000
PDPT_ADDR          equ 0x00081000
PD_ADDR            equ 0x00082000

; How much of RAM the hand-off map covers, in 2 MiB pages.  128 of them is
; 256 MiB, which is exactly what the kernel's frame-allocator bitmap can hand
; out, so nothing the kernel can allocate is unmapped.
;
; This has to cover STACK_TOP.  The kernel installs its own 4 GiB map in
; mem::init, but it runs on the bootloader's stack until it gets there -- so
; the stack has to be mapped by *these* tables.  Two pages (4 MiB), which is
; what this used to be, covers the kernel image and nothing else: raising
; STACK_TOP to 16 MiB without widening the map meant `mov rsp, STACK_TOP` put
; the stack on an unmapped page, and the first push triple-faulted before a
; single line of kernel code ran.
IDENTITY_2M_PAGES  equ 128
PAGE_2M_ADDR_MASK  equ 0x83          ; present | writable | page size

; -----------------------------------------------------------------------------
;  Serial trace: COM1 equates and the one macro usable from any bitness.
;
;  NASM expands macros in file order, so both have to sit above the first use
;  -- which is in the streaming loop, barely a hundred lines down.
; -----------------------------------------------------------------------------
SER_PORT        equ 0x3F8
SER_LSR_THRE    equ 0x20           ; transmitter holding register empty

; One character on COM1.  The ser_* helper functions further down are real-mode
; only (`lodsb` off DS:SI, and `push ax` is not encodable in 64-bit mode), so
; this is the one that works at every stage.  Clobbers AX and DX.
%macro SER_BYTE 1
    mov dx, SER_PORT + 5
%%wait:
    in al, dx
    test al, SER_LSR_THRE
    jz %%wait
    mov dx, SER_PORT
    mov al, %1
    out dx, al
%endmacro

; BootInfo handed to the kernel in RDI (matches kernel/src/bootinfo.rs).
BOOTINFO_ADDR      equ 0x00006000
BOOTINFO_MAGIC     equ 0x0000534F52524142   ; 'BARROS' + two NULs

stage2_start:
    ; Two bytes of jump over the build-time header, two of padding, then the
    ; kernel size.  The Makefile patches offset 4 with the real byte count,
    ; which frees the loader from a compile-time sector budget entirely.
    jmp short real_start
    times 2 db 0
; The compressed image's length, and the length it should decompress to.
; Both are patched in by scripts/patch-stage2.py.
image_bytes: dd 0
kernel_bytes: dd 0

real_start:
    ; --- save boot drive ---
    mov [boot_drive], dl

    ; --- give ourselves a real stack (the MBR's leaves only ~2 KiB) ---
    cli
    xor ax, ax
    mov ss, ax
    mov sp, 0x5FFE                     ; just under BOOTINFO_ADDR
    sti

    ; --- serial trace: on a headless run this is the only view into stage2 ---
    call ser_init
    movzx eax, byte [boot_drive]
    mov si, msg_ser_enter
    call ser_mark

    ; --- announce (teletype) ---
    mov si, msg_loading
    call print16

    ; --- install or just start? ---
    ; Shown before VBE takes the display: BIOS teletype output does not work
    ; once a graphics mode is set, and the splash needs a real framebuffer.
    call boot_menu
    movzx eax, byte [boot_flags]
    mov si, msg_ser_menu
    call ser_mark

    ; --- VBE: set up the framebuffer before anything else touches the screen ---
    call vbe_setup

    ; --- stage the kernel below 1 MiB ------------------------------------
    ; The BIOS can only be asked to write to a 16-bit segment:offset, so the
    ; image has to land in conventional memory first and be copied up to
    ; KERNEL_FINAL afterwards.  32 KiB per read, into successive positions in
    ; the staging area; no copying between batches.
    ;
    ; The earlier version of this read one batch, switched to protected mode to
    ; copy it, came back for the next, and repeated.  That return trip never
    ; worked on this machine: the mode change back to real mode did not land
    ; where it was aimed, whatever order the two steps were done in, and the
    ; first 32 KiB of the kernel was all that ever got loaded -- and not even
    ; that, since the copy goes to 0x100000 while the boot path needs the whole
    ; image.  A single switch at the end, with nothing to come back to, is the
    ; shape the pre-rewrite loader used and the shape that demonstrably boots
    ; here.
    ;
    ; The price is a ceiling of KERNEL_STAGE_MAX instead of the 8 MiB the
    ; staging design was reaching for.
    mov eax, [image_bytes]
    test eax, eax
    jz no_size                          ; header not patched: refuse to guess
    cmp eax, KERNEL_STAGE_MAX
    ja too_big

    xor ebx, ebx
.stream:
    mov eax, [image_bytes]
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
    mov word [dap_kernel + 2], cx

    ; Buffer = STAGING_LINEAR + ebx, written as segment:offset.  Every batch
    ; starts on a 32 KiB boundary, so the offset is always zero and the whole
    ; transfer stays inside one 64 KiB window.
    mov eax, ebx
    shr eax, 4
    add eax, STAGING_SEG
    mov word [dap_kernel + 4], 0
    mov word [dap_kernel + 6], ax

    mov eax, ebx
    shr eax, 9
    add eax, KERNEL_DISK_LBA            ; LBA of this batch
    mov dword [dap_kernel + 8], eax
    mov dword [dap_kernel + 12], 0

    SER_BYTE 'r'                        ; about to ask the BIOS for a batch
    mov si, dap_kernel
    mov ah, 0x42
    mov dl, [boot_drive]
    int 0x13
    jc disk_err
    SER_BYTE 'R'

    movzx eax, word [batch_sectors]
    shl eax, 9
    add ebx, eax
    jmp .stream
.done:
    mov si, msg_ser_loaded
    call ser_line

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

; The image does not fit in conventional memory.  Say so rather than loading a
; prefix of it and jumping into whatever the tail happened to be.
too_big:
    mov si, msg_too_big
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

; Print AX as four hex digits, high nibble first.  Clobbers ax/bx/cx.
; `int 10h` is not required to preserve registers, so both AX and CX go on the
; stack around every call rather than being trusted to survive.
print_hex16:
    mov cx, 4
.digit:
    rol ax, 4
    push ax
    push cx
    and al, 0x0F
    cmp al, 10
    jb .dec
    add al, 'A' - 10
    jmp .emit
.dec:
    add al, '0'
.emit:
    mov ah, 0x0E
    mov bx, 0x0007
    int 0x10
    pop cx
    pop ax
    dec cx
    jnz .digit
    ret

; -----------------------------------------------------------------------------
;  Serial trace helpers.
;
;  Everything before the kernel is otherwise invisible: the screen only shows
;  what the last thing to touch it drew, and a triple fault leaves no trace at
;  all.  `serial0.fileType = "file"` in the VMX captures COM1 to a file on the
;  host, so a few markers turn "it hangs" into "it stopped after X".
;
;  COM1 is initialised to 115200 8N1 with the FIFO off and no interrupts --
;  the same shape the kernel's serial driver uses, so the kernel can carry on
;  writing without reconfiguring anything.
;
;  SER_PORT/SER_LSR_THRE and the SER_BYTE macro are defined at the top of the
;  file, not here: NASM expands macros in file order and the first marker is
;  emitted long before this section is reached.
; -----------------------------------------------------------------------------
ser_init:
    push ax
    push dx
    mov dx, SER_PORT + 1            ; IER: no interrupts
    xor al, al
    out dx, al
    mov dx, SER_PORT + 3            ; LCR: divisor latch access
    mov al, 0x80
    out dx, al
    mov dx, SER_PORT + 0            ; DLL = 1 -> 115200 baud
    mov al, 1
    out dx, al
    mov dx, SER_PORT + 1            ; DLM = 0
    xor al, al
    out dx, al
    mov dx, SER_PORT + 3            ; LCR: 8 bits, no parity, one stop
    mov al, 0x03
    out dx, al
    mov dx, SER_PORT + 2            ; FCR: FIFO off, clear both queues
    mov al, 0x07
    out dx, al
    mov dx, SER_PORT + 4            ; MCR: DTR | RTS | OUT2
    mov al, 0x0B
    out dx, al
    pop dx
    pop ax
    ret

; AL = byte to send.  Clobbers nothing: called from the middle of the boot
; path, where DX holds the boot drive and SI a message pointer.
ser_putc:
    push ax
    push dx
    mov ah, al
    mov dx, SER_PORT + 5            ; LSR
.poll:
    in al, dx
    test al, SER_LSR_THRE
    jz .poll
    mov dx, SER_PORT
    mov al, ah
    out dx, al
    pop dx
    pop ax
    ret

; SI = NUL-terminated string, DS = 0.
ser_puts:
    push ax
    push si
.next:
    lodsb
    test al, al
    jz .done
    call ser_putc
    jmp .next
.done:
    pop si
    pop ax
    ret

; SI = string, AX = value printed after it as four hex digits, then CRLF.
ser_mark:
    push ax
    call ser_puts
    pop ax
    call ser_hex16
    mov si, ser_crlf
    call ser_puts
    ret

; SI = string, then CRLF.  For milestones with no number worth printing.
ser_line:
    call ser_puts
    mov si, ser_crlf
    call ser_puts
    ret

ser_hex16:
    mov cx, 4
.digit:
    rol ax, 4
    push ax
    push cx
    and al, 0x0F
    cmp al, 10
    jb .dec
    add al, 'A' - 10
    jmp .emit
.dec:
    add al, '0'
.emit:
    call ser_putc
    pop cx
    pop ax
    dec cx
    jnz .digit
    ret

ser_crlf: db 13, 10, 0

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

    ; The tick counter lives at 0040:006C -- the BIOS data area, *segment* 0x40.
    ; Reading it as [es:0x6C] with ES=0 lands on physical 0x06C, which is the
    ; interrupt vector table: INT 1Bh's vector, a value that never changes.  The
    ; elapsed-tick subtraction was therefore always zero and the timeout below
    ; never fired.  The menu only ever got past itself because someone pressed
    ; a key, and an unattended or headless boot sat here forever -- which is
    ; exactly what it did.
    push es
    mov ax, 0x40
    mov es, ax
    mov eax, [es:0x6C]
    mov [menu_ticks], eax
    pop es

.wait:
    mov ah, 0x01
    int 0x16                            ; peek: is a key waiting?
    jnz .key

    push es
    mov ax, 0x40
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
;  VBE: pick a 32-bpp linear-framebuffer mode and fill in the BootInfo block.
;
;  The kernel used to hardcode the QEMU Bochs VBE framebuffer at 0xE0000000.
;  That address means nothing on real hardware or under VMware, so the BIOS
;  path drew into whatever happened to live there and showed nothing.  Ask the
;  BIOS where the framebuffer actually is instead.
;
;  The mode numbers are *not* hardcoded, because they cannot be.  This used to
;  walk a list of 0x118 / 0x115 / 0x112 -- but those are the standard VBE
;  entries for 24-bpp packed pixel, so the "BitsPerPixel == 32" test below
;  rejected every one of them and the machine stayed in text mode.  32-bpp
;  modes are a vendor extension with vendor-chosen numbers (Bochs and QEMU
;  put them at 0x140+, VMware elsewhere), so the only portable thing to do is
;  read the list the BIOS hands back in VBE_INFO_BLOCK and walk that.
;
;  Among the acceptable modes, the largest that still fits 1024x768 wins.
;
;  DS stays 0 throughout; ES is borrowed to reach the BIOS's mode list and put
;  back to 0 before every `int 10h`, which needs ES:DI to point at a buffer in
;  our own memory.
; -----------------------------------------------------------------------------
VBE_MAX_WIDTH   equ 1024
VBE_MAX_HEIGHT  equ 768

vbe_setup:
    ; --- 4F00: controller info.  Write the "VBE2" signature first: that is how
    ;     the caller asks for the VBE 2.0+ block, and the mode list is in it.
    mov dword [vbe_controller], 0x32454256     ; "VBE2"
    mov ax, 0x4F00
    mov di, vbe_controller
    int 0x10
    cmp ax, 0x004F
    jne .fail_probe
    cmp dword [vbe_controller], 0x41534556     ; "VESA"
    jne .fail_probe
    cmp word [vbe_controller + 4], 0x0200      ; VbeVersion >= 2.0
    jb .fail_probe

    ; --- where the mode list lives (a real-mode far pointer into the BIOS) ---
    mov ax, [vbe_controller + 0x0E]
    mov [vbe_list_off], ax
    mov ax, [vbe_controller + 0x10]
    mov [vbe_list_seg], ax

    mov word [vbe_best_mode], 0xFFFF           ; nothing chosen yet
    mov dword [vbe_best_area], 0
    mov word [vbe_seen], 0

.next:
    ; Bounded: a malformed list that never reaches the 0xFFFF terminator would
    ; otherwise walk off the end of the BIOS segment and spin here forever.
    inc word [vbe_seen]
    cmp word [vbe_seen], 512
    ja .fail

    mov ax, [vbe_list_seg]
    mov es, ax
    mov bx, [vbe_list_off]
    mov cx, [es:bx]
    add word [vbe_list_off], 2

    xor ax, ax
    mov es, ax
    cmp cx, 0xFFFF                             ; end of list
    je .pick

    mov ax, 0x4F01                             ; get mode info
    mov di, vbe_modeinfo
    int 0x10
    cmp ax, 0x004F
    jne .next

    test word [vbe_modeinfo + 0x00], 0x0001    ; ModeAttributes: mode supported
    jz .next
    test word [vbe_modeinfo + 0x00], 0x0080    ; ModeAttributes: linear FB
    jz .next
    cmp byte [vbe_modeinfo + 0x1B], 6          ; MemoryModel: direct colour
    jne .next
    cmp byte [vbe_modeinfo + 0x19], 32         ; BitsPerPixel
    jne .next
    cmp dword [vbe_modeinfo + 0x28], 0         ; PhysBasePtr
    je .next

    movzx eax, word [vbe_modeinfo + 0x12]      ; XResolution
    cmp ax, VBE_MAX_WIDTH
    ja .next
    movzx edx, word [vbe_modeinfo + 0x14]      ; YResolution
    cmp dx, VBE_MAX_HEIGHT
    ja .next

    imul eax, edx                              ; area, as the tie-breaker
    cmp eax, [vbe_best_area]
    jbe .next
    mov [vbe_best_area], eax
    mov [vbe_best_mode], cx
    jmp .next

.pick:
    mov cx, [vbe_best_mode]
    cmp cx, 0xFFFF
    je .fail

    ; Re-read the winner's ModeInfoBlock: the buffer holds whichever mode was
    ; scanned last, and fill_bootinfo reads the framebuffer address out of it.
    mov ax, 0x4F01
    mov di, vbe_modeinfo
    xor bx, bx
    mov es, bx
    int 0x10
    cmp ax, 0x004F
    jne .fail

    ; --- 4F02: set it, with the linear-framebuffer bit ---
    ; Reloaded from memory, not from CX: the 4F01 above is not required to
    ; return CX unchanged.
    mov bx, [vbe_best_mode]
    or bx, 0x4000
    mov ax, 0x4F02
    int 0x10
    cmp ax, 0x004F
    jne .fail

    call fill_bootinfo
    mov si, msg_vbe_ok
    call print16
    mov ax, [vbe_best_mode]                    ; which mode the BIOS gave us
    call print_hex16
    mov si, msg_crlf
    call print16
    mov si, msg_ser_vbe
    mov ax, [vbe_best_mode]
    call ser_mark
    ret

.fail_probe:
    ; No VBE 2.0 at all -- a different problem from "VBE is there but nothing
    ; in its mode list is usable", and worth saying so.
    mov si, msg_no_vbe20
    call print16
    mov si, msg_ser_novbe
    xor eax, eax
    call ser_mark
    ret
.fail:
    ; Leave the BootInfo magic unset -- the kernel will notice and warn.
    mov si, msg_no_vbe
    call print16
    mov ax, [vbe_seen]                         ; how far the scan got
    call print_hex16
    mov si, msg_crlf
    call print16
    mov si, msg_ser_novbe
    mov ax, [vbe_seen]
    call ser_mark
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

; How many sectors the batch currently being read holds.  Kept in memory
; rather than on the stack because the interrupt handlers are the BIOS's.
batch_sectors: dw 0

msg_loading:    db 13, 10, "[barryOS] stage2: loading kernel...", 13, 10, 0
msg_disk_err:   db "[barryOS] disk read error", 0
msg_no_size:    db "[barryOS] stage2 header not patched (no kernel size)", 0
msg_too_big:    db "[barryOS] kernel is larger than the staging area below 1 MiB", 0
msg_menu:       db 13, 10
                db "  barryOS", 13, 10
                db "  --------", 13, 10
                db "   [1] Install barryOS onto a hard disk", 13, 10
                db "   [2] Start from this medium", 13, 10
                db "  choice: ", 0
msg_chosen_install: db "1 - install", 13, 10, 13, 10, 0
msg_chosen_start:   db "2 - start", 13, 10, 13, 10, 0
msg_timeout:        db "no key pressed, starting", 13, 10, 13, 10, 0
msg_vbe_ok:     db "[barryOS] VBE mode 0x", 0
msg_crlf:       db 13, 10, 0
msg_no_vbe20:   db "[barryOS] no VBE 2.0 BIOS extension", 13, 10, 0
msg_no_vbe:     db "[barryOS] no usable VBE mode, scanned 0x", 0

; Serial trace, so a headless boot leaves a record.
msg_ser_enter:  db "[barryOS] stage2 entered, boot drive 0x", 0
msg_ser_menu:   db "[barryOS] menu answer 0x", 0
msg_ser_loaded: db "[barryOS] compressed image staged below 1 MiB", 0
msg_ser_vbe:    db "[barryOS] VBE mode 0x", 0
msg_ser_novbe:  db "[barryOS] VBE failed, modes scanned 0x", 0

; VBE scratch.  vbe_controller is the 512-byte VBE_INFO_BLOCK and vbe_modeinfo
; the 256-byte MODE_INFO_BLOCK; both are written directly by the BIOS, so they
; must be in flat real-mode addressable memory (DS=0, ES=0).
;
; vbe_list_off/vbe_list_seg carry the far pointer to the BIOS's mode list --
; it lives in the BIOS's segment, which is why the walk has to borrow ES.
; vbe_best_* remember the winner while the scan is still running.
align 4
vbe_list_off:   dw 0
vbe_list_seg:   dw 0
vbe_best_mode:  dw 0xFFFF
vbe_best_area:  dd 0
vbe_seen:       dw 0

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
gdt32_code16:                         ; 0x18 - 16-bit code, flat, 4 GiB
    dw 0xFFFF, 0x0000                 ; the D bit (0xCF -> 0x00) is the whole point
    db 0x00, 0x9A, 0x00, 0x00
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

    SER_BYTE 'P'                       ; reached 32-bit protected mode

    ; The kernel sits compressed in the staging buffer below 1 MiB and this
    ; is the one trip it takes to its final home.  It is decompressed rather
    ; than copied because the staging window -- 0x10000 to the start of video
    ; RAM -- is 572 KiB, which the kernel has outgrown and which cannot be
    ; made bigger: the BIOS can only be asked to write to a 16-bit
    ; segment:offset, so nothing larger can be read in the first place.
    ; Reading it in pieces and copying each one up means switching back to
    ; real mode between pieces, which an earlier version of this loader tried
    ; and did not survive.
    ;
    ; LZ4 block format: a token byte, a literal run, a two-byte offset back
    ; into what has already been written, and a match length, with the two
    ; lengths extended by a run of 255s.  About forty instructions, no bit
    ; reader and no dictionary.
    mov esi, STAGING_LINEAR
    mov edi, KERNEL_FINAL
.sequence:
    movzx eax, byte [esi]
    inc esi
    mov ebx, eax                    ; the token, for the match nibble
    shr eax, 4                      ; literal length
    cmp eax, 15
    jne .lit_ok
    call read_extended
.lit_ok:
    mov ecx, eax
    rep movsb                       ; the literals
    mov edx, STAGING_LINEAR
    add edx, [image_bytes]
    cmp esi, edx
    jae .decoded                    ; a block ends with literals and no match
    movzx edx, word [esi]           ; offset back into the output
    add esi, 2
    mov eax, ebx
    and eax, 15
    add eax, 4                      ; a match is at least four bytes
    cmp eax, 19
    jne .match_ok
    call read_extended
.match_ok:
    mov ecx, eax
    mov ebp, edi
    sub ebp, edx                    ; the match may overlap what it copies,
.match:                             ; which is why this is a byte at a time
    mov al, [ebp]
    mov [edi], al
    inc ebp
    inc edi
    dec ecx
    jnz .match
    mov edx, STAGING_LINEAR
    add edx, [image_bytes]
    cmp esi, edx
    jb .sequence
.decoded:
    ; A short output means the stream was not what it claimed, and jumping
    ; into it would run whatever those bytes happen to be.  Thirty-two bytes
    ; of leeway for an image whose length is not a multiple of four.
    mov eax, edi
    sub eax, KERNEL_FINAL
    add eax, 32
    cmp eax, [kernel_bytes]
    jb decode_short

    SER_BYTE 'K'                       ; kernel image is at KERNEL_FINAL

    ; --- zero page tables (3 pages) ---
    mov edi, PML4_ADDR
    xor eax, eax
    mov ecx, (4096 * 3) / 4
    rep stosd

    ; --- PML4[0] -> PDPT ---
    mov dword [PML4_ADDR], PDPT_ADDR | 0x03
    ; --- PDPT[0] -> PD ---
    mov dword [PDPT_ADDR], PD_ADDR | 0x03

    ; --- PD[0..IDENTITY_2M_PAGES) = identity-mapped 2 MiB pages ---
    ; One PD covers 1 GiB, so this is the whole address space the kernel can
    ; touch before it builds its own tables: the kernel image at 1 MiB, the
    ; stack just below STACK_TOP, and any frame the allocator hands out.
    mov edi, PD_ADDR
    xor eax, eax                        ; physical address, stepping by 2 MiB
    mov ecx, IDENTITY_2M_PAGES
.fill:
    mov edx, eax
    or  edx, PAGE_2M_ADDR_MASK
    mov [edi], edx
    add edi, 8
    add eax, 0x200000
    dec ecx
    jnz .fill

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

; These sit after an unconditional jump on purpose.  Placed among the
; instructions above they would be fallen into, and the `ret` at the end of
; the first would pop whatever the page-table setup had left on the stack.

; eax += a run of 255s and the byte that ends it.  Used for both lengths,
; and it lives here because only the decompressor calls it.
read_extended:
    push ecx
.next:
    movzx ecx, byte [esi]
    inc esi
    add eax, ecx
    cmp ecx, 255
    je .next
    pop ecx
    ret

decode_short:
    ; Say how much was decoded: a stream that stops early and a stream that
    ; decodes to nothing need different fixes, and the number says which.
    SER_BYTE 'S'
    mov ecx, 8
.hex:
    rol eax, 4
    push eax
    push ecx
    and al, 0x0F
    cmp al, 10
    jb .dec
    add al, 'A' - 10
    jmp .emit
.dec:
    add al, '0'
.emit:
    SER_BYTE al
    pop ecx
    pop eax
    dec ecx
    jnz .hex
    SER_BYTE 10
.hang: hlt
    jmp .hang


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

    SER_BYTE 'L'                       ; reached long mode, about to jump

    jmp KERNEL_FINAL                   ; enter kernel _start

; pad to 40 sectors (20 KiB) — occupies LBA 1..40, kernel starts at LBA 40
times 40*512 - ($-$$) db 0
