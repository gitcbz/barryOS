; =============================================================================
;  barryOS — BIOS Master Boot Record (Stage 1)
; -----------------------------------------------------------------------------
;  Loaded by BIOS ROM at 0000:7C00h. 512 bytes total, magic 0x55AA at 510.
;  Job:
;    1. Set up real-mode segments + stack.
;    2. Save boot drive (DL).
;    3. Read stage2 (LBA 1..STAGE2_SECTORS) to 0000:7E00h via int 13h/42h.
;    4. Far-jump to 0000:7E00h, passing DL in DL (BIOS convention).
;
;  Self-developed; no Linux/GRUB code. Real-mode 16-bit.
; =============================================================================
[bits 16]
[org 0x7C00]

STAGE2_LOAD_SEG    equ 0x0000
STAGE2_LOAD_OFF    equ 0x7E00
STAGE2_LBA_START   equ 1
STAGE2_SECTORS     equ 31            ; 31 * 512 = 15872 bytes (≈16 KiB)

start:
    cli
    xor ax, ax
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov sp, 0x7C00                   ; stack grows down from 0x7C00
    mov bp, sp
    sti

    ; Save boot drive (BIOS passes it in DL)
    mov [boot_drive], dl

    ; --- Load stage2 via int 13h extended read (LBA) ---
    mov si, dap
    mov ah, 0x42                     ; Extended Read Sectors
    mov dl, [boot_drive]
    int 0x13
    jc disk_error

    ; --- Jump to stage2 ---
    jmp STAGE2_LOAD_SEG:STAGE2_LOAD_OFF

disk_error:
    ; Print "E1" to the screen (teletype) and halt
    mov al, 'E'
    mov ah, 0x0E
    mov bx, 0x0007
    int 0x10
    mov al, '1'
    int 0x10
.hang:
    hlt
    jmp .hang

; --- Disk Address Packet (DAP) for int 13h/42h ---
align 4
dap:
    db 0x10                          ; size of packet = 16
    db 0                             ; reserved
    dw STAGE2_SECTORS                ; number of sectors to read
    dw STAGE2_LOAD_OFF               ; offset
    dw STAGE2_LOAD_SEG               ; segment
    dq STAGE2_LBA_START              ; starting LBA (64-bit)

boot_drive: db 0

; --- Padding + magic ---
times 510-($-$$) db 0
dw 0xAA55                           ; BIOS MBR magic (little-endian 0x55 0xAA)
