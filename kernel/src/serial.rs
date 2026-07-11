use core::fmt::{self, Write};
use spin::Mutex;
use x86_64::instructions::port::Port;

const COM1: u16 = 0x3f8;
static SERIAL_LOCK: Mutex<()> = Mutex::new(());

pub fn init() {
    let _guard = SERIAL_LOCK.lock();
    // SAFETY: COM1 is the conventional 16550 UART range. The lock ensures that this
    // kernel has exclusive access while programming the device registers.
    unsafe {
        write_port(COM1 + 1, 0x00);
        write_port(COM1 + 3, 0x80);
        write_port(COM1, 0x01);
        write_port(COM1 + 1, 0x00);
        write_port(COM1 + 3, 0x03);
        write_port(COM1 + 2, 0xc7);
        write_port(COM1 + 4, 0x0b);
    }
}

pub fn print(args: fmt::Arguments<'_>) {
    let _guard = SERIAL_LOCK.lock();
    let _ = SerialWriter.write_fmt(args);
}

struct SerialWriter;

impl Write for SerialWriter {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        for byte in text.bytes() {
            if byte == b'\n' {
                write_byte(b'\r');
            }
            write_byte(byte);
        }
        Ok(())
    }
}

fn write_byte(byte: u8) {
    for _ in 0..100_000 {
        // SAFETY: Reading the line-status register is side-effect free for a 16550 UART.
        if unsafe { read_port(COM1 + 5) } & 0x20 != 0 {
            // SAFETY: COM1 was initialized by `init`, and serial writes are locked.
            unsafe { write_port(COM1, byte) };
            return;
        }
        core::hint::spin_loop();
    }
}

unsafe fn write_port(port: u16, value: u8) {
    // SAFETY: The caller owns the relevant I/O port for the duration of this operation.
    unsafe { Port::<u8>::new(port).write(value) };
}

unsafe fn read_port(port: u16) -> u8 {
    // SAFETY: The caller owns the relevant I/O port for the duration of this operation.
    unsafe { Port::<u8>::new(port).read() }
}
