#![no_std]
#![no_main]
#![deny(unsafe_op_in_unsafe_fn)]

use crate::memory_descriptor::UefiMemoryDescriptor;
use bootloader_api::info::FrameBufferInfo;
use bootloader_boot_config::BootConfig;
use bootloader_x86_64_common::{
    Kernel, RawFrameBufferInfo, SystemInfo, legacy_memory_region::LegacyFrameAllocator,
};
use core::{
    cell::UnsafeCell,
    ops::{Deref, DerefMut},
    ptr, slice,
};
use uefi::{
    CStr8, CStr16,
    prelude::{Boot, Handle, Status, SystemTable, entry},
    proto::{
        ProtocolPointer,
        console::gop::{GraphicsOutput, PixelFormat},
        device_path::DevicePath,
        loaded_image::LoadedImage,
        media::{
            block::BlockIO,
            file::{File, FileAttribute, FileInfo, FileMode},
            fs::SimpleFileSystem,
            partition::PartitionInfo,
        },
        network::{
            IpAddress,
            pxe::{BaseCode, DhcpV4Packet},
        },
    },
    table::boot::{
        AllocateType, MemoryType, OpenProtocolAttributes, OpenProtocolParams, ScopedProtocol,
        SearchType,
    },
};
use x86_64::{
    PhysAddr, VirtAddr,
    structures::paging::{FrameAllocator, OffsetPageTable, PageTable, PhysFrame, Size4KiB},
};

mod memory_descriptor;

static SYSTEM_TABLE: RacyCell<Option<SystemTable<Boot>>> = RacyCell::new(None);

struct RacyCell<T>(UnsafeCell<T>);

impl<T> RacyCell<T> {
    const fn new(v: T) -> Self {
        Self(UnsafeCell::new(v))
    }
}

unsafe impl<T> Sync for RacyCell<T> {}

impl<T> core::ops::Deref for RacyCell<T> {
    type Target = UnsafeCell<T>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[entry]
fn efi_main(image: Handle, st: SystemTable<Boot>) -> Status {
    main_inner(image, st)
}

fn main_inner(image: Handle, mut st: SystemTable<Boot>) -> Status {
    // temporarily clone the y table for printing panics
    unsafe {
        *SYSTEM_TABLE.get() = Some(st.unsafe_clone());
    }

    let mut boot_mode = BootMode::Disk;

    let mut kernel = load_kernel(image, &mut st, boot_mode);
    if kernel.is_none() {
        // Try TFTP boot
        boot_mode = BootMode::Tftp;
        kernel = load_kernel(image, &mut st, boot_mode);
    }
    let kernel = kernel.expect("Failed to load kernel");

    let config_file = load_config_file(image, &mut st, boot_mode);
    let mut error_loading_config: Option<serde_json_core::de::Error> = None;
    let mut config: BootConfig = match config_file
        .as_deref()
        .map(serde_json_core::from_slice)
        .transpose()
    {
        Ok(data) => data.unwrap_or_default().0,
        Err(err) => {
            error_loading_config = Some(err);
            Default::default()
        }
    };

    #[allow(deprecated)]
    if config.frame_buffer.minimum_framebuffer_height.is_none() {
        config.frame_buffer.minimum_framebuffer_height =
            kernel.config.frame_buffer.minimum_framebuffer_height;
    }
    #[allow(deprecated)]
    if config.frame_buffer.minimum_framebuffer_width.is_none() {
        config.frame_buffer.minimum_framebuffer_width =
            kernel.config.frame_buffer.minimum_framebuffer_width;
    }
    let framebuffer = init_logger(image, &st, &config);

    unsafe {
        *SYSTEM_TABLE.get() = None;
    }

    log::info!("UEFI bootloader started");

    if let Some(framebuffer) = framebuffer {
        log::info!("Using framebuffer at {:#x}", framebuffer.addr);
    }

    if let Some(err) = error_loading_config {
        log::warn!("Failed to deserialize the config file {:?}", err);
    } else {
        log::info!("Reading configuration from disk was successful");
    }

    log::info!("Trying to load Sol OS data partition via {:?}", boot_mode);
    // The kernel's filesystem image is read from the actual GPT partition on
    // the boot device while UEFI Block I/O is still available. `BootInfo`
    // calls this payload a ramdisk, but it is deliberately a firmware-read
    // snapshot of `sol-data`, not a build-time file embedded in the ESP.
    let ramdisk = load_sol_data_partition(image, &mut st, boot_mode);

    log::info!(
        "{}",
        match ramdisk {
            Some(_) => "Loaded Sol OS data partition",
            None => "Sol OS data partition not found.",
        }
    );

    log::trace!("exiting boot services");
    let (system_table, mut memory_map) = st.exit_boot_services();

    memory_map.sort();

    let mut frame_allocator =
        LegacyFrameAllocator::new(memory_map.entries().copied().map(UefiMemoryDescriptor));

    let max_phys_addr = frame_allocator.max_phys_addr();
    let page_tables = create_page_tables(&mut frame_allocator, max_phys_addr, framebuffer.as_ref());
    let mut ramdisk_len = 0u64;
    let ramdisk_addr = if let Some(rd) = ramdisk {
        ramdisk_len = rd.len() as u64;
        Some(rd.as_ptr() as usize as u64)
    } else {
        None
    };
    let system_info = SystemInfo {
        framebuffer,
        rsdp_addr: {
            use uefi::table::cfg;
            let mut config_entries = system_table.config_table().iter();
            // look for an ACPI2 RSDP first
            let acpi2_rsdp = config_entries.find(|entry| matches!(entry.guid, cfg::ACPI2_GUID));
            // if no ACPI2 RSDP is found, look for a ACPI1 RSDP
            let rsdp = acpi2_rsdp
                .or_else(|| config_entries.find(|entry| matches!(entry.guid, cfg::ACPI_GUID)));
            rsdp.map(|entry| PhysAddr::new(entry.address as u64))
        },
        ramdisk_addr,
        ramdisk_len,
    };

    bootloader_x86_64_common::load_and_switch_to_kernel(
        kernel,
        config,
        frame_allocator,
        page_tables,
        system_info,
    );
}

#[derive(Clone, Copy, Debug)]
pub enum BootMode {
    Disk,
    Tftp,
}

const SOL_DATA_PARTITION_TYPE: uefi::Guid = uefi::guid!("ebd0a0a2-b9e5-4433-87c0-68b6b72699c7");
const SOL_DATA_PARTITION_NAME: &[u8] = b"sol-data";
const PAGE_SIZE: u64 = 4096;
const SOL_DATA_BYTES: u64 = 64 * 1024 * 1024;
const BLOCK_IO_CHUNK_BYTES: usize = 64 * 1024;

/// Read the dedicated FAT32 GPT partition into LoaderData before
/// ExitBootServices. UEFI invalidates Block I/O protocols afterwards, so a
/// read-only snapshot is the narrowest reliable hand-off for this no_std
/// kernel while still sourcing every byte from the boot USB's real partition.
fn load_sol_data_partition(
    image: Handle,
    st: &mut SystemTable<Boot>,
    boot_mode: BootMode,
) -> Option<&'static mut [u8]> {
    match boot_mode {
        BootMode::Disk => load_sol_data_partition_from_disk(image, st),
        // A TFTP boot has no local boot medium whose partition can be mounted.
        BootMode::Tftp => None,
    }
}

fn load_sol_data_partition_from_disk(
    image: Handle,
    st: &mut SystemTable<Boot>,
) -> Option<&'static mut [u8]> {
    let boot_services = st.boot_services();
    let handles = boot_services
        .locate_handle_buffer(SearchType::from_proto::<PartitionInfo>())
        .ok()?;

    for handle in handles.iter().copied() {
        // `GetProtocol` deliberately shares a short-lived read-only view with
        // firmware's filesystem driver. The target partition can already have
        // a FAT driver bound, so taking exclusive ownership would be brittle.
        let partition_info = unsafe {
            boot_services.open_protocol::<PartitionInfo>(
                OpenProtocolParams {
                    handle,
                    agent: image,
                    controller: None,
                },
                OpenProtocolAttributes::GetProtocol,
            )
        };
        let Ok(partition_info) = partition_info else {
            continue;
        };
        let is_sol_data = partition_info
            .gpt_partition_entry()
            .is_some_and(is_sol_data_partition_entry);
        drop(partition_info);
        if !is_sol_data {
            continue;
        }

        let block_io = unsafe {
            boot_services.open_protocol::<BlockIO>(
                OpenProtocolParams {
                    handle,
                    agent: image,
                    controller: None,
                },
                OpenProtocolAttributes::GetProtocol,
            )
        };
        let Ok(block_io) = block_io else {
            log::error!("sol-data partition has no Block I/O protocol");
            return None;
        };
        let media = block_io.media();
        let block_size = u64::from(media.block_size());
        let block_count = media.last_block().checked_add(1)?;
        let byte_len = block_count.checked_mul(block_size)?;
        if !media.is_media_present()
            || !media.is_logical_partition()
            || block_size != 512
            || media.io_align() > PAGE_SIZE as u32
            || byte_len != SOL_DATA_BYTES
        {
            log::error!("sol-data Block I/O geometry is unsupported");
            return None;
        }
        let byte_len_usize = usize::try_from(byte_len).ok()?;
        let page_count = byte_len / PAGE_SIZE;
        let data_ptr = boot_services
            .allocate_pages(AllocateType::AnyPages, MemoryType::LOADER_DATA, page_count as usize)
            .ok()? as *mut u8;
        // SAFETY: `allocate_pages` returned exactly `page_count * PAGE_SIZE`
        // bytes, and the checked byte length has that same value.
        let data = unsafe { slice::from_raw_parts_mut(data_ptr, byte_len_usize) };
        for (index, chunk) in data.chunks_mut(BLOCK_IO_CHUNK_BYTES).enumerate() {
            let lba = (index as u64)
                .checked_mul((BLOCK_IO_CHUNK_BYTES as u64) / block_size)?;
            if block_io.read_blocks(media.media_id(), lba, chunk).is_err() {
                log::error!("failed to read sol-data partition");
                return None;
            }
        }
        log::info!("Loaded {} bytes from the sol-data GPT partition", byte_len);
        return Some(data);
    }

    None
}

fn is_sol_data_partition_entry(entry: &uefi::proto::media::partition::GptPartitionEntry) -> bool {
    // GPT entries are packed by the UEFI ABI, so copy their multi-byte fields
    // with an explicitly unaligned read before Rust observes them by value.
    let partition_type = unsafe { core::ptr::addr_of!(entry.partition_type_guid).read_unaligned() };
    if partition_type.0 != SOL_DATA_PARTITION_TYPE {
        return false;
    }
    let name = unsafe { core::ptr::addr_of!(entry.partition_name).read_unaligned() };
    if name.len() <= SOL_DATA_PARTITION_NAME.len() {
        return false;
    }
    name[..SOL_DATA_PARTITION_NAME.len()]
        .iter()
        .zip(SOL_DATA_PARTITION_NAME)
        .all(|(character, expected)| u16::from(*character) == u16::from(*expected))
        && u16::from(name[SOL_DATA_PARTITION_NAME.len()]) == 0
}

fn load_config_file(
    image: Handle,
    st: &mut SystemTable<Boot>,
    boot_mode: BootMode,
) -> Option<&'static mut [u8]> {
    load_file_from_boot_method(image, st, "boot.json\0", boot_mode)
}

fn load_kernel(
    image: Handle,
    st: &mut SystemTable<Boot>,
    boot_mode: BootMode,
) -> Option<Kernel<'static>> {
    let kernel_slice = load_file_from_boot_method(image, st, "kernel-x86_64\0", boot_mode)?;
    Some(Kernel::parse(kernel_slice))
}

fn load_file_from_boot_method(
    image: Handle,
    st: &mut SystemTable<Boot>,
    filename: &str,
    boot_mode: BootMode,
) -> Option<&'static mut [u8]> {
    match boot_mode {
        BootMode::Disk => load_file_from_disk(filename, image, st),
        BootMode::Tftp => load_file_from_tftp_boot_server(filename, image, st),
    }
}

fn open_device_path_protocol(
    image: Handle,
    st: &SystemTable<Boot>,
) -> Option<ScopedProtocol<DevicePath>> {
    let this = st.boot_services();
    let loaded_image = unsafe {
        this.open_protocol::<LoadedImage>(
            OpenProtocolParams {
                handle: image,
                agent: image,
                controller: None,
            },
            OpenProtocolAttributes::Exclusive,
        )
    };

    if loaded_image.is_err() {
        log::error!("Failed to open protocol LoadedImage");
        return None;
    }
    let loaded_image = loaded_image.unwrap();
    let loaded_image = loaded_image.deref();

    let device_handle = loaded_image.device();

    let device_path = unsafe {
        this.open_protocol::<DevicePath>(
            OpenProtocolParams {
                handle: device_handle,
                agent: image,
                controller: None,
            },
            OpenProtocolAttributes::Exclusive,
        )
    };
    if device_path.is_err() {
        log::error!("Failed to open protocol DevicePath");
        return None;
    }
    Some(device_path.unwrap())
}

fn locate_and_open_protocol<P: ProtocolPointer>(
    image: Handle,
    st: &SystemTable<Boot>,
) -> Option<ScopedProtocol<P>> {
    let this = st.boot_services();
    let device_path = open_device_path_protocol(image, st)?;
    let mut device_path = device_path.deref();

    let fs_handle = this.locate_device_path::<P>(&mut device_path);
    if fs_handle.is_err() {
        log::error!("Failed to open device path");
        return None;
    }

    let fs_handle = fs_handle.unwrap();

    let opened_handle = unsafe {
        this.open_protocol::<P>(
            OpenProtocolParams {
                handle: fs_handle,
                agent: image,
                controller: None,
            },
            OpenProtocolAttributes::Exclusive,
        )
    };

    if opened_handle.is_err() {
        log::error!("Failed to open protocol {}", core::any::type_name::<P>());
        return None;
    }
    Some(opened_handle.unwrap())
}

fn load_file_from_disk(
    name: &str,
    image: Handle,
    st: &SystemTable<Boot>,
) -> Option<&'static mut [u8]> {
    let mut file_system_raw = locate_and_open_protocol::<SimpleFileSystem>(image, st)?;
    let file_system = file_system_raw.deref_mut();

    let mut root = file_system.open_volume().unwrap();
    let mut buf = [0u16; 256];
    assert!(name.len() < 256);
    let filename = CStr16::from_str_with_buf(name.trim_end_matches('\0'), &mut buf)
        .expect("Failed to convert string to utf16");

    let file_handle_result = root.open(filename, FileMode::Read, FileAttribute::empty());

    let file_handle = file_handle_result.ok()?;

    let mut file = match file_handle.into_type().unwrap() {
        uefi::proto::media::file::FileType::Regular(f) => f,
        uefi::proto::media::file::FileType::Dir(_) => panic!(),
    };

    let mut buf = [0; 500];
    let file_info: &mut FileInfo = file.get_info(&mut buf).unwrap();
    let file_size = usize::try_from(file_info.file_size()).unwrap();

    let file_ptr = st
        .boot_services()
        .allocate_pages(
            AllocateType::AnyPages,
            MemoryType::LOADER_DATA,
            ((file_size - 1) / 4096) + 1,
        )
        .unwrap() as *mut u8;
    unsafe { ptr::write_bytes(file_ptr, 0, file_size) };
    let file_slice = unsafe { slice::from_raw_parts_mut(file_ptr, file_size) };
    file.read(file_slice).unwrap();

    Some(file_slice)
}

/// Try to load a kernel from a TFTP boot server.
fn load_file_from_tftp_boot_server(
    name: &str,
    image: Handle,
    st: &SystemTable<Boot>,
) -> Option<&'static mut [u8]> {
    let mut base_code_raw = locate_and_open_protocol::<BaseCode>(image, st)?;
    let base_code = base_code_raw.deref_mut();

    // Find the TFTP boot server.
    let mode = base_code.mode();
    assert!(mode.dhcp_ack_received);
    let dhcpv4: &DhcpV4Packet = mode.dhcp_ack.as_ref();
    let server_ip = IpAddress::new_v4(dhcpv4.bootp_si_addr);
    assert!(name.len() < 256);

    let filename = CStr8::from_bytes_with_nul(name.as_bytes()).unwrap();

    // Determine the kernel file size.
    let file_size = base_code.tftp_get_file_size(&server_ip, filename).ok()?;
    let kernel_size = usize::try_from(file_size).expect("The file size should fit into usize");

    // Allocate some memory for the kernel file.
    let ptr = st
        .boot_services()
        .allocate_pages(
            AllocateType::AnyPages,
            MemoryType::LOADER_DATA,
            ((kernel_size - 1) / 4096) + 1,
        )
        .expect("Failed to allocate memory for the file") as *mut u8;
    let slice = unsafe { slice::from_raw_parts_mut(ptr, kernel_size) };

    // Load the kernel file.
    base_code
        .tftp_read_file(&server_ip, filename, Some(slice))
        .expect("Failed to read kernel file from the TFTP boot server");

    Some(slice)
}

/// Creates page table abstraction types for both the bootloader and kernel page tables.
fn create_page_tables(
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
    max_phys_addr: PhysAddr,
    frame_buffer: Option<&RawFrameBufferInfo>,
) -> bootloader_x86_64_common::PageTables {
    // UEFI identity-maps all memory, so the offset between physical and virtual addresses is 0
    let phys_offset = VirtAddr::new(0);

    // copy the currently active level 4 page table, because it might be read-only
    log::trace!("switching to new level 4 table");
    let bootloader_page_table = {
        let old_table = {
            let frame = x86_64::registers::control::Cr3::read().0;
            let ptr: *const PageTable = (phys_offset + frame.start_address().as_u64()).as_ptr();
            unsafe { &*ptr }
        };
        let new_frame = frame_allocator
            .allocate_frame()
            .expect("Failed to allocate frame for new level 4 table");
        let new_table: &mut PageTable = {
            let ptr: *mut PageTable =
                (phys_offset + new_frame.start_address().as_u64()).as_mut_ptr();
            // create a new, empty page table
            unsafe {
                ptr.write(PageTable::new());
                &mut *ptr
            }
        };

        // copy the pml4 entries for all identity mapped memory.
        let end_addr = VirtAddr::new(max_phys_addr.as_u64() - 1);
        for p4 in 0..=usize::from(end_addr.p4_index()) {
            new_table[p4] = old_table[p4].clone();
        }

        // copy the pml4 entry for the frame buffer (the frame buffer is not
        // necessarily part of the identity mapping).
        if let Some(frame_buffer) = frame_buffer {
            let start_addr = VirtAddr::new(frame_buffer.addr.as_u64());
            let end_addr = start_addr + frame_buffer.info.byte_len as u64;
            for p4 in usize::from(start_addr.p4_index())..=usize::from(end_addr.p4_index()) {
                new_table[p4] = old_table[p4].clone();
            }
        }

        // the first level 4 table entry is now identical, so we can just load the new one
        unsafe {
            x86_64::registers::control::Cr3::write(
                new_frame,
                x86_64::registers::control::Cr3Flags::empty(),
            );
            OffsetPageTable::new(&mut *new_table, phys_offset)
        }
    };

    // create a new page table hierarchy for the kernel
    let (kernel_page_table, kernel_level_4_frame) = {
        // get an unused frame for new level 4 page table
        let frame: PhysFrame = frame_allocator.allocate_frame().expect("no unused frames");
        log::info!("New page table at: {:#?}", &frame);
        // get the corresponding virtual address
        let addr = phys_offset + frame.start_address().as_u64();
        // initialize a new page table
        let ptr = addr.as_mut_ptr();
        unsafe { *ptr = PageTable::new() };
        let level_4_table = unsafe { &mut *ptr };
        (
            unsafe { OffsetPageTable::new(level_4_table, phys_offset) },
            frame,
        )
    };

    bootloader_x86_64_common::PageTables {
        bootloader: bootloader_page_table,
        kernel: kernel_page_table,
        kernel_level_4_frame,
    }
}

fn init_logger(
    image_handle: Handle,
    st: &SystemTable<Boot>,
    config: &BootConfig,
) -> Option<RawFrameBufferInfo> {
    let gop_handle = st
        .boot_services()
        .get_handle_for_protocol::<GraphicsOutput>()
        .ok()?;
    let mut gop = unsafe {
        st.boot_services()
            .open_protocol::<GraphicsOutput>(
                OpenProtocolParams {
                    handle: gop_handle,
                    agent: image_handle,
                    controller: None,
                },
                OpenProtocolAttributes::Exclusive,
            )
            .ok()?
    };

    let mode = {
        let modes = gop.modes();
        match (
            config
                .frame_buffer
                .minimum_framebuffer_height
                .map(|v| usize::try_from(v).unwrap()),
            config
                .frame_buffer
                .minimum_framebuffer_width
                .map(|v| usize::try_from(v).unwrap()),
        ) {
            (Some(height), Some(width)) => modes
                .filter(|m| {
                    let res = m.info().resolution();
                    res.1 >= height && res.0 >= width
                })
                .last(),
            (Some(height), None) => modes.filter(|m| m.info().resolution().1 >= height).last(),
            (None, Some(width)) => modes.filter(|m| m.info().resolution().0 >= width).last(),
            _ => None,
        }
    };
    if let Some(mode) = mode {
        gop.set_mode(&mode)
            .expect("Failed to apply the desired display mode");
    }

    let mode_info = gop.current_mode_info();
    let mut framebuffer = gop.frame_buffer();
    let slice = unsafe { slice::from_raw_parts_mut(framebuffer.as_mut_ptr(), framebuffer.size()) };
    let info = FrameBufferInfo {
        byte_len: framebuffer.size(),
        width: mode_info.resolution().0,
        height: mode_info.resolution().1,
        pixel_format: match mode_info.pixel_format() {
            PixelFormat::Rgb => bootloader_api::info::PixelFormat::Rgb,
            PixelFormat::Bgr => bootloader_api::info::PixelFormat::Bgr,
            PixelFormat::Bitmask | PixelFormat::BltOnly => {
                panic!("Bitmask and BltOnly framebuffers are not supported")
            }
        },
        bytes_per_pixel: 4,
        stride: mode_info.stride(),
    };

    bootloader_x86_64_common::init_logger(
        slice,
        info,
        config.log_level,
        config.frame_buffer_logging,
        config.serial_logging,
    );

    Some(RawFrameBufferInfo {
        addr: PhysAddr::new(framebuffer.as_mut_ptr() as u64),
        info,
    })
}

#[cfg(target_os = "uefi")]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    use core::arch::asm;
    use core::fmt::Write;

    if let Some(st) = unsafe { &mut *SYSTEM_TABLE.get() } {
        let _ = st.stdout().clear();
        let _ = writeln!(st.stdout(), "{}", info);
    }

    unsafe {
        bootloader_x86_64_common::logger::LOGGER
            .get()
            .map(|l| l.force_unlock())
    };
    log::error!("{}", info);

    loop {
        unsafe { asm!("cli; hlt") };
    }
}
