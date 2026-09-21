//! EFI memory descriptor types (matches UEFI 2.10 spec, see DECISIONS D11).
//!
//! These are the same constants the UEFI firmware uses in its memory map.
//! We reproduce them here so the kernel can interpret the BootInfo memmap
//! without depending on edk2 headers.

/// Memory type constants from the UEFI spec (EFI_MEMORY_DESCRIPTOR.Type).
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryType {
    Reserved              = 0,
    LoaderCode            = 1,
    LoaderData            = 2,
    BootServicesCode      = 3,
    BootServicesData      = 4,
    RuntimeServicesCode   = 5,
    RuntimeServicesData   = 6,
    ConventionalMemory    = 7,
    UnusableMemory        = 8,
    AcpiReclaimMemory     = 9,
    AcpiMemoryNvs         = 10,
    MemoryMappedIo        = 11,
    MemoryMappedIoPortSpace = 12,
    PalCode               = 13,
    PersistentMemory      = 14,
}

impl MemoryType {
    pub fn from_u32(v: u32) -> Self {
        match v {
            0 => Self::Reserved,
            1 => Self::LoaderCode,
            2 => Self::LoaderData,
            3 => Self::BootServicesCode,
            4 => Self::BootServicesData,
            5 => Self::RuntimeServicesCode,
            6 => Self::RuntimeServicesData,
            7 => Self::ConventionalMemory,
            8 => Self::UnusableMemory,
            9 => Self::AcpiReclaimMemory,
            10 => Self::AcpiMemoryNvs,
            11 => Self::MemoryMappedIo,
            12 => Self::MemoryMappedIoPortSpace,
            13 => Self::PalCode,
            14 => Self::PersistentMemory,
            _ => Self::Reserved,
        }
    }

    /// Human-readable short name for diagnostics.
    pub fn name(self) -> &'static str {
        match self {
            Self::Reserved                => "RSDV",
            Self::LoaderCode              => "LDRC",
            Self::LoaderData              => "LDRD",
            Self::BootServicesCode        => "BTSC",
            Self::BootServicesData        => "BTSD",
            Self::RuntimeServicesCode     => "RTSC",
            Self::RuntimeServicesData     => "RTSD",
            Self::ConventionalMemory      => "FREE",
            Self::UnusableMemory          => "BAD ",
            Self::AcpiReclaimMemory       => "ACPI",
            Self::AcpiMemoryNvs           => "NVS ",
            Self::MemoryMappedIo          => "MMIO",
            Self::MemoryMappedIoPortSpace => "MMIP",
            Self::PalCode                 => "PAL ",
            Self::PersistentMemory        => "PMEM",
        }
    }

    /// True if this memory is usable for the frame allocator.
    /// We only use ConventionalMemory (free RAM reported by UEFI).
    /// BootServices regions *become* free after ExitBootServices, but
    /// sticking to ConventionalMemory is safer for Stage 2.
    pub fn is_usable(self) -> bool {
        matches!(self, Self::ConventionalMemory)
    }
}

/// EFI_MEMORY_DESCRIPTOR — matches UEFI spec exactly (40 bytes).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct EfiMemoryDescriptor {
    pub memory_type:      u32,
    pub _pad:             u32,
    pub physical_start:   u64,
    pub virtual_start:    u64,
    pub number_of_pages:  u64,
    pub attribute:        u64,
}

impl EfiMemoryDescriptor {
    pub fn region_type(&self) -> MemoryType {
        MemoryType::from_u32(self.memory_type)
    }

    pub fn end(&self) -> u64 {
        self.physical_start + self.number_of_pages * 4096
    }
}

pub const PAGE_SIZE: usize = 4096;
pub const PAGE_SHIFT: u32 = 12;
