use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use x86_64::instructions::port::Port;

const INPUT_QUEUE_CAPACITY: usize = 256;
const SHIFT: u8 = 1;
const CAPS_LOCK: u8 = 2;
const PS2_DATA: u16 = 0x60;
const PS2_STATUS_COMMAND: u16 = 0x64;
const CONTROLLER_TIMEOUT: usize = 100_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyEvent {
    Character(u8),
    Backspace,
    Tab,
    Enter,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    Delete,
}

struct InputQueue {
    events: UnsafeCell<[KeyEvent; INPUT_QUEUE_CAPACITY]>,
    head: AtomicUsize,
    tail: AtomicUsize,
    dropped: AtomicUsize,
}

// The keyboard IRQ is the sole producer and the kernel main loop is the sole
// consumer. Atomics publish each written event before its head index advances.
unsafe impl Sync for InputQueue {}

impl InputQueue {
    const fn new() -> Self {
        Self {
            events: UnsafeCell::new([KeyEvent::Enter; INPUT_QUEUE_CAPACITY]),
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
            dropped: AtomicUsize::new(0),
        }
    }

    fn push(&self, event: KeyEvent) {
        let head = self.head.load(Ordering::Relaxed);
        let next = (head + 1) % INPUT_QUEUE_CAPACITY;
        if next == self.tail.load(Ordering::Acquire) {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }

        // SAFETY: IRQ1 is the only producer and owns the slot at `head` until
        // its Release store makes `next` visible to the main-loop consumer.
        unsafe { (*self.events.get())[head] = event };
        self.head.store(next, Ordering::Release);
    }

    fn pop(&self) -> Option<KeyEvent> {
        let tail = self.tail.load(Ordering::Relaxed);
        if tail == self.head.load(Ordering::Acquire) {
            return None;
        }

        // SAFETY: seeing a different head after Acquire means the producer
        // completed the write to this slot before publishing it.
        let event = unsafe { (*self.events.get())[tail] };
        self.tail
            .store((tail + 1) % INPUT_QUEUE_CAPACITY, Ordering::Release);
        Some(event)
    }
}

static INPUT_QUEUE: InputQueue = InputQueue::new();
static MODIFIERS: AtomicU8 = AtomicU8::new(0);
static EXTENDED_PREFIX: AtomicBool = AtomicBool::new(false);

/// Configure the 8042 controller to translate the keyboard's native set-2
/// bytes into set 1, which is the compact decoder below. UEFI firmware can
/// leave this bit in either state, so setting it explicitly makes QEMU's and
/// legacy hardware's behavior deterministic. Returns `false` if no responsive
/// PS/2 controller was found rather than spinning forever on a USB-only laptop.
pub fn initialize_controller() -> bool {
    // SAFETY: these are the documented 8042 command/status and data ports. IRQ1
    // is still masked, so controller replies cannot race the normal handler.
    unsafe {
        if !wait_for_input_empty() {
            return false;
        }
        write_command(0xad); // disable first PS/2 port while changing config
        drain_output();

        if !wait_for_input_empty() {
            return false;
        }
        write_command(0x20); // read command byte
        if !wait_for_output_full() {
            return false;
        }
        let command_byte = read_data();
        let configured = (command_byte | 0x01 | 0x40) & !0x10;

        if !wait_for_input_empty() {
            return false;
        }
        write_command(0x60); // next data byte is the command byte
        if !wait_for_input_empty() {
            return false;
        }
        write_data(configured);
        if !wait_for_input_empty() {
            return false;
        }
        write_command(0xae); // re-enable first PS/2 port
    }
    true
}

/// Consume a raw PS/2 set-1 scancode from IRQ1 and queue input events for the
/// shell. This intentionally performs no allocation, locking, or rendering in
/// interrupt context.
pub fn handle_scancode(scancode: u8) {
    if scancode == 0xe0 {
        EXTENDED_PREFIX.store(true, Ordering::Relaxed);
        return;
    }
    if EXTENDED_PREFIX.swap(false, Ordering::Relaxed) {
        if scancode & 0x80 == 0 {
            if let Some(event) = extended_event_for_set_1(scancode) {
                INPUT_QUEUE.push(event);
            }
        }
        return;
    }

    match scancode {
        0x2a | 0x36 => {
            MODIFIERS.fetch_or(SHIFT, Ordering::Relaxed);
            return;
        }
        0xaa | 0xb6 => {
            MODIFIERS.fetch_and(!SHIFT, Ordering::Relaxed);
            return;
        }
        0x3a => {
            MODIFIERS.fetch_xor(CAPS_LOCK, Ordering::Relaxed);
            return;
        }
        code if code & 0x80 != 0 => return,
        _ => {}
    }

    let modifiers = MODIFIERS.load(Ordering::Relaxed);
    if let Some(event) =
        ascii_for_set_1(scancode, modifiers & SHIFT != 0, modifiers & CAPS_LOCK != 0)
    {
        INPUT_QUEUE.push(event);
    }
}

pub fn next_event() -> Option<KeyEvent> {
    INPUT_QUEUE.pop()
}

pub fn dropped_chars() -> usize {
    INPUT_QUEUE.dropped.load(Ordering::Relaxed)
}

fn extended_event_for_set_1(scancode: u8) -> Option<KeyEvent> {
    match scancode {
        0x47 => Some(KeyEvent::Home),
        0x48 => Some(KeyEvent::Up),
        0x4b => Some(KeyEvent::Left),
        0x4d => Some(KeyEvent::Right),
        0x4f => Some(KeyEvent::End),
        0x50 => Some(KeyEvent::Down),
        0x53 => Some(KeyEvent::Delete),
        _ => None,
    }
}

fn ascii_for_set_1(scancode: u8, shift: bool, caps_lock: bool) -> Option<KeyEvent> {
    let (plain, shifted) = match scancode {
        0x02 => (b'1', b'!'),
        0x03 => (b'2', b'@'),
        0x04 => (b'3', b'#'),
        0x05 => (b'4', b'$'),
        0x06 => (b'5', b'%'),
        0x07 => (b'6', b'^'),
        0x08 => (b'7', b'&'),
        0x09 => (b'8', b'*'),
        0x0a => (b'9', b'('),
        0x0b => (b'0', b')'),
        0x0c => (b'-', b'_'),
        0x0d => (b'=', b'+'),
        0x0e => return Some(KeyEvent::Backspace),
        0x0f => return Some(KeyEvent::Tab),
        0x1a => (b'[', b'{'),
        0x1b => (b']', b'}'),
        0x1c => return Some(KeyEvent::Enter),
        0x27 => (b';', b':'),
        0x28 => (b'\'', b'\"'),
        0x29 => (b'`', b'~'),
        0x2b => (b'\\', b'|'),
        0x33 => (b',', b'<'),
        0x34 => (b'.', b'>'),
        0x35 => (b'/', b'?'),
        0x39 => return Some(KeyEvent::Character(b' ')),
        code => return letter_for_set_1(code, shift ^ caps_lock),
    };
    Some(KeyEvent::Character(if shift { shifted } else { plain }))
}

fn letter_for_set_1(scancode: u8, uppercase: bool) -> Option<KeyEvent> {
    let lowercase = match scancode {
        0x10 => b'q',
        0x11 => b'w',
        0x12 => b'e',
        0x13 => b'r',
        0x14 => b't',
        0x15 => b'y',
        0x16 => b'u',
        0x17 => b'i',
        0x18 => b'o',
        0x19 => b'p',
        0x1e => b'a',
        0x1f => b's',
        0x20 => b'd',
        0x21 => b'f',
        0x22 => b'g',
        0x23 => b'h',
        0x24 => b'j',
        0x25 => b'k',
        0x26 => b'l',
        0x2c => b'z',
        0x2d => b'x',
        0x2e => b'c',
        0x2f => b'v',
        0x30 => b'b',
        0x31 => b'n',
        0x32 => b'm',
        _ => return None,
    };
    Some(KeyEvent::Character(if uppercase {
        lowercase.to_ascii_uppercase()
    } else {
        lowercase
    }))
}

unsafe fn read_status() -> u8 {
    // SAFETY: only controller setup and IRQ1 access this hardware status port.
    unsafe { Port::<u8>::new(PS2_STATUS_COMMAND).read() }
}

unsafe fn read_data() -> u8 {
    // SAFETY: caller first observed the controller output-buffer-full bit.
    unsafe { Port::<u8>::new(PS2_DATA).read() }
}

unsafe fn write_command(command: u8) {
    // SAFETY: caller first observed the controller input-buffer-empty bit.
    unsafe { Port::<u8>::new(PS2_STATUS_COMMAND).write(command) };
}

unsafe fn write_data(data: u8) {
    // SAFETY: caller first observed the controller input-buffer-empty bit.
    unsafe { Port::<u8>::new(PS2_DATA).write(data) };
}

unsafe fn wait_for_input_empty() -> bool {
    for _ in 0..CONTROLLER_TIMEOUT {
        if unsafe { read_status() } & 0x02 == 0 {
            return true;
        }
        core::hint::spin_loop();
    }
    false
}

unsafe fn wait_for_output_full() -> bool {
    for _ in 0..CONTROLLER_TIMEOUT {
        if unsafe { read_status() } & 0x01 != 0 {
            return true;
        }
        core::hint::spin_loop();
    }
    false
}

unsafe fn drain_output() {
    for _ in 0..32 {
        if unsafe { read_status() } & 0x01 == 0 {
            return;
        }
        let _ = unsafe { read_data() };
    }
}
