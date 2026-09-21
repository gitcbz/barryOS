/* =============================================================================
 *  barryOS — UEFI loader PE32+ linker script
 *  Produces a PE32+ (EFI application, subsystem 10) image with entry efi_main.
 * ==========================================================================*/
OUTPUT_FORMAT(pei-x86-64)
ENTRY(efi_main)

SECTIONS
{
    . = 0x10000;
    .text : {
        *(.text*)
    }
    .data : {
        *(.data*)
    }
    .rdata : {
        *(.rdata*)
        *(.rodata*)
    }
    .pdata : {
        *(.pdata*)
    }
    .xdata : {
        *(.xdata*)
    }
    .bss : {
        *(.bss*)
        *(COMMON)
    }
    /DISCARD/ : {
        *(.comment)
        *(.note*)
        *(.eh_frame*)
        *(.debug*)
    }
}
