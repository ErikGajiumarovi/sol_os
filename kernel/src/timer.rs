use core::sync::atomic::{AtomicU64, Ordering};
use x86_64::instructions::port::Port;

pub const TICKS_PER_SECOND: u64 = 100;
const PIT_INPUT_HZ: u64 = 1_193_182;
const PIT_COMMAND: u16 = 0x43;
const PIT_CHANNEL_0: u16 = 0x40;

static TICKS: AtomicU64 = AtomicU64::new(0);

pub fn initialize() {
    let divisor = (PIT_INPUT_HZ / TICKS_PER_SECOND) as u16;
    // SAFETY: channel 0 is the legacy timer wired to IRQ0. The PIC remains
    // masked until this programming sequence completes.
    unsafe {
        Port::<u8>::new(PIT_COMMAND).write(0x36);
        Port::<u8>::new(PIT_CHANNEL_0).write((divisor & 0xff) as u8);
        Port::<u8>::new(PIT_CHANNEL_0).write((divisor >> 8) as u8);
    }
}

pub fn tick() {
    TICKS.fetch_add(1, Ordering::Relaxed);
}

pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

pub fn uptime_seconds() -> u64 {
    ticks() / TICKS_PER_SECOND
}
