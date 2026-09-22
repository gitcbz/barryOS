/* =============================================================================
 *  barryOS — UEFI types (self-developed, hand-written)
 *  ---------------------------------------------------------------------------
 *  Minimal subset of the UEFI specification structures needed by the barryOS
 *  EFI loader.  Written from the UEFI 2.10 spec; no third-party headers.
 *  All EFI protocol function pointers use the EFI (MS x64) calling convention,
 *  enforced with __attribute__((ms_abi)) so gcc on Linux calls them correctly.
 * ==========================================================================*/
#ifndef BARRYOS_EFI_TYPES_H
#define BARRYOS_EFI_TYPES_H

typedef unsigned char       UINT8;
typedef unsigned short      UINT16;
typedef unsigned int        UINT32;
typedef unsigned long long  UINT64;
typedef long long           INT64;
typedef int                 INT32;
typedef unsigned long long  UINTN;        /* same width as a pointer */
typedef UINTN               EFI_STATUS;
typedef UINT64              EFI_PHYSICAL_ADDRESS;
typedef UINT64              EFI_VIRTUAL_ADDRESS;
typedef UINT32              EFI_TPL;
typedef void               *EFI_HANDLE;
typedef unsigned short      CHAR16;
typedef char                CHAR8;
typedef UINT8               BOOLEAN;

#define NULL ((void*)0)
#define IN
#define OUT
#define EFIAPI __attribute__((ms_abi))
#define EFI_SUCCESS              0ULL
#define EFI_ERROR(n) (((UINTN)(n)) >> 63 == 1)
#define EFI_ERROR_CODE(n) ((UINTN)(n) & 0x7FFFFFFFFFFFFFFFULL)
#define EFI_LOAD_ERROR           0x8000000000000001ULL
#define EFI_INVALID_PARAMETER    0x8000000000000002ULL
#define EFI_UNSUPPORTED          0x8000000000000003ULL
#define EFI_BAD_BUFFER_SIZE      0x8000000000000004ULL
#define EFI_BUFFER_TOO_SMALL     0x8000000000000005ULL
#define EFI_NOT_FOUND            0x800000000000000EULL
#define EFI_OUT_OF_RESOURCES     0x8000000000000009ULL

typedef struct {
    UINT32 Data1;
    UINT16 Data2;
    UINT16 Data3;
    UINT8  Data4[8];
} EFI_GUID;

#define EFI_LOADED_IMAGE_PROTOCOL_GUID \
    {0x5B1B31A1,0x9562,0x11D2,{0x8E,0x3F,0x00,0xA0,0xC9,0x69,0x72,0x3B}}
#define EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_GUID \
    {0x964E5B22,0x6459,0x11D2,{0x8E,0x39,0x00,0xA0,0xC9,0x69,0x72,0x3B}}
#define EFI_FILE_INFO_ID \
    {0x09576E92,0x6D3F,0x11D2,{0x8E,0x39,0x00,0xA0,0xC9,0x69,0x72,0x3B}}
#define EFI_GRAPHICS_OUTPUT_PROTOCOL_GUID \
    {0x9042A9DE,0x23DC,0x4A38,{0x96,0xFB,0x7A,0xDE,0xD0,0x80,0x51,0x6A}}

typedef enum {
    EfiReservedMemoryType,
    EfiLoaderCode,
    EfiLoaderData,
    EfiBootServicesCode,
    EfiBootServicesData,
    EfiRuntimeServicesCode,
    EfiRuntimeServicesData,
    EfiConventionalMemory,
    EfiUnusableMemory,
    EfiACPIReclaimMemory,
    EfiACPIMemoryNVS,
    EfiMemoryMappedIO,
    EfiMemoryMappedIOPortSpace,
    EfiPalCode,
    EfiPersistentMemory,
    EfiMaxMemoryType
} EFI_MEMORY_TYPE;

typedef enum {
    AllocateAnyPages,
    AllocateMaxAddress,
    AllocateAddress,
    MaxAllocateType
} EFI_ALLOCATE_TYPE;

/* EfiLoaderData = 0x2, used by AllocatePool/AllocatePages */

/* ----------------------------------------------------------------- headers */
typedef struct {
    UINT64 Signature;
    UINT32 Revision;
    UINT32 HeaderSize;
    UINT32 CRC32;
    UINT32 Reserved;
} EFI_TABLE_HEADER;

/* ----------------------------------------------------------------- text I/O */
struct EFI_SIMPLE_TEXT_INPUT_PROTOCOL;
typedef struct EFI_SIMPLE_TEXT_OUTPUT_PROTOCOL EFI_SIMPLE_TEXT_OUTPUT_PROTOCOL;

typedef EFI_STATUS (EFIAPI *EFI_TEXT_RESET)(EFI_SIMPLE_TEXT_OUTPUT_PROTOCOL*, BOOLEAN);
typedef EFI_STATUS (EFIAPI *EFI_TEXT_STRING)(EFI_SIMPLE_TEXT_OUTPUT_PROTOCOL*, CHAR16*);
typedef EFI_STATUS (EFIAPI *EFI_TEXT_TEST_STRING)(EFI_SIMPLE_TEXT_OUTPUT_PROTOCOL*, CHAR16*);
typedef EFI_STATUS (EFIAPI *EFI_TEXT_CLEAR_SCREEN)(EFI_SIMPLE_TEXT_OUTPUT_PROTOCOL*);
typedef EFI_STATUS (EFIAPI *EFI_TEXT_QUERY_MODE)(EFI_SIMPLE_TEXT_OUTPUT_PROTOCOL*, UINTN, UINTN*, UINTN*);
typedef EFI_STATUS (EFIAPI *EFI_TEXT_SET_MODE)(EFI_SIMPLE_TEXT_OUTPUT_PROTOCOL*, UINTN);
typedef EFI_STATUS (EFIAPI *EFI_TEXT_SET_ATTRIBUTE)(EFI_SIMPLE_TEXT_OUTPUT_PROTOCOL*, UINTN);

typedef struct {
    INT32 MaxMode;
    INT32 Mode;
    EFI_SIMPLE_TEXT_OUTPUT_PROTOCOL *Attribute;
    INT32 CursorColumn;
    INT32 CursorRow;
    BOOLEAN CursorVisible;
} SIMPLE_TEXT_OUTPUT_MODE;

struct EFI_SIMPLE_TEXT_OUTPUT_PROTOCOL {
    EFI_TEXT_RESET Reset;
    EFI_TEXT_STRING OutputString;
    EFI_TEXT_TEST_STRING TestString;
    EFI_TEXT_QUERY_MODE QueryMode;
    EFI_TEXT_SET_MODE SetMode;
    EFI_TEXT_SET_ATTRIBUTE SetAttribute;
    EFI_TEXT_CLEAR_SCREEN ClearScreen;
    SIMPLE_TEXT_OUTPUT_MODE *Mode;
};

typedef struct {
    EFI_TABLE_HEADER Hdr;
    void *WaitForKey;                       /* EFI_EVENT */
    void *ReadKeyStroke;                    /* function ptr */
} EFI_SIMPLE_TEXT_INPUT_PROTOCOL;

/* ----------------------------------------------------------------- memory   */
typedef struct {
    UINT32 Type;
    UINT32 Pad;
    EFI_PHYSICAL_ADDRESS PhysicalStart;
    UINT64 VirtualStart;
    UINT64 NumberOfPages;
    UINT64 Attribute;
} EFI_MEMORY_DESCRIPTOR;

typedef EFI_STATUS (EFIAPI *EFI_ALLOCATE_PAGES)(EFI_ALLOCATE_TYPE, EFI_MEMORY_TYPE, UINTN, EFI_PHYSICAL_ADDRESS*);
typedef EFI_STATUS (EFIAPI *EFI_FREE_PAGES)(EFI_PHYSICAL_ADDRESS, UINTN);
typedef EFI_STATUS (EFIAPI *EFI_GET_MEMORY_MAP)(UINTN*, EFI_MEMORY_DESCRIPTOR*, UINTN*, UINTN*, UINT32*);
typedef EFI_STATUS (EFIAPI *EFI_ALLOCATE_POOL)(EFI_MEMORY_TYPE, UINTN, void**);
typedef EFI_STATUS (EFIAPI *EFI_FREE_POOL)(void*);
typedef EFI_STATUS (EFIAPI *EFI_HANDLE_PROTOCOL)(EFI_HANDLE, EFI_GUID*, void**);
typedef EFI_STATUS (EFIAPI *EFI_LOCATE_PROTOCOL)(EFI_GUID*, void*, void**);
typedef EFI_STATUS (EFIAPI *EFI_EXIT_BOOT_SERVICES)(EFI_HANDLE, UINTN);

/* ----------------------------------------------------------------- events   */
typedef EFI_STATUS (EFIAPI *EFI_CREATE_EVENT)(UINT32, EFI_TPL, void*, void*, void**);
typedef EFI_STATUS (EFIAPI *EFI_SET_TIMER)(void*, UINT32, UINT64);
typedef EFI_STATUS (EFIAPI *EFI_WAIT_FOR_EVENT)(UINTN, void**, UINTN*);
typedef EFI_STATUS (EFIAPI *EFI_CLOSE_EVENT)(void*);

/* ----------------------------------------------------------------- image    */
/* EFI_EXIT_BOOT_SERVICES typedef is above with the other boot svc ptr types */

/* ---------------------------------------------------------- loaded image  */
typedef struct {
    UINT32 Revision;
    EFI_HANDLE ParentHandle;
    EFI_HANDLE DeviceHandle;     /* handle of device the image was loaded from */
    void *FilePath;             /* EFI_DEVICE_PATH_PROTOCOL* */
    void *Reserved;
    UINT32 LoadOptionsSize;
    void *LoadOptions;
    void *ImageBase;
    UINT64 ImageSize;
    EFI_MEMORY_TYPE ImageCodeType;
    EFI_MEMORY_TYPE ImageDataType;
} EFI_LOADED_IMAGE;

/* ------------------------------------------------------------ file system */
typedef struct EFI_FILE_PROTOCOL EFI_FILE_PROTOCOL;

typedef EFI_STATUS (EFIAPI *EFI_VOLUME_OPEN)(void*, EFI_FILE_PROTOCOL**);
typedef EFI_STATUS (EFIAPI *EFI_FILE_OPEN)(EFI_FILE_PROTOCOL*, EFI_FILE_PROTOCOL**, CHAR16*, UINT64, UINT64);
typedef EFI_STATUS (EFIAPI *EFI_FILE_CLOSE)(EFI_FILE_PROTOCOL*);
typedef EFI_STATUS (EFIAPI *EFI_FILE_READ)(EFI_FILE_PROTOCOL*, UINTN*, void*);
typedef EFI_STATUS (EFIAPI *EFI_FILE_WRITE)(EFI_FILE_PROTOCOL*, UINTN*, void*);
typedef EFI_STATUS (EFIAPI *EFI_FILE_GET_INFO)(EFI_FILE_PROTOCOL*, EFI_GUID*, UINTN*, void*);
typedef EFI_STATUS (EFIAPI *EFI_FILE_SET_POSITION)(EFI_FILE_PROTOCOL*, UINT64);
typedef EFI_STATUS (EFIAPI *EFI_FILE_GET_POSITION)(EFI_FILE_PROTOCOL*, UINT64*);

struct EFI_FILE_PROTOCOL {
    UINT64 Revision;
    EFI_FILE_OPEN Open;
    EFI_FILE_CLOSE Close;
    EFI_FILE_CLOSE Delete;
    EFI_FILE_READ Read;
    EFI_FILE_WRITE Write;
    EFI_FILE_GET_POSITION GetPosition;
    EFI_FILE_SET_POSITION SetPosition;
    EFI_FILE_GET_INFO GetInfo;
    void *SetInfo;
    EFI_FILE_CLOSE Flush;
    /* … more fields we don't use */
};

typedef struct {
    UINT64 Revision;
    EFI_VOLUME_OPEN OpenVolume;
} EFI_SIMPLE_FILE_SYSTEM_PROTOCOL;

/* ------------------------------------------------------------- GOP        */
typedef enum {
    PixelRedGreenBlueReserved8BitPerColor,
    PixelBlueGreenRedReserved8BitPerColor,
    PixelBitMask,
    PixelBltOnly,
    PixelFormatMax
} EFI_PIXEL_FORMAT;

typedef struct {
    UINT32 RedMask;
    UINT32 GreenMask;
    UINT32 BlueMask;
    UINT32 ReservedMask;
} EFI_PIXEL_BITMASK;

typedef struct {
    UINT32 Version;
    UINT32 HorizontalResolution;
    UINT32 VerticalResolution;
    EFI_PIXEL_FORMAT PixelFormat;
    EFI_PIXEL_BITMASK PixelInformation;
    UINT32 PixelsPerScanLine;
} EFI_GRAPHICS_OUTPUT_MODE_INFORMATION;

typedef struct {
    UINT32 MaxMode;
    UINT32 Mode;
    EFI_GRAPHICS_OUTPUT_MODE_INFORMATION *Info;
    UINTN SizeOfInfo;
    EFI_PHYSICAL_ADDRESS FrameBufferBase;
    UINTN FrameBufferSize;
} EFI_GRAPHICS_OUTPUT_PROTOCOL_MODE;

/* `This` is typed `void *` because the protocol struct below is anonymous;
   the ABI is identical either way.  QueryMode allocates *Info from the pool
   and the caller owns it (FreePool). */
typedef EFI_STATUS (EFIAPI *EFI_GRAPHICS_OUTPUT_PROTOCOL_QUERY_MODE)(
    void *This, UINT32 ModeNumber, UINTN *SizeOfInfo,
    EFI_GRAPHICS_OUTPUT_MODE_INFORMATION **Info);

typedef EFI_STATUS (EFIAPI *EFI_GRAPHICS_OUTPUT_PROTOCOL_SET_MODE)(
    void *This, UINT32 ModeNumber);

typedef struct {
    EFI_GRAPHICS_OUTPUT_PROTOCOL_QUERY_MODE QueryMode;
    EFI_GRAPHICS_OUTPUT_PROTOCOL_SET_MODE   SetMode;
    void *Blt;              /* EFI_GRAPHICS_OUTPUT_PROTOCOL_BLT            */
    EFI_GRAPHICS_OUTPUT_PROTOCOL_MODE *Mode;   /* offset 24 */
} EFI_GRAPHICS_OUTPUT_PROTOCOL;

/* ------------------------------------------------------------- boot svc   */
/* Full layout per UEFI 2.10 spec (verified against EDK2 UefiSpec.h).        */
typedef struct {
    EFI_TABLE_HEADER Hdr;
    /* Task Priority */
    void *RaiseTPL;
    void *RestoreTPL;
    /* Memory Services */
    EFI_ALLOCATE_PAGES AllocatePages;
    EFI_FREE_PAGES FreePages;
    EFI_GET_MEMORY_MAP GetMemoryMap;
    EFI_ALLOCATE_POOL AllocatePool;
    EFI_FREE_POOL FreePool;
    /* Event & Timer Services */
    void *CreateEvent;
    void *SetTimer;
    EFI_WAIT_FOR_EVENT WaitForEvent;
    void *SignalEvent;
    void *CloseEvent;
    void *CheckEvent;
    /* Protocol Handler Services */
    void *InstallProtocolInterface;
    void *ReinstallProtocolInterface;
    void *UninstallProtocolInterface;
    EFI_HANDLE_PROTOCOL HandleProtocol;
    void *Reserved;
    void *RegisterProtocolNotify;
    void *LocateHandle;
    void *LocateDevicePath;
    void *InstallConfigurationTable;
    /* Image Services */
    void *LoadImage;
    void *StartImage;
    void *Exit;
    void *UnloadImage;
    EFI_EXIT_BOOT_SERVICES ExitBootServices;
    /* Miscellaneous Services */
    void *GetNextMonotonicCount;
    void *Stall;
    void *SetWatchdogTimer;
    /* DriverSupport Services */
    void *ConnectController;
    void *DisconnectController;
    /* Open and Close Protocol Services */
    void *OpenProtocol;
    void *CloseProtocol;
    void *OpenProtocolInformation;
    /* Library Services */
    void *ProtocolsPerHandle;
    void *LocateHandleBuffer;
    EFI_LOCATE_PROTOCOL LocateProtocol;
    void *InstallMultipleProtocolInterfaces;
    void *UninstallMultipleProtocolInterfaces;
    /* 32-bit CRC Services */
    void *CalculateCrc32;
    /* Miscellaneous Services */
    void *CopyMem;
    void *SetMem;
    void *CreateEventEx;
} EFI_BOOT_SERVICES;

/* ------------------------------------------------------- runtime services  */
typedef struct { EFI_TABLE_HEADER Hdr; /* … */ } EFI_RUNTIME_SERVICES;

/* ---------------------------------------------------------- system table  */
typedef struct {
    EFI_TABLE_HEADER Hdr;
    CHAR16 *FirmwareVendor;
    UINT32 FirmwareRevision;
    EFI_HANDLE ConsoleInHandle;
    EFI_SIMPLE_TEXT_INPUT_PROTOCOL *ConIn;
    EFI_HANDLE ConsoleOutHandle;
    EFI_SIMPLE_TEXT_OUTPUT_PROTOCOL *ConOut;
    EFI_HANDLE StandardErrorHandle;
    EFI_SIMPLE_TEXT_OUTPUT_PROTOCOL *StdErr;
    EFI_RUNTIME_SERVICES *RuntimeServices;
    EFI_BOOT_SERVICES *BootServices;
    UINTN NumberOfTableEntries;
    void *ConfigurationTable;
} EFI_SYSTEM_TABLE;

/* ---------------------------------------------------------- barryOS boot  */
typedef struct {
    UINT64 magic;                  /* 'BAROS' */
    UINT64 framebuffer_addr;
    UINT64 framebuffer_size;
    UINT32 width;
    UINT32 height;
    UINT32 pixels_per_scanline;
    UINT32 pixel_format;           /* EFI_PIXEL_FORMAT */
    EFI_MEMORY_DESCRIPTOR *memmap;
    UINT64 memmap_size;
    UINT64 memmap_desc_size;
    UINT64 memmap_desc_version;
} BootInfo;

#define BARRYOS_BOOTINFO_MAGIC 0x534F52524142ULL   /* 'BARROS' */

#endif /* BARRYOS_EFI_TYPES_H */
