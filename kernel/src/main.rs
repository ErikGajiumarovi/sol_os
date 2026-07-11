#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

extern crate alloc;

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use bootloader_api::config::{BootloaderConfig, Mapping};
use bootloader_api::{BootInfo, entry_point};
use core::panic::PanicInfo;
use x86_64::VirtAddr;

mod allocator;
mod console;
mod fat32;
mod framebuffer;
mod gdt;
mod interrupts;
mod keyboard;
mod memory;
mod pci;
mod pic;
mod serial;
mod shell;
mod storage;
mod timer;

#[cfg(any(
    all(feature = "fault-test-double", feature = "fault-test-gpf"),
    all(feature = "fault-test-double", feature = "fault-test-page"),
    all(feature = "fault-test-gpf", feature = "fault-test-page")
))]
compile_error!("select only one fault-test feature at a time");

const BOOTLOADER_CONFIG: BootloaderConfig = {
    let mut config = BootloaderConfig::new_default();
    config.kernel_stack_size = 128 * 1024;
    config.mappings.physical_memory = Some(Mapping::Dynamic);
    config
};

entry_point!(kernel_main, config = &BOOTLOADER_CONFIG);

fn kernel_main(boot_info: &'static mut BootInfo) -> ! {
    // Firmware normally transfers control with interrupts disabled, but this
    // kernel must not depend on that undocumented hand-off detail. The IDT and
    // dedicated double-fault stack are installed below before any IRQ can run.
    x86_64::instructions::interrupts::disable();
    serial::init();

    let framebuffer = boot_info
        .framebuffer
        .take()
        .expect("UEFI did not provide a framebuffer");
    framebuffer::init(framebuffer);
    framebuffer::clear();

    println!("Hello from kernel");
    println!("Sol OS: x86_64 UEFI framebuffer online");

    let ramdisk_address = boot_info.ramdisk_addr.into_option();
    let ramdisk_length = boot_info.ramdisk_len;

    gdt::init();
    interrupts::init();
    x86_64::instructions::interrupts::int3();
    println!("GDT/TSS/IDT initialized; breakpoint handler returned");

    let physical_memory_offset = boot_info
        .physical_memory_offset
        .into_option()
        .expect("bootloader did not map physical memory");
    let mut mapper = unsafe { memory::init(VirtAddr::new(physical_memory_offset)) };
    let mut frame_allocator =
        unsafe { memory::BootInfoFrameAllocator::init(&boot_info.memory_regions) };
    memory::init_heap(&mut mapper, &mut frame_allocator).expect("heap mapping failed");

    let mut values = Vec::new();
    values.extend_from_slice(&[3_u64, 1, 4, 1, 5]);
    let greeting = String::from("alloc online");
    let boxed_value = Box::new(42_u64);
    println!(
        "Heap: {} KiB mapped, {} usable frames ({} consumed); {greeting}",
        allocator::ALLOCATOR.capacity() / 1024,
        frame_allocator.usable_frame_count(),
        frame_allocator.allocated_frames(),
    );
    serial_println!(
        "SOL_OS_M3_OK vec_len={} box={} heap_used={}",
        values.len(),
        *boxed_value,
        allocator::ALLOCATOR.used(),
    );

    let data_device =
        unsafe { storage::RamDiskBlockDevice::from_boot_info(ramdisk_address, ramdisk_length) }
            .expect("bootloader did not provide the FAT32 ramdisk");
    let data_filesystem = fat32::Fat32::mount(&data_device).expect("FAT32 mount failed");
    println!(
        "FAT32 data volume mounted: {} sectors, root cluster {}",
        storage::BlockDevice::sector_count(&data_device),
        data_filesystem.root_cluster(),
    );
    data_filesystem
        .list_dir("/", |entry| {
            let kind = if entry.is_directory() {
                "<DIR>"
            } else {
                "     "
            };
            println!("  {kind} {:>7} {}", entry.size(), entry.name());
            true
        })
        .expect("root directory read failed");
    print!("HELLO.TXT: ");
    data_filesystem
        .read_file("HELLO.TXT", |bytes| {
            let text = core::str::from_utf8(bytes).unwrap_or("<non-UTF-8 file data>");
            print!("{text}");
        })
        .expect("HELLO.TXT read failed");
    println!();
    serial_println!("SOL_OS_M7_OK root_and_hello_read");

    run_requested_fault_test();

    let ps2_configured = keyboard::initialize_controller();
    interrupts::initialize_hardware();
    println!("PIT and PS/2 IRQ1 enabled; keyboard input will echo below.");
    serial_println!(
        "SOL_OS_M4_READY ps2_set1={} ticks={} uptime={} dropped={}",
        ps2_configured,
        timer::ticks(),
        timer::uptime_seconds(),
        keyboard::dropped_chars(),
    );
    shell::run(
        &data_filesystem,
        shell::SystemInfo {
            usable_frames: frame_allocator.usable_frame_count(),
            frames_consumed_at_boot: frame_allocator.allocated_frames(),
        },
    )
}

#[inline(never)]
fn run_requested_fault_test() {
    #[cfg(feature = "fault-test-page")]
    {
        serial_println!("FAULT_TEST_PAGE_BEGIN");
        // SAFETY: This build is intentionally destructive. The chosen canonical address is
        // outside every bootloader mapping and is written only to exercise the page-fault IDT entry.
        unsafe { (0x0000_4444_4444_0000usize as *mut u64).write_volatile(0xfeed_face_cafe_beef) };
        panic!("page-fault test unexpectedly returned");
    }

    #[cfg(feature = "fault-test-gpf")]
    {
        serial_println!("FAULT_TEST_GPF_BEGIN");
        // SAFETY: An instruction fetch from this deliberately non-canonical address raises
        // #GP(0). This path is compiled only for the dedicated fault-test image. `0x0200…`
        // stays non-canonical on both four- and five-level paging CPUs.
        unsafe { core::arch::asm!("mov rax, 0x0200000000000000", "jmp rax", options(noreturn)) }
    }

    #[cfg(feature = "fault-test-double")]
    {
        serial_println!("FAULT_TEST_DOUBLE_BEGIN");
        // SAFETY: Replacing RSP with zero makes delivery of the following breakpoint fail. The
        // CPU must then enter #DF on the independently allocated IST stack.
        unsafe {
            core::arch::asm!("mov rsp, 0", "int3", options(noreturn));
        }
    }
}

pub fn hlt_loop() -> ! {
    loop {
        x86_64::instructions::hlt();
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    // A panic can occur while an interrupt controller is live. Stop further
    // IRQ delivery before rendering the diagnostic so a second fault cannot
    // re-enter the panic path while its framebuffer/serial locks are held.
    x86_64::instructions::interrupts::disable();
    serial_println!("KERNEL PANIC: {info}");
    console::panic_print(format_args!("\nKERNEL PANIC: {info}\n"));
    hlt_loop()
}
