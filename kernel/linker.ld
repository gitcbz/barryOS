/* =============================================================================
 *  barryOS — kernel linker script
 *  Kernel loads at physical 0x00100000 (1 MiB).  Entry _start must be the
 *  first byte of the flat binary, so .text.entry is placed before .text.
 * ==========================================================================*/
OUTPUT_FORMAT(elf64-x86-64)
OUTPUT_ARCH(i386:x86-64)
ENTRY(_start)

KERNEL_LOAD = 0x00100000;

SECTIONS
{
    . = KERNEL_LOAD;

    .text : {
        KEEP(*(.text.entry))
        *(.text .text.*)
    }

    .rodata : ALIGN(0x1000) {
        *(.rodata .rodata.*)
        *(.interp)
    }

    .data : ALIGN(0x1000) {
        *(.data .data.*)
    }

    .bss : ALIGN(0x1000) {
        __bss_start = .;
        *(.bss .bss.*)
        *(COMMON)
        __bss_end = .;
    }

    /DISCARD/ : {
        *(.comment)
        *(.note*)
        *(.eh_frame*)
        *(.eh_frame_hdr*)
        *(.debug*)
    }
}
