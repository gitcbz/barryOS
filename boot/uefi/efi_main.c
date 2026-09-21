/* =============================================================================
 *  barryOS — UEFI loader (self-developed PE32+ EFI application)
 *  ---------------------------------------------------------------------------
 *  Entered by UEFI firmware in 64-bit long mode.  Builds a BootInfo struct,
 *  loads /kernel.bin from the ESP into physical 0x00100000, queries GOP for
 *  the framebuffer, captures the UEFI memory map, calls ExitBootServices,
 *  then jumps to the kernel entry at 0x00100000 with RDI = &BootInfo.
 *
 *  Entry signature is the EFI ABI (MS x64): RCX = image handle, RDX = ST.
 * ==========================================================================*/
#include "efi_types.h"

#define KERNEL_LOAD_ADDR   0x00100000ULL
#define KERNEL_MAX_PAGES    256                /* 256 * 4 KiB = 1 MiB cap */
#define BOOTINFO_ADDR      0x00070000ULL       /* 1 page for BootInfo */
#define STACK_ADDR         0x00080000ULL       /* 16 pages for stack */
#define STACK_TOP          0x00090000ULL
#define MEMMAP_ADDR        0x000A0000ULL       /* room for memmap (16 pages) */
#define MEMMAP_PAGES       16

/* forward decl of global system table pointer (defined below helpers) */
extern EFI_SYSTEM_TABLE *g_SystemTable;
static void putc_(CHAR16 c);
static void puts_(const CHAR16 *s);
static void puts8(const char *s);
static void put_hex(unsigned long long v);
static void put_dec(unsigned long long v);

/* ---------------------------------------------------------------------------
 *  efi_main — entry point, EFI ABI.
 * ------------------------------------------------------------------------- */
EFI_STATUS EFIAPI efi_main(EFI_HANDLE ImageHandle, EFI_SYSTEM_TABLE *SystemTable)
{
    EFI_STATUS st;
    g_SystemTable = SystemTable;
    EFI_BOOT_SERVICES *BS = SystemTable->BootServices;
    EFI_SIMPLE_TEXT_OUTPUT_PROTOCOL *ConOut = SystemTable->ConOut;

    /* clear + banner */
    ConOut->ClearScreen(ConOut);
    puts_(u"[barryOS] UEFI loader starting\r\n");

    /* ---- 1. Locate our own EFI_LOADED_IMAGE to find the boot device ------ */
    EFI_GUID imgGuid = EFI_LOADED_IMAGE_PROTOCOL_GUID;
    EFI_LOADED_IMAGE *Loaded = NULL;
    st = BS->HandleProtocol(ImageHandle, &imgGuid, (void**)&Loaded);
    if (st != EFI_SUCCESS) { puts_(u"[barryOS] FAIL: LoadedImage\r\n"); goto hang; }
    if (Loaded->DeviceHandle == NULL) { puts_(u"[barryOS] FAIL: no device\r\n"); goto hang; }

    /* ---- 2. Locate EFI_SIMPLE_FILE_SYSTEM (any handle, first wins) --------
     * Loaded->DeviceHandle may be the raw disk handle (no SimpleFS), so we
     * use LocateProtocol to grab the first filesystem protocol directly.
     */
    EFI_GUID fsGuid = EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_GUID;
    EFI_SIMPLE_FILE_SYSTEM_PROTOCOL *FS = NULL;
    st = BS->LocateProtocol(&fsGuid, NULL, (void**)&FS);
    if (st != EFI_SUCCESS) {
        /* fallback: try the device handle from LoadedImage */
        st = BS->HandleProtocol(Loaded->DeviceHandle, &fsGuid, (void**)&FS);
    }
    if (st != EFI_SUCCESS) { puts_(u"[barryOS] FAIL: SimpleFS\r\n"); goto hang; }

    EFI_FILE_PROTOCOL *Root = NULL;
    st = FS->OpenVolume(FS, &Root);
    if (st != EFI_SUCCESS) { puts_(u"[barryOS] FAIL: OpenVolume\r\n"); goto hang; }

    /* ---- 3. Open \kernel.bin ---------------------------------------------- */
    EFI_FILE_PROTOCOL *KernelFile = NULL;
    st = Root->Open(Root, &KernelFile, u"kernel.bin", 0x01 /*READ*/, 0);
    if (st != EFI_SUCCESS) { puts_(u"[barryOS] FAIL: open kernel.bin\r\n"); goto hang; }

    /* ---- 4. Get file size via EFI_FILE_INFO ------------------------------- */
    EFI_GUID infoGuid = EFI_FILE_INFO_ID;
    UINT8 infobuf[256];
    UINTN infosz = sizeof(infobuf);
    st = KernelFile->GetInfo(KernelFile, &infoGuid, &infosz, infobuf);
    if (st != EFI_SUCCESS) { puts_(u"[barryOS] FAIL: GetInfo\r\n"); goto hang; }
    /* EFI_FILE_INFO layout: UINT64 Size, UINT64 FileSize, UINT64 PhysicalSize,
       EFI_TIME CreateTime, EFI_TIME LastAccessTime, EFI_TIME ModificationTime,
       UINT64 Attribute, CHAR16 FileName[].  FileSize is at offset 8. */
    UINT64 FileSize = *(UINT64*)(infobuf + 8);
    puts_(u"[barryOS] kernel.bin size: "); put_dec(FileSize); puts_(u" bytes\r\n");

    /* ---- 5. Allocate a buffer (anywhere) and read kernel into it ------------
     * 0x100000 may be in UEFI's used pool, so AllocateAddress can fail.  We
     * allocate anywhere, read the kernel, and memcpy it to 0x100000 after
     * ExitBootServices (when all EfiBootServicesData becomes ours).
     */
    EFI_PHYSICAL_ADDRESS Kbuf = 0;
    UINTN Kpages = (UINTN)((FileSize + 4095) / 4096);
    if (Kpages < 16) Kpages = 16;                   /* min 64 KiB */
    if (Kpages > KERNEL_MAX_PAGES) Kpages = KERNEL_MAX_PAGES;
    st = BS->AllocatePages(AllocateAnyPages, EfiLoaderData, Kpages, &Kbuf);
    if (st != EFI_SUCCESS) {
        puts_(u"[barryOS] FAIL: AllocatePages buf ("); put_hex(st); puts_(u")\r\n");
        goto hang;
    }
    UINTN read = (UINTN)FileSize;
    st = KernelFile->Read(KernelFile, &read, (void*)Kbuf);
    if (st != EFI_SUCCESS) { puts_(u"[barryOS] FAIL: Read kernel\r\n"); goto hang; }
    KernelFile->Close(KernelFile);
    Root->Close(Root);
    puts_(u"[barryOS] kernel read into buffer, will copy to 0x100000 after exit\r\n");

    /* ---- 6. Locate GOP for framebuffer info -------------------------------- */
    EFI_GUID gopGuid = EFI_GRAPHICS_OUTPUT_PROTOCOL_GUID;
    EFI_GRAPHICS_OUTPUT_PROTOCOL *Gop = NULL;
    st = BS->LocateProtocol(&gopGuid, NULL, (void**)&Gop);
    if (st != EFI_SUCCESS) { puts_(u"[barryOS] WARN: no GOP\r\n"); }

    /* ---- 7. Allocate BootInfo (anywhere) ----------------------------------- */
    EFI_PHYSICAL_ADDRESS bi_addr = 0;
    BS->AllocatePages(AllocateAnyPages, EfiLoaderData, 1, &bi_addr);
    BootInfo *BI = (BootInfo*)bi_addr;
    /* zero it */
    for (UINTN i = 0; i < sizeof(BootInfo)/8; i++) ((UINT64*)BI)[i] = 0;

    /* ---- 8. Allocate stack (anywhere, 64 KiB) ------------------------------- */
    EFI_PHYSICAL_ADDRESS stk_addr = 0;
    BS->AllocatePages(AllocateAnyPages, EfiLoaderData, 16, &stk_addr);
    UINT64 stk_top = stk_addr + 16 * 4096;

    /* ---- 9. Allocate memmap buffer (anywhere) ------------------------------- */
    EFI_PHYSICAL_ADDRESS mm_addr = 0;
    BS->AllocatePages(AllocateAnyPages, EfiLoaderData, MEMMAP_PAGES, &mm_addr);
    EFI_MEMORY_DESCRIPTOR *Mmap = (EFI_MEMORY_DESCRIPTOR*)mm_addr;
    UINTN MmapSize = MEMMAP_PAGES * 4096;
    UINTN MapKey = 0, DescSize = 0;
    UINT32 DescVer = 0;

    /* ---- 10. GetMemoryMap + ExitBootServices (loop: map may change) -------- */
    for (;;) {
        UINTN sz = MmapSize;
        st = BS->GetMemoryMap(&sz, Mmap, &MapKey, &DescSize, &DescVer);
        if (st != EFI_SUCCESS) { puts_(u"[barryOS] FAIL: GetMemoryMap\r\n"); goto hang; }
        st = BS->ExitBootServices(ImageHandle, MapKey);
        if (st == EFI_SUCCESS) break;
        /* map changed between GetMemoryMap & ExitBootServices: retry */
    }

    /* ---- 10b. Copy kernel buffer -> 0x00100000 ----------------------------- */
    {
        UINT8 *src = (UINT8*)Kbuf;
        UINT8 *dst = (UINT8*)KERNEL_LOAD_ADDR;
        for (UINTN i = 0; i < FileSize; i++) dst[i] = src[i];
    }

    /* ---- 11. Fill BootInfo -------------------------------------------------- */
    BI->magic = BARRYOS_BOOTINFO_MAGIC;
    BI->memmap = Mmap;
    BI->memmap_size = MmapSize;
    BI->memmap_desc_size = DescSize;
    BI->memmap_desc_version = DescVer;
    if (Gop) {
        BI->framebuffer_addr  = Gop->Mode->FrameBufferBase;
        BI->framebuffer_size   = Gop->Mode->FrameBufferSize;
        BI->width              = Gop->Mode->Info->HorizontalResolution;
        BI->height             = Gop->Mode->Info->VerticalResolution;
        BI->pixels_per_scanline = Gop->Mode->Info->PixelsPerScanLine;
        BI->pixel_format       = Gop->Mode->Info->PixelFormat;
    }

    /* ---- 12. Disable interrupts, set stack, set RDI=&BootInfo, jmp kernel --- */
    __asm__ __volatile__(
        "cli\n\t"
        "mov %0, %%rsp\n\t"
        "mov %1, %%rdi\n\t"
        "mov %2, %%rax\n\t"
        "jmp *%%rax\n\t"
        :
        : "r"(stk_top),
          "r"((UINT64)BI),
          "r"((UINT64)KERNEL_LOAD_ADDR)
        : "rax", "rdi", "memory"
    );

    /* unreachable */
    for (;;) __asm__ __volatile__("hlt");

hang:
    puts_(u"[barryOS] halted.\r\n");
    for (;;) __asm__ __volatile__("hlt");
}

/* ===========================================================================
 *  tiny text helpers (UEFI text mode, wide chars)
 * ======================================================================== */
EFI_SYSTEM_TABLE *g_SystemTable = NULL;

static void putc_(CHAR16 c) {
    if (!g_SystemTable) return;
    if (c == u'\n') {
        CHAR16 nl[2] = {u'\r', 0};
        g_SystemTable->ConOut->OutputString(g_SystemTable->ConOut, nl);
        CHAR16 nl2[2] = {u'\n', 0};
        g_SystemTable->ConOut->OutputString(g_SystemTable->ConOut, nl2);
    } else {
        CHAR16 one[2] = {c, 0};
        g_SystemTable->ConOut->OutputString(g_SystemTable->ConOut, one);
    }
}
static void puts_(const CHAR16 *s) {
    while (*s) { putc_(*s); s++; }
}
static void puts8(const char *s) __attribute__((unused));
static void puts8(const char *s) {
    while (*s) { putc_((CHAR16)(unsigned char)*s); s++; }
}
static void put_hex(unsigned long long v) {
    CHAR16 tmp[17];
    tmp[16] = 0;
    for (int j = 15; j >= 0; j--) {
        unsigned d = (unsigned)(v & 0xF);
        tmp[j] = (CHAR16)(d < 10 ? u'0' + d : u'A' + d - 10);
        v >>= 4;
    }
    puts_(tmp);
}
static void put_dec(unsigned long long v) {
    CHAR16 tmp[21];
    int i = 20; tmp[20] = 0;
    if (v == 0) { putc_(u'0'); return; }
    while (v && i > 0) { tmp[--i] = (CHAR16)(u'0' + (v % 10)); v /= 10; }
    puts_(&tmp[i]);
}
