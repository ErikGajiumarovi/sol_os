#![no_std]
#![no_main]

use bootloader_api::config::{BootloaderConfig, Mapping};
use bootloader_api::{BootInfo, entry_point};
use core::panic::PanicInfo;

mod console;
mod framebuffer;
mod serial;

const BOOTLOADER_CONFIG: BootloaderConfig = {
    let mut config = BootloaderConfig::new_default();
    config.kernel_stack_size = 128 * 1024;
    config.mappings.physical_memory = Some(Mapping::Dynamic);
    config
};

entry_point!(kernel_main, config = &BOOTLOADER_CONFIG);

fn kernel_main(boot_info: &'static mut BootInfo) -> ! {
    serial::init();

    let framebuffer = boot_info
        .framebuffer
        .take()
        .expect("UEFI did not provide a framebuffer");
    framebuffer::init(framebuffer);
    framebuffer::clear();

    println!("Hello from kernel");
    println!("Sol OS milestone 1: x86_64 UEFI framebuffer online");
    println!("The kernel is halted cleanly.");
    serial_println!("SOL_OS_M1_OK");

    hlt_loop()
}

pub fn hlt_loop() -> ! {
    loop {
        x86_64::instructions::hlt();
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    serial_println!("KERNEL PANIC: {info}");
    console::panic_print(format_args!("\nKERNEL PANIC: {info}\n"));
    hlt_loop()
}
