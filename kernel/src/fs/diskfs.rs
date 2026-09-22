//! barryFS — the on-disk filesystem.
//!
//! Self-developed, and deliberately small: a superblock, a fixed inode table,
//! a data bitmap and a run of 4 KiB data blocks.  Each file occupies one
//! contiguous run of blocks, which is a poor choice for a general-purpose
//! filesystem and a perfectly good one for a few dozen small files — it makes
//! both allocation and the read path trivial.
//!
//! The kernel works on the RAM filesystem (`vfs` + `ramfs`) and this module
//! moves whole filesystems between that and the disk:
//!
//!   boot from disk  → `load`  builds the VFS from the inode table
//!   `sync` / install → `save` writes it back
//!
//! That means a file survives a reboot, but it is *not* a block-level
//! filesystem: a write rewrites the image rather than the blocks it touched.

use crate::dev::ata;
use crate::fs::{perm, ramfs, vfs};
use crate::serial;

/// Where the filesystem starts on the disk, in sectors.  1 MiB in, which
/// leaves the boot chain (MBR + stage2 + kernel) plenty of room below it.
pub const FS_LBA: u32 = 2048;
pub const BLOCK_SIZE: usize = 4096;
/// 1024 blocks = 4 MiB, and 8 sectors per block.
pub const BLOCK_COUNT: u32 = 1024;
pub const SECTORS_PER_BLOCK: u8 = (BLOCK_SIZE / ata::SECTOR_SIZE) as u8;

pub const MAGIC: u64 = 0x3146_5952_5241_42;  // "BARRYF1\0" little-endian-ish

/// Fixed-size inode record.  64 bytes, so 64 of them fill exactly one block.
const INODE_SIZE: usize = 64;
const INODE_COUNT: u32 = (BLOCK_SIZE / INODE_SIZE) as u32;   // 64, matches VFS
const INODE_TABLE_BLOCK: u32 = 1;
const DATA_BITMAP_BLOCK: u32 = 2;
const DATA_START_BLOCK: u32 = 3;

#[repr(C)]
#[derive(Clone, Copy)]
struct Superblock {
    magic: u64,
    version: u32,
    block_size: u32,
    inode_count: u32,
    inode_table_block: u32,
    data_bitmap_block: u32,
    data_start_block: u32,
    total_blocks: u32,
    root_inode: u32,
    /// Number of filesystem writes, so a mount can tell a repaired image from
    /// a fresh one.  Not a journal — just a sanity counter.
    generation: u32,
}

impl Superblock {
    const fn blank() -> Self {
        Self {
            magic: 0, version: 0, block_size: 0, inode_count: 0,
            inode_table_block: 0, data_bitmap_block: 0, data_start_block: 0,
            total_blocks: 0, root_inode: 0, generation: 0,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct DiskInode {
    used: u8,
    vtype: u8,      // 0 = file, 1 = directory
    name_len: u8,
    _pad0: u8,
    mode: u16,
    _pad1: u16,
    uid: u32,
    gid: u32,
    size: u32,
    parent: u32,        // disk inode index, 0 for the root
    first_block: u32,   // block index relative to the filesystem start
    block_count: u32,
    name: [u8; 32],
}

impl DiskInode {
    const fn blank() -> Self {
        Self {
            used: 0, vtype: 0, name_len: 0, _pad0: 0,
            mode: 0, _pad1: 0, uid: 0, gid: 0, size: 0,
            parent: 0, first_block: 0, block_count: 0,
            name: [0; 32],
        }
    }
    fn name_str(&self) -> &str {
        let n = (self.name_len as usize).min(32);
        core::str::from_utf8(&self.name[..n]).unwrap_or("?")
    }
}

/// Read one 4 KiB block into `buf`.
fn read_block(drive: usize, block: u32, buf: &mut [u8; BLOCK_SIZE]) -> bool {
    if block >= BLOCK_COUNT {
        return false;
    }
    ata::read_sectors(drive, FS_LBA + block * SECTORS_PER_BLOCK as u32, SECTORS_PER_BLOCK, buf)
}

/// Write one 4 KiB block.
fn write_block(drive: usize, block: u32, buf: &[u8; BLOCK_SIZE]) -> bool {
    if block >= BLOCK_COUNT {
        return false;
    }
    ata::write_sectors(drive, FS_LBA + block * SECTORS_PER_BLOCK as u32, SECTORS_PER_BLOCK, buf)
}

fn read_superblock(drive: usize) -> Option<Superblock> {
    let mut buf = [0u8; BLOCK_SIZE];
    if !read_block(drive, 0, &mut buf) {
        return None;
    }
    // The struct is a plain sequence of integers; reading it back field by
    // field keeps this independent of any padding the compiler chose.
    let u32_at = |off: usize| -> u32 {
        u32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]])
    };
    let magic = u64::from_le_bytes([
        buf[0], buf[1], buf[2], buf[3], buf[4], buf[5], buf[6], buf[7],
    ]);
    if magic != MAGIC {
        return None;
    }
    Some(Superblock {
        magic,
        version: u32_at(8),
        block_size: u32_at(12),
        inode_count: u32_at(16),
        inode_table_block: u32_at(20),
        data_bitmap_block: u32_at(24),
        data_start_block: u32_at(28),
        total_blocks: u32_at(32),
        root_inode: u32_at(36),
        generation: u32_at(40),
    })
}

fn write_superblock(drive: usize, sb: &Superblock) -> bool {
    let mut buf = [0u8; BLOCK_SIZE];
    buf[0..8].copy_from_slice(&sb.magic.to_le_bytes());
    buf[8..12].copy_from_slice(&sb.version.to_le_bytes());
    buf[12..16].copy_from_slice(&sb.block_size.to_le_bytes());
    buf[16..20].copy_from_slice(&sb.inode_count.to_le_bytes());
    buf[20..24].copy_from_slice(&sb.inode_table_block.to_le_bytes());
    buf[24..28].copy_from_slice(&sb.data_bitmap_block.to_le_bytes());
    buf[28..32].copy_from_slice(&sb.data_start_block.to_le_bytes());
    buf[32..36].copy_from_slice(&sb.total_blocks.to_le_bytes());
    buf[36..40].copy_from_slice(&sb.root_inode.to_le_bytes());
    buf[40..44].copy_from_slice(&sb.generation.to_le_bytes());
    write_block(drive, 0, &buf)
}

/// Serialise an inode.  `out` must be exactly `INODE_SIZE` bytes; taking a
/// slice rather than an array reference lets callers pass a slice of the
/// inode block without a copy.
fn pack_inode(d: &DiskInode, out: &mut [u8]) {
    let out = &mut out[..INODE_SIZE];
    out.fill(0);
    out[0] = d.used;
    out[1] = d.vtype;
    out[2] = d.name_len;
    out[4..6].copy_from_slice(&d.mode.to_le_bytes());
    out[8..12].copy_from_slice(&d.uid.to_le_bytes());
    out[12..16].copy_from_slice(&d.gid.to_le_bytes());
    out[16..20].copy_from_slice(&d.size.to_le_bytes());
    out[20..24].copy_from_slice(&d.parent.to_le_bytes());
    out[24..28].copy_from_slice(&d.first_block.to_le_bytes());
    out[28..32].copy_from_slice(&d.block_count.to_le_bytes());
    out[32..64].copy_from_slice(&d.name);
}

fn unpack_inode(src: &[u8]) -> DiskInode {
    let u16_at = |o: usize| u16::from_le_bytes([src[o], src[o + 1]]);
    let u32_at = |o: usize| u32::from_le_bytes([src[o], src[o + 1], src[o + 2], src[o + 3]]);
    let mut name = [0u8; 32];
    name.copy_from_slice(&src[32..64]);
    DiskInode {
        used: src[0],
        vtype: src[1],
        name_len: src[2],
        _pad0: 0,
        mode: u16_at(4),
        _pad1: 0,
        uid: u32_at(8),
        gid: u32_at(12),
        size: u32_at(16),
        parent: u32_at(20),
        first_block: u32_at(24),
        block_count: u32_at(28),
        name,
    }
}

/// Has a barryFS been installed at `FS_LBA` on this drive?
pub fn is_present(drive: usize) -> bool {
    read_superblock(drive).is_some()
}

/// Lay down an empty filesystem: superblock, an inode table holding just the
/// root, and a data bitmap with nothing allocated.
pub fn format(drive: usize) -> bool {
    serial::print_str("[barryfs] formatting drive ");
    serial::print_hex(drive as u64);
    serial::print_str("\n");

    let mut zero = [0u8; BLOCK_SIZE];

    // Inode table: root only.
    let mut root = DiskInode::blank();
    root.used = 1;
    root.vtype = vfs::VnodeType::Dir as u8;
    root.mode = perm::MODE_DIR;
    root.uid = perm::UID_ROOT;
    root.gid = perm::GID_ROOT;
    root.name[0] = b'/';
    root.name_len = 1;
    pack_inode(&root, &mut zero[0..INODE_SIZE]);
    if !write_block(drive, INODE_TABLE_BLOCK, &zero) {
        return false;
    }

    // Data bitmap: all free.
    let mut bitmap = [0u8; BLOCK_SIZE];
    // The blocks before the data area are not data, but nothing allocates them
    // either; leave the bitmap describing the data area only.
    if !write_block(drive, DATA_BITMAP_BLOCK, &bitmap) {
        return false;
    }
    let _ = &mut bitmap;

    let sb = Superblock {
        magic: MAGIC,
        version: 1,
        block_size: BLOCK_SIZE as u32,
        inode_count: INODE_COUNT,
        inode_table_block: INODE_TABLE_BLOCK,
        data_bitmap_block: DATA_BITMAP_BLOCK,
        data_start_block: DATA_START_BLOCK,
        total_blocks: BLOCK_COUNT,
        root_inode: 0,
        generation: 0,
    };
    write_superblock(drive, &sb)
}

/// Walk the VFS breadth-first so that a parent is always written before its
/// children — the loader depends on that order to relink them.
fn collect_order(out: &mut [u64; vfs::MAX_VNODES]) -> usize {
    let mut queue = [0u64; vfs::MAX_VNODES];
    let mut head = 0usize;
    let mut tail = 0usize;
    let mut n = 0usize;

    queue[tail] = vfs::ROOT_ID;
    tail += 1;

    while head < tail && n < out.len() {
        let id = queue[head];
        head += 1;
        out[n] = id;
        n += 1;

        let mut entries = [vfs::DirEntry::empty(); vfs::MAX_CHILDREN];
        let c = vfs::read_dir(id, &mut entries);
        for e in entries.iter().take(c) {
            if tail < queue.len() {
                queue[tail] = e.id;
                tail += 1;
            }
        }
    }
    n
}

/// Write the in-memory filesystem to disk.
pub fn save(drive: usize) -> bool {
    let mut order = [0u64; vfs::MAX_VNODES];
    let n = collect_order(&mut order);

    let mut inode_block = [0u8; BLOCK_SIZE];
    let mut data_bitmap = [0u8; BLOCK_SIZE];
    let mut next_data_block: u32 = 0;      // relative to DATA_START_BLOCK

    // Map vnode id → disk inode index, so children can record their parent.
    let mut index_of = [u32::MAX; vfs::MAX_VNODES];
    let mut vnode_of_slot = [0u64; vfs::MAX_VNODES];

    for (slot, &id) in order.iter().enumerate().take(n) {
        let Some(vinfo) = vfs::get_type(id) else { continue };
        let p = vfs::get_perm(id).unwrap_or(vfs::Perm { uid: 0, gid: 0, mode: 0 });

        let mut d = DiskInode::blank();
        d.used = 1;
        d.vtype = match vinfo {
            vfs::VnodeType::Dir => 1,
            _ => 0,
        };
        d.mode = p.mode;
        d.uid = p.uid;
        d.gid = p.gid;
        let name = {
            let mut nb = [0u8; vfs::MAX_NAME + 1];
            let nl = vfs::get_name(id, &mut nb);
            let mut out = [0u8; 32];
            out[..nl].copy_from_slice(&nb[..nl]);
            (out, nl)
        };
        d.name = name.0;
        d.name_len = name.1 as u8;

        // Root has no parent; everything else points at the slot its parent
        // was written to.
        let parent_id = vfs::get_parent(id);
        d.parent = if parent_id == 0 {
            0
        } else {
            match index_of.iter().position(|&x| x != u32::MAX
                && vnode_of_slot[x as usize] == parent_id)
            {
                Some(s) => s as u32,
                None => 0,
            }
        };

        // File contents, one contiguous run of blocks.
        if d.vtype == 0 {
            let size = vfs::get_size(id) as usize;
            d.size = size as u32;
            if size > 0 {
                let blocks = size.div_ceil(BLOCK_SIZE);
                if next_data_block as usize + blocks > (BLOCK_COUNT - DATA_START_BLOCK) as usize {
                    serial::print_str("[barryfs] out of data blocks\n");
                    return false;
                }
                d.first_block = DATA_START_BLOCK + next_data_block;
                d.block_count = blocks as u32;

                let mut buf = [0u8; BLOCK_SIZE];
                let mut off = 0usize;
                for b in 0..blocks {
                    buf.fill(0);
                    let chunk = BLOCK_SIZE.min(size - off);
                    if ramfs::read_file_at(id, off as u64, &mut buf[..chunk]) != chunk {
                        serial::print_str("[barryfs] short read while saving\n");
                        return false;
                    }
                    if !write_block(drive, d.first_block + b as u32, &buf) {
                        return false;
                    }
                    off += chunk;
                }
                // Mark the data blocks used in the bitmap.
                for b in 0..blocks {
                    let bit = next_data_block as usize + b;
                    data_bitmap[bit / 8] |= 1 << (bit % 8);
                }
                next_data_block += blocks as u32;
            }
        }

        index_of[slot] = slot as u32;
        vnode_of_slot[slot] = id;
        pack_inode(&d, &mut inode_block[slot * INODE_SIZE..(slot + 1) * INODE_SIZE]);
    }

    // Inode table, then the bitmap, then the superblock — superblock last, so
    // an interrupted save leaves the old one pointing at a consistent image.
    if !write_block(drive, INODE_TABLE_BLOCK, &inode_block) {
        return false;
    }
    if !write_block(drive, DATA_BITMAP_BLOCK, &data_bitmap) {
        return false;
    }

    let generation = read_superblock(drive).map(|s| s.generation).unwrap_or(0);
    let sb = Superblock {
        magic: MAGIC,
        version: 1,
        block_size: BLOCK_SIZE as u32,
        inode_count: INODE_COUNT,
        inode_table_block: INODE_TABLE_BLOCK,
        data_bitmap_block: DATA_BITMAP_BLOCK,
        data_start_block: DATA_START_BLOCK,
        total_blocks: BLOCK_COUNT,
        root_inode: 0,
        generation: generation.wrapping_add(1),
    };
    if !write_superblock(drive, &sb) {
        return false;
    }

    serial::print_str("[barryfs] saved ");
    serial::print_hex(n as u64);
    serial::print_str(" entries, ");
    serial::print_hex(next_data_block as u64);
    serial::print_str(" data blocks\n");
    true
}

/// Replace the in-memory filesystem with the one on disk.
pub fn load(drive: usize) -> bool {
    let Some(sb) = read_superblock(drive) else {
        serial::print_str("[barryfs] no filesystem on this drive\n");
        return false;
    };
    if sb.block_size as usize != BLOCK_SIZE || sb.version != 1 {
        serial::print_str("[barryfs] unsupported filesystem version\n");
        return false;
    }

    let mut inode_block = [0u8; BLOCK_SIZE];
    if !read_block(drive, sb.inode_table_block, &mut inode_block) {
        return false;
    }

    // Start from an empty tree and rebuild it in inode order.  Parents come
    // first because the filesystem was written breadth-first.
    vfs::reset();

    let mut id_of_index = [0u64; INODE_COUNT as usize];
    let mut loaded = 0usize;

    for i in 0..sb.inode_count as usize {
        let d = unpack_inode(&inode_block[i * INODE_SIZE..(i + 1) * INODE_SIZE]);
        if d.used == 0 {
            continue;
        }

        if i as u32 == sb.root_inode {
            vfs::set_owner(vfs::ROOT_ID, d.uid, d.gid);
            vfs::set_mode(vfs::ROOT_ID, d.mode);
            id_of_index[i] = vfs::ROOT_ID;
            loaded += 1;
            continue;
        }

        let parent = if d.parent as usize >= id_of_index.len() {
            continue;
        } else {
            id_of_index[d.parent as usize]
        };
        if parent == 0 {
            serial::print_str("[barryfs] orphan inode, skipped\n");
            continue;
        }
        let name = d.name_str();
        if name.is_empty() {
            continue;
        }

        let id = if d.vtype == 1 {
            vfs::create_dir(parent, name)
        } else {
            vfs::create_file(parent, name)
        };
        if id == 0 {
            continue;
        }

        if d.vtype == 0 && d.size > 0 {
            let mut buf = [0u8; vfs::MAX_FILE_SIZE];
            let want = (d.size as usize).min(buf.len());
            let mut off = 0usize;
            let mut block = [0u8; BLOCK_SIZE];
            while off < want {
                if !read_block(drive, d.first_block + (off / BLOCK_SIZE) as u32, &mut block) {
                    break;
                }
                let chunk = BLOCK_SIZE.min(want - off);
                buf[off..off + chunk].copy_from_slice(&block[..chunk]);
                off += chunk;
            }
            ramfs::write_file(id, &buf[..want]);
        }

        vfs::set_owner(id, d.uid, d.gid);
        vfs::set_mode(id, d.mode);
        id_of_index[i] = id;
        loaded += 1;
    }

    serial::print_str("[barryfs] loaded ");
    serial::print_hex(loaded as u64);
    serial::print_str(" entries from drive ");
    serial::print_hex(drive as u64);
    serial::print_str(", generation ");
    serial::print_hex(sb.generation as u64);
    serial::print_str("\n");
    true
}
