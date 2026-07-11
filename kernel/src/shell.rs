use core::str;

use x86_64::instructions::port::Port;

use crate::fat32::Fat32;
use crate::storage::BlockDevice;
use crate::{print, println};

const LINE_CAPACITY: usize = 256;

#[derive(Clone, Copy)]
pub struct SystemInfo {
    pub usable_frames: usize,
    pub frames_consumed_at_boot: usize,
}

/// Run the interactive shell. Keyboard bytes are produced by IRQ1 and consumed
/// here, so all parsing, allocation, and framebuffer output stay out of
/// interrupt context.
pub fn run<D: BlockDevice>(filesystem: &Fat32<'_, D>, system: SystemInfo) -> ! {
    println!("\nSol OS shell — type 'help' for commands.");
    prompt();

    let mut line = [0_u8; LINE_CAPACITY];
    let mut length = 0;
    loop {
        while let Some(byte) = crate::keyboard::next_char() {
            match byte {
                b'\n' => {
                    println!();
                    execute(filesystem, system, &line[..length]);
                    length = 0;
                    prompt();
                }
                0x08 => {
                    if length > 0 {
                        length -= 1;
                        print!("\u{8} \u{8}");
                    }
                }
                b'\t' => {
                    if length + 4 <= LINE_CAPACITY {
                        line[length..length + 4].fill(b' ');
                        length += 4;
                        print!("    ");
                    }
                }
                byte if byte.is_ascii_graphic() || byte == b' ' => {
                    if length < LINE_CAPACITY {
                        line[length] = byte;
                        length += 1;
                        print!("{}", byte as char);
                    }
                }
                _ => {}
            }
        }

        // `sti; hlt` executes atomically with respect to an IRQ, avoiding the
        // otherwise possible missed-key race between checking the queue and halt.
        x86_64::instructions::interrupts::enable_and_hlt();
    }
}

fn prompt() {
    print!("sol> ");
}

fn execute<D: BlockDevice>(filesystem: &Fat32<'_, D>, system: SystemInfo, input: &[u8]) {
    let line = match str::from_utf8(input) {
        Ok(line) => line.trim(),
        Err(_) => {
            println!("input is not valid UTF-8");
            return;
        }
    };
    if line.is_empty() {
        return;
    }

    let mut words = line.split_whitespace();
    let command = words.next().unwrap_or("");
    match command {
        "help" => help(),
        "echo" => println!("{}", arguments_after_command(line)),
        "ls" => {
            let path = words.next().unwrap_or("/");
            if words.next().is_some() {
                println!("usage: ls [path]");
                return;
            }
            match filesystem.list_dir(path, |entry| {
                let kind = if entry.is_directory() {
                    "<DIR>"
                } else {
                    "     "
                };
                println!("{kind} {:>7} {}", entry.size(), entry.name());
                true
            }) {
                Ok(()) => {}
                Err(error) => println!("ls: {error:?}"),
            }
        }
        "cat" => {
            let path = match words.next() {
                Some(path) => path,
                None => {
                    println!("usage: cat <file>");
                    return;
                }
            };
            if words.next().is_some() {
                println!("usage: cat <file>");
                return;
            }
            match filesystem.read_file(path, print_file_chunk) {
                Ok(_) => println!(),
                Err(error) => println!("cat: {error:?}"),
            }
        }
        "clear" => {
            crate::framebuffer::clear();
            crate::serial_print!("\x1b[2J\x1b[H");
        }
        "uptime" => println!(
            "uptime: {} s ({} timer ticks)",
            crate::timer::uptime_seconds(),
            crate::timer::ticks(),
        ),
        "meminfo" => {
            let physical_kib = system.usable_frames.saturating_mul(4);
            println!("usable physical memory: {physical_kib} KiB");
            println!(
                "heap: {} / {} KiB used (bump allocator)",
                crate::allocator::ALLOCATOR.used() / 1024,
                crate::allocator::ALLOCATOR.capacity() / 1024,
            );
            println!(
                "frames consumed during bring-up: {}",
                system.frames_consumed_at_boot
            );
        }
        "reboot" => reboot(),
        "halt" => halt(),
        _ => println!("unknown command: {command}; type 'help'"),
    }
}

fn help() {
    println!("commands:");
    println!("  help             show this text");
    println!("  echo <text>      print text");
    println!("  ls [path]        list a FAT32 directory");
    println!("  cat <file>       print a FAT32 file");
    println!("  clear            clear the framebuffer console");
    println!("  uptime           show PIT-based uptime");
    println!("  meminfo          show frame and heap statistics");
    println!("  reboot | halt    stop or reset the machine");
}

fn arguments_after_command(line: &str) -> &str {
    line.find(char::is_whitespace)
        .map(|index| line[index..].trim_start())
        .unwrap_or("")
}

fn print_file_chunk(bytes: &[u8]) {
    match str::from_utf8(bytes) {
        Ok(text) => print!("{text}"),
        Err(_) => {
            for &byte in bytes {
                if byte.is_ascii_graphic() || byte == b' ' || byte == b'\n' || byte == b'\r' {
                    print!("{}", byte as char);
                } else {
                    print!("\\x{byte:02x}");
                }
            }
        }
    }
}

fn reboot() -> ! {
    println!("rebooting through the PS/2 controller...");
    x86_64::instructions::interrupts::disable();
    // The 8042 ignores a command while its input buffer is occupied. Wait for
    // it briefly rather than racing the final keyboard IRQ/controller command.
    let mut status = Port::<u8>::new(0x64);
    for _ in 0..100_000 {
        // SAFETY: port 0x64 is the documented PS/2 controller status port.
        if unsafe { status.read() } & 0x02 == 0 {
            // SAFETY: 0xfe is the documented 8042 pulse-reset command. If
            // there is no 8042 controller, the fallback halt below avoids
            // executing arbitrary firmware.
            unsafe { status.write(0xfe) };
            break;
        }
        core::hint::spin_loop();
    }
    crate::hlt_loop()
}

fn halt() -> ! {
    println!("CPU halted.");
    x86_64::instructions::interrupts::disable();
    crate::hlt_loop()
}
