use core::str;

use x86_64::instructions::port::Port;

use crate::fat32::Fat32;
use crate::storage::BlockDevice;
use crate::{print, println};

// Keep editable commands on one row of the minimum 1024px framebuffer. The
// redraw path uses carriage return so it intentionally does not span rows.
const LINE_CAPACITY: usize = 80;
const HISTORY_CAPACITY: usize = 16;

#[derive(Clone, Copy)]
pub struct SystemInfo {
    pub usable_frames: usize,
    pub frames_consumed_at_boot: usize,
}

/// Run the interactive shell. Keyboard events are produced by IRQ1 and consumed
/// here, so all parsing, allocation, and framebuffer output stay out of
/// interrupt context.
pub fn run<D: BlockDevice>(filesystem: &Fat32<'_, D>, system: SystemInfo) -> ! {
    println!("\nSol OS shell — type 'help' for commands.");
    prompt();

    let mut line = [0_u8; LINE_CAPACITY];
    let mut length = 0;
    let mut cursor = 0;
    let mut displayed_length = 0;
    let mut history = CommandHistory::new();
    let mut history_position = None;
    let mut draft = [0_u8; LINE_CAPACITY];
    let mut draft_length = 0;
    loop {
        while let Some(event) = crate::keyboard::next_event() {
            match event {
                crate::keyboard::KeyEvent::Enter => {
                    println!();
                    history.push(&line[..length]);
                    execute(filesystem, system, &line[..length]);
                    length = 0;
                    cursor = 0;
                    displayed_length = 0;
                    history_position = None;
                    prompt();
                }
                crate::keyboard::KeyEvent::Backspace => {
                    if cursor > 0 {
                        line.copy_within(cursor..length, cursor - 1);
                        length -= 1;
                        cursor -= 1;
                        leave_history(&mut history_position);
                        redraw(&line, length, cursor, &mut displayed_length);
                    }
                }
                crate::keyboard::KeyEvent::Delete => {
                    if cursor < length {
                        line.copy_within(cursor + 1..length, cursor);
                        length -= 1;
                        leave_history(&mut history_position);
                        redraw(&line, length, cursor, &mut displayed_length);
                    }
                }
                crate::keyboard::KeyEvent::Tab => {
                    if length + 4 <= LINE_CAPACITY {
                        line.copy_within(cursor..length, cursor + 4);
                        line[cursor..cursor + 4].fill(b' ');
                        length += 4;
                        cursor += 4;
                        leave_history(&mut history_position);
                        redraw(&line, length, cursor, &mut displayed_length);
                    }
                }
                crate::keyboard::KeyEvent::Character(byte) => {
                    if length < LINE_CAPACITY {
                        line.copy_within(cursor..length, cursor + 1);
                        line[cursor] = byte;
                        length += 1;
                        cursor += 1;
                        leave_history(&mut history_position);
                        redraw(&line, length, cursor, &mut displayed_length);
                    }
                }
                crate::keyboard::KeyEvent::Left if cursor > 0 => {
                    cursor -= 1;
                    redraw(&line, length, cursor, &mut displayed_length);
                }
                crate::keyboard::KeyEvent::Right if cursor < length => {
                    cursor += 1;
                    redraw(&line, length, cursor, &mut displayed_length);
                }
                crate::keyboard::KeyEvent::Home => {
                    cursor = 0;
                    redraw(&line, length, cursor, &mut displayed_length);
                }
                crate::keyboard::KeyEvent::End => {
                    cursor = length;
                    redraw(&line, length, cursor, &mut displayed_length);
                }
                crate::keyboard::KeyEvent::Up => {
                    if let Some(position) = history.previous(
                        history_position,
                        &line,
                        length,
                        &mut draft,
                        &mut draft_length,
                    ) {
                        history_position = Some(position);
                        length = history.copy_into(position, &mut line);
                        cursor = length;
                        redraw(&line, length, cursor, &mut displayed_length);
                    }
                }
                crate::keyboard::KeyEvent::Down => {
                    if let Some(position) = history.next(history_position) {
                        history_position = position;
                        if let Some(position) = position {
                            length = history.copy_into(position, &mut line);
                        } else {
                            line[..draft_length].copy_from_slice(&draft[..draft_length]);
                            length = draft_length;
                        }
                        cursor = length;
                        redraw(&line, length, cursor, &mut displayed_length);
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
    crate::framebuffer::show_cursor();
}

fn redraw(line: &[u8; LINE_CAPACITY], length: usize, cursor: usize, displayed_length: &mut usize) {
    print!("\rsol> ");
    print_ascii(&line[..length]);
    for _ in length..*displayed_length {
        print!(" ");
    }
    print!("\rsol> ");
    print_ascii(&line[..cursor]);
    *displayed_length = length;
    crate::framebuffer::show_cursor();
}

fn print_ascii(bytes: &[u8]) {
    for &byte in bytes {
        print!("{}", byte as char);
    }
}

fn leave_history(history_position: &mut Option<usize>) {
    *history_position = None;
}

struct CommandHistory {
    entries: [[u8; LINE_CAPACITY]; HISTORY_CAPACITY],
    lengths: [usize; HISTORY_CAPACITY],
    count: usize,
    next: usize,
}

impl CommandHistory {
    const fn new() -> Self {
        Self {
            entries: [[0; LINE_CAPACITY]; HISTORY_CAPACITY],
            lengths: [0; HISTORY_CAPACITY],
            count: 0,
            next: 0,
        }
    }

    fn push(&mut self, line: &[u8]) {
        if line.iter().all(|byte| byte.is_ascii_whitespace()) {
            return;
        }
        if self.count > 0 {
            let newest = (self.next + HISTORY_CAPACITY - 1) % HISTORY_CAPACITY;
            if self.lengths[newest] == line.len() && self.entries[newest][..line.len()] == line[..]
            {
                return;
            }
        }

        self.entries[self.next][..line.len()].copy_from_slice(line);
        self.lengths[self.next] = line.len();
        self.next = (self.next + 1) % HISTORY_CAPACITY;
        self.count = (self.count + 1).min(HISTORY_CAPACITY);
    }

    fn previous(
        &self,
        current: Option<usize>,
        line: &[u8; LINE_CAPACITY],
        line_length: usize,
        draft: &mut [u8; LINE_CAPACITY],
        draft_length: &mut usize,
    ) -> Option<usize> {
        if self.count == 0 {
            return None;
        }
        match current {
            Some(position) if position + 1 < self.count => Some(position + 1),
            Some(position) => Some(position),
            None => {
                draft[..line_length].copy_from_slice(&line[..line_length]);
                *draft_length = line_length;
                Some(0)
            }
        }
    }

    fn next(&self, current: Option<usize>) -> Option<Option<usize>> {
        match current {
            Some(position) if position > 0 => Some(Some(position - 1)),
            Some(_) => Some(None),
            None => None,
        }
    }

    fn copy_into(&self, position: usize, output: &mut [u8; LINE_CAPACITY]) -> usize {
        let index = (self.next + HISTORY_CAPACITY - 1 - position) % HISTORY_CAPACITY;
        let length = self.lengths[index];
        output[..length].copy_from_slice(&self.entries[index][..length]);
        length
    }
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
