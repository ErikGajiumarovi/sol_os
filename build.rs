use std::collections::BTreeMap;
use std::convert::TryFrom;
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use bootloader::{BootConfig, DiskImageBuilder};
use fatfs_sfn::{FatType, FileSystem, FormatVolumeOptions, FsOptions};
use fscommon::StreamSlice;
use gpt::disk::LogicalBlockSize;

const MIB: u64 = 1024 * 1024;
const SECTOR_SIZE: u64 = 512;
const PARTITION_ALIGNMENT_LBAS: u64 = MIB / SECTOR_SIZE;
const DATA_PARTITION_BYTES: u64 = 64 * MIB;

fn main() {
    // The kernel is a Cargo artifact dependency. Rebuild the disk image when a
    // feature selects one of its intentional exception-test variants; otherwise
    // Cargo can correctly rebuild the kernel while leaving a stale image behind.
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_FAULT_TEST_DOUBLE");
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_FAULT_TEST_GPF");
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_FAULT_TEST_PAGE");

    let kernel = PathBuf::from(
        env::var_os("CARGO_BIN_FILE_KERNEL_kernel")
            .expect("Cargo did not expose the kernel artifact path"),
    );
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is not set"));
    let base_image = out_dir.join("bootloader-base.img");
    let data_volume = out_dir.join("sol-data.fat");
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());

    let data_files = manifest_dir.join("disk_files");
    // Cargo does not notice edits to a file merely because its containing
    // directory was watched. Register every current entry (and each directory
    // for newly added entries) so `make image` cannot silently retain an old
    // FAT32 payload after a host file changes.
    watch_host_tree(&data_files).expect("failed to watch disk_files");
    build_data_volume(&data_volume, &data_files).expect("failed to build the FAT32 data volume");

    let mut boot_config = BootConfig::default();
    boot_config.frame_buffer.minimum_framebuffer_width = Some(1024);
    boot_config.frame_buffer.minimum_framebuffer_height = Some(768);
    boot_config.frame_buffer_logging = false;
    boot_config.serial_logging = false;

    let mut builder = DiskImageBuilder::new(kernel);
    builder.set_boot_config(&boot_config);
    // The UEFI bootloader fork reads the `sol-data` partition through firmware
    // Block I/O at boot. Do not embed this volume as a ramdisk in the ESP: the
    // runtime proof must come from the real GPT partition on the USB image.
    builder
        .create_uefi_image(&base_image)
        .expect("failed to create the bootloader UEFI image");

    let build_dir = manifest_dir.join("build");
    fs::create_dir_all(&build_dir).expect("failed to create build directory");
    let final_image = build_dir.join("sol-os.img");

    assemble_two_partition_image(&base_image, &final_image, &data_volume)
        .expect("failed to assemble the final GPT/FAT32 disk image");

    println!("cargo:rustc-env=SOL_OS_IMAGE={}", final_image.display());
    println!("cargo:warning=bootable image: {}", final_image.display());
}

fn assemble_two_partition_image(
    base_image: &Path,
    output_image: &Path,
    data_volume: &Path,
) -> io::Result<()> {
    let (boot_partition_bytes, boot_partition_size) = extract_efi_partition(base_image)?;
    let disk_size = MIB + align_up(boot_partition_size, MIB) + MIB + DATA_PARTITION_BYTES + MIB;

    let disk = OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(output_image)?;
    disk.set_len(disk_size)?;

    let mut disk = Box::new(disk);
    let last_lba = u32::try_from((disk_size / SECTOR_SIZE) - 1).unwrap_or(u32::MAX);
    gpt::mbr::ProtectiveMBR::with_lb_size(last_lba).overwrite_lba0(&mut disk)?;

    let mut table = gpt::GptConfig::new()
        .writable(true)
        .initialized(false)
        .logical_block_size(LogicalBlockSize::Lb512)
        .create_from_device(disk, None)?;
    table.update_partitions(BTreeMap::new())?;

    let boot_id = table.add_partition(
        "sol-boot",
        boot_partition_size,
        gpt::partition_types::EFI,
        0,
        Some(PARTITION_ALIGNMENT_LBAS),
    )?;
    let data_id = table.add_partition(
        "sol-data",
        DATA_PARTITION_BYTES,
        // Use the conventional Basic Data GUID so desktop hosts can mount the
        // FAT32 data partition normally. The UEFI hand-off matches it together
        // with the `sol-data` GPT name, never by an unstable partition index.
        gpt::partition_types::BASIC,
        0,
        Some(PARTITION_ALIGNMENT_LBAS),
    )?;

    let boot_partition = table.partitions()[&boot_id].clone();
    let data_partition = table.partitions()[&data_id].clone();
    drop(table.write()?);

    let mut output = OpenOptions::new()
        .read(true)
        .write(true)
        .open(output_image)?;
    output.seek(SeekFrom::Start(
        boot_partition.bytes_start(LogicalBlockSize::Lb512)?,
    ))?;
    output.write_all(&boot_partition_bytes)?;
    output.flush()?;
    drop(output);

    copy_data_partition(data_volume, output_image, &data_partition)
}

fn extract_efi_partition(image: &Path) -> io::Result<(Vec<u8>, u64)> {
    let table = gpt::GptConfig::new()
        .writable(false)
        .initialized(true)
        .logical_block_size(LogicalBlockSize::Lb512)
        .open(image)?;
    let partition = table
        .partitions()
        .values()
        .find(|partition| partition.part_type_guid == gpt::partition_types::EFI)
        .cloned()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "EFI partition is missing"))?;
    drop(table);

    let start = partition.bytes_start(LogicalBlockSize::Lb512)?;
    let len = partition.bytes_len(LogicalBlockSize::Lb512)?;
    let mut bytes = vec![
        0;
        usize::try_from(len).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "EFI partition is too large")
        })?
    ];
    let mut source = File::open(image)?;
    source.seek(SeekFrom::Start(start))?;
    source.read_exact(&mut bytes)?;
    Ok((bytes, len))
}

fn build_data_volume(output: &Path, data_files: &Path) -> io::Result<()> {
    let disk = OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(output)?;
    disk.set_len(DATA_PARTITION_BYTES)?;
    let mut slice = StreamSlice::new(disk, 0, DATA_PARTITION_BYTES)?;

    fatfs_sfn::format_volume(
        &mut slice,
        FormatVolumeOptions::new()
            .fat_type(FatType::Fat32)
            .volume_label(*b"SOL_DATA   "),
    )?;

    let filesystem = FileSystem::new(slice, FsOptions::new())?;
    if filesystem.fat_type() != FatType::Fat32 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "data partition was not formatted as FAT32",
        ));
    }
    let root = filesystem.root_dir();
    copy_host_tree(data_files, &root)?;
    drop(root);
    filesystem.unmount()
}

fn copy_data_partition(
    data_volume: &Path,
    image: &Path,
    partition: &gpt::partition::Partition,
) -> io::Result<()> {
    let start = partition.bytes_start(LogicalBlockSize::Lb512)?;
    let len = partition.bytes_len(LogicalBlockSize::Lb512)?;
    if fs::metadata(data_volume)?.len() != len {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "data volume size does not match the GPT data partition",
        ));
    }

    let mut source = File::open(data_volume)?;
    let mut output = OpenOptions::new().read(true).write(true).open(image)?;
    output.seek(SeekFrom::Start(start))?;
    io::copy(&mut source, &mut output)?;
    output.flush()
}

fn copy_host_tree(
    host_directory: &Path,
    destination: &fatfs_sfn::Dir<'_, StreamSlice<File>>,
) -> io::Result<()> {
    let mut entries: Vec<_> = fs::read_dir(host_directory)?.collect::<Result<_, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        let name = entry.file_name();
        let name = name
            .to_str()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "non-UTF-8 data filename"))?;
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            let child = destination.create_dir(name)?;
            copy_host_tree(&entry.path(), &child)?;
        } else if file_type.is_file() {
            let mut source = File::open(entry.path())?;
            let mut target = destination.create_file(name)?;
            io::copy(&mut source, &mut target)?;
            target.flush()?;
        }
    }
    Ok(())
}

fn watch_host_tree(directory: &Path) -> io::Result<()> {
    println!("cargo:rerun-if-changed={}", directory.display());
    let mut entries: Vec<_> = fs::read_dir(directory)?.collect::<Result<_, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        let path = entry.path();
        println!("cargo:rerun-if-changed={}", path.display());
        if entry.file_type()?.is_dir() {
            watch_host_tree(&path)?;
        }
    }
    Ok(())
}

const fn align_up(value: u64, alignment: u64) -> u64 {
    value.div_ceil(alignment) * alignment
}
