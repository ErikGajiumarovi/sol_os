use core::slice;

pub const SECTOR_SIZE: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageError {
    MissingRamdisk,
    InvalidRamdisk,
    SectorOutOfRange,
}

/// The block-device seam used by the FAT32 layer. The initial implementation
/// reads the FAT image that bootloader loaded before leaving UEFI. A future
/// xHCI mass-storage transport can implement the same trait without changing
/// filesystem or shell code.
pub trait BlockDevice {
    fn sector_count(&self) -> u64;
    fn read_sector(&self, lba: u32, output: &mut [u8; SECTOR_SIZE]) -> Result<(), StorageError>;
}

pub struct RamDiskBlockDevice {
    bytes: &'static [u8],
}

impl RamDiskBlockDevice {
    /// # Safety
    ///
    /// `address..address + length` must be the immutable virtual mapping
    /// supplied by bootloader's `BootInfo.ramdisk_*` fields for this boot.
    pub unsafe fn from_boot_info(address: Option<u64>, length: u64) -> Result<Self, StorageError> {
        let address = address.ok_or(StorageError::MissingRamdisk)?;
        let length = usize::try_from(length).map_err(|_| StorageError::InvalidRamdisk)?;
        if length == 0 || length % SECTOR_SIZE != 0 {
            return Err(StorageError::InvalidRamdisk);
        }

        // SAFETY: bootloader owns the mapping lifetime and guarantees it stays
        // valid for the kernel. We expose it read-only through `BlockDevice`.
        let bytes = unsafe { slice::from_raw_parts(address as *const u8, length) };
        Ok(Self { bytes })
    }
}

impl BlockDevice for RamDiskBlockDevice {
    fn sector_count(&self) -> u64 {
        (self.bytes.len() / SECTOR_SIZE) as u64
    }

    fn read_sector(&self, lba: u32, output: &mut [u8; SECTOR_SIZE]) -> Result<(), StorageError> {
        let start = (lba as usize)
            .checked_mul(SECTOR_SIZE)
            .ok_or(StorageError::SectorOutOfRange)?;
        let end = start
            .checked_add(SECTOR_SIZE)
            .ok_or(StorageError::SectorOutOfRange)?;
        let source = self
            .bytes
            .get(start..end)
            .ok_or(StorageError::SectorOutOfRange)?;
        output.copy_from_slice(source);
        Ok(())
    }
}
