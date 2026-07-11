use core::cmp::min;
use core::str;

use crate::storage::{BlockDevice, SECTOR_SIZE, StorageError};

const FAT32_EOC: u32 = 0x0fff_fff8;
const FAT32_BAD_CLUSTER: u32 = 0x0fff_fff7;
const ATTR_DIRECTORY: u8 = 0x10;
const ATTR_VOLUME_ID: u8 = 0x08;
const ATTR_LONG_FILE_NAME: u8 = 0x0f;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsError {
    Device(StorageError),
    InvalidBootSector,
    UnsupportedVolume,
    InvalidCluster,
    CorruptFat,
    DirectoryLoop,
    NotFound,
    NotDirectory,
    IsDirectory,
    TruncatedFile,
}

impl From<StorageError> for FsError {
    fn from(error: StorageError) -> Self {
        Self::Device(error)
    }
}

/// A compact, read-only FAT32 implementation for the OS data volume.
///
/// It deliberately supports the subset the shell needs: 512-byte sectors,
/// FAT32 cluster chains, 8.3 entries, nested directories, and streamed files.
/// Long names and write support are omitted rather than silently mishandled.
pub struct Fat32<'a, D: BlockDevice> {
    device: &'a D,
    sectors_per_cluster: u32,
    fat_start_sector: u32,
    sectors_per_fat: u32,
    first_data_sector: u32,
    root_cluster: u32,
    cluster_count: u32,
}

#[derive(Clone, Copy)]
pub struct DirEntry {
    name: [u8; 12],
    name_len: u8,
    attributes: u8,
    first_cluster: u32,
    size: u32,
}

impl DirEntry {
    pub fn name(&self) -> &str {
        // Names are assembled only from an 8.3 entry's ASCII byte fields. The
        // image builder rejects non-UTF-8 host filenames, so this is valid UTF-8.
        unsafe { str::from_utf8_unchecked(&self.name[..self.name_len as usize]) }
    }

    pub fn is_directory(&self) -> bool {
        self.attributes & ATTR_DIRECTORY != 0
    }

    pub fn size(&self) -> u32 {
        self.size
    }
}

impl<'a, D: BlockDevice> Fat32<'a, D> {
    pub fn mount(device: &'a D) -> Result<Self, FsError> {
        let mut sector = [0; SECTOR_SIZE];
        device.read_sector(0, &mut sector)?;

        if sector[510] != 0x55 || sector[511] != 0xaa {
            return Err(FsError::InvalidBootSector);
        }
        let bytes_per_sector = le_u16(&sector, 11);
        let sectors_per_cluster = sector[13] as u32;
        let reserved_sectors = le_u16(&sector, 14) as u32;
        let fat_count = sector[16] as u32;
        let root_entry_count = le_u16(&sector, 17);
        let total_sectors_16 = le_u16(&sector, 19) as u32;
        let fat_size_16 = le_u16(&sector, 22) as u32;
        let total_sectors_32 = le_u32(&sector, 32);
        let sectors_per_fat = le_u32(&sector, 36);
        let root_cluster = le_u32(&sector, 44);

        if bytes_per_sector != SECTOR_SIZE as u16
            || sectors_per_cluster == 0
            || !sectors_per_cluster.is_power_of_two()
            || reserved_sectors == 0
            || fat_count == 0
            || fat_size_16 != 0
            || sectors_per_fat == 0
            || root_entry_count != 0
            || root_cluster < 2
        {
            return Err(FsError::UnsupportedVolume);
        }

        let total_sectors = if total_sectors_16 != 0 {
            total_sectors_16
        } else {
            total_sectors_32
        };
        let first_data_sector = reserved_sectors
            .checked_add(
                fat_count
                    .checked_mul(sectors_per_fat)
                    .ok_or(FsError::InvalidBootSector)?,
            )
            .ok_or(FsError::InvalidBootSector)?;
        if total_sectors <= first_data_sector || total_sectors as u64 > device.sector_count() {
            return Err(FsError::InvalidBootSector);
        }

        let cluster_count = (total_sectors - first_data_sector) / sectors_per_cluster;
        if cluster_count < 65_525 || root_cluster >= cluster_count + 2 {
            return Err(FsError::UnsupportedVolume);
        }
        let fat_entries = (sectors_per_fat as u64 * SECTOR_SIZE as u64) / 4;
        if root_cluster as u64 >= fat_entries {
            return Err(FsError::InvalidBootSector);
        }

        Ok(Self {
            device,
            sectors_per_cluster,
            fat_start_sector: reserved_sectors,
            sectors_per_fat,
            first_data_sector,
            root_cluster,
            cluster_count,
        })
    }

    pub fn list_dir<F>(&self, path: &str, visitor: F) -> Result<(), FsError>
    where
        F: FnMut(DirEntry) -> bool,
    {
        let cluster = if path.trim_matches('/').is_empty() {
            self.root_cluster
        } else {
            let entry = self.resolve_path(path)?;
            if !entry.is_directory() {
                return Err(FsError::NotDirectory);
            }
            entry.first_cluster
        };
        self.visit_directory(cluster, visitor)
    }

    pub fn read_file<F>(&self, path: &str, mut visitor: F) -> Result<usize, FsError>
    where
        F: FnMut(&[u8]),
    {
        let entry = self.resolve_path(path)?;
        if entry.is_directory() {
            return Err(FsError::IsDirectory);
        }
        if entry.size == 0 {
            return Ok(0);
        }
        if !self.is_valid_cluster(entry.first_cluster) {
            return Err(FsError::InvalidCluster);
        }

        let mut remaining = entry.size as usize;
        let mut cluster = entry.first_cluster;
        let mut sector = [0; SECTOR_SIZE];
        for _ in 0..=self.cluster_count {
            for sector_index in 0..self.sectors_per_cluster {
                if remaining == 0 {
                    return Ok(entry.size as usize);
                }
                let lba = self.cluster_to_sector(cluster, sector_index)?;
                self.device.read_sector(lba, &mut sector)?;
                let count = min(remaining, SECTOR_SIZE);
                visitor(&sector[..count]);
                remaining -= count;
            }

            if remaining == 0 {
                return Ok(entry.size as usize);
            }
            let next = self.next_cluster(cluster)?;
            if next >= FAT32_EOC {
                return Err(FsError::TruncatedFile);
            }
            cluster = self.checked_next_cluster(next)?;
        }
        Err(FsError::DirectoryLoop)
    }

    pub fn root_cluster(&self) -> u32 {
        self.root_cluster
    }

    fn resolve_path(&self, path: &str) -> Result<DirEntry, FsError> {
        let mut components = path.split('/').filter(|component| !component.is_empty());
        let first = components.next().ok_or(FsError::NotFound)?;
        if first == "." || first == ".." {
            return Err(FsError::NotFound);
        }

        let mut directory = self.root_cluster;
        let mut component = first;
        loop {
            let entry = self.find_in_directory(directory, component)?;
            match components.next() {
                Some(next_component) => {
                    if !entry.is_directory() {
                        return Err(FsError::NotDirectory);
                    }
                    if next_component == "." || next_component == ".." {
                        return Err(FsError::NotFound);
                    }
                    directory = entry.first_cluster;
                    component = next_component;
                }
                None => return Ok(entry),
            }
        }
    }

    fn find_in_directory(&self, cluster: u32, name: &str) -> Result<DirEntry, FsError> {
        let mut result = None;
        self.visit_directory(cluster, |entry| {
            if entry.name().eq_ignore_ascii_case(name) {
                result = Some(entry);
                false
            } else {
                true
            }
        })?;
        result.ok_or(FsError::NotFound)
    }

    fn visit_directory<F>(&self, start_cluster: u32, mut visitor: F) -> Result<(), FsError>
    where
        F: FnMut(DirEntry) -> bool,
    {
        if !self.is_valid_cluster(start_cluster) {
            return Err(FsError::InvalidCluster);
        }

        let mut cluster = start_cluster;
        let mut sector = [0; SECTOR_SIZE];
        for _ in 0..=self.cluster_count {
            for sector_index in 0..self.sectors_per_cluster {
                let lba = self.cluster_to_sector(cluster, sector_index)?;
                self.device.read_sector(lba, &mut sector)?;
                for raw in sector.chunks_exact(32) {
                    if raw[0] == 0x00 {
                        return Ok(());
                    }
                    if raw[0] == 0xe5
                        || raw[0] == b'.'
                        || raw[11] == ATTR_LONG_FILE_NAME
                        || raw[11] & ATTR_VOLUME_ID != 0
                    {
                        continue;
                    }
                    if let Some(entry) = DirEntry::from_raw(raw) {
                        if !visitor(entry) {
                            return Ok(());
                        }
                    }
                }
            }

            let next = self.next_cluster(cluster)?;
            if next >= FAT32_EOC {
                return Ok(());
            }
            cluster = self.checked_next_cluster(next)?;
        }
        Err(FsError::DirectoryLoop)
    }

    fn next_cluster(&self, cluster: u32) -> Result<u32, FsError> {
        if !self.is_valid_cluster(cluster) {
            return Err(FsError::InvalidCluster);
        }
        let fat_offset = (cluster as u64).checked_mul(4).ok_or(FsError::CorruptFat)?;
        let sector_offset = fat_offset / SECTOR_SIZE as u64;
        if sector_offset >= self.sectors_per_fat as u64 {
            return Err(FsError::CorruptFat);
        }
        let entry_offset = (fat_offset % SECTOR_SIZE as u64) as usize;
        let lba = self
            .fat_start_sector
            .checked_add(sector_offset as u32)
            .ok_or(FsError::CorruptFat)?;
        let mut sector = [0; SECTOR_SIZE];
        self.device.read_sector(lba, &mut sector)?;
        Ok(le_u32(&sector, entry_offset) & 0x0fff_ffff)
    }

    fn checked_next_cluster(&self, cluster: u32) -> Result<u32, FsError> {
        if cluster == FAT32_BAD_CLUSTER || !self.is_valid_cluster(cluster) {
            Err(FsError::CorruptFat)
        } else {
            Ok(cluster)
        }
    }

    fn cluster_to_sector(&self, cluster: u32, sector_index: u32) -> Result<u32, FsError> {
        if !self.is_valid_cluster(cluster) || sector_index >= self.sectors_per_cluster {
            return Err(FsError::InvalidCluster);
        }
        self.first_data_sector
            .checked_add(
                (cluster - 2)
                    .checked_mul(self.sectors_per_cluster)
                    .ok_or(FsError::InvalidCluster)?,
            )
            .and_then(|sector| sector.checked_add(sector_index))
            .ok_or(FsError::InvalidCluster)
    }

    fn is_valid_cluster(&self, cluster: u32) -> bool {
        cluster >= 2 && cluster < self.cluster_count + 2
    }
}

impl DirEntry {
    fn from_raw(raw: &[u8]) -> Option<Self> {
        let mut name = [0; 12];
        let mut name_len = 0;
        for &byte in &raw[..8] {
            if byte == b' ' {
                break;
            }
            if !byte.is_ascii() {
                return None;
            }
            name[name_len] = byte;
            name_len += 1;
        }
        if name_len == 0 {
            return None;
        }

        let extension = &raw[8..11];
        if extension.iter().any(|&byte| byte != b' ') {
            name[name_len] = b'.';
            name_len += 1;
            for &byte in extension {
                if byte == b' ' {
                    break;
                }
                if !byte.is_ascii() {
                    return None;
                }
                name[name_len] = byte;
                name_len += 1;
            }
        }

        let first_cluster = ((le_u16(raw, 20) as u32) << 16) | le_u16(raw, 26) as u32;
        Some(Self {
            name,
            name_len: name_len as u8,
            attributes: raw[11],
            first_cluster,
            size: le_u32(raw, 28),
        })
    }
}

fn le_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

fn le_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}
