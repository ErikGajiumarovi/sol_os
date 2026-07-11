use x86_64::instructions::port::Port;

pub const PIC_1_OFFSET: u8 = 32;
pub const PIC_2_OFFSET: u8 = PIC_1_OFFSET + 8;
pub const TIMER_INTERRUPT_ID: u8 = PIC_1_OFFSET;
pub const KEYBOARD_INTERRUPT_ID: u8 = PIC_1_OFFSET + 1;

const PIC_1_COMMAND: u16 = 0x20;
const PIC_1_DATA: u16 = 0x21;
const PIC_2_COMMAND: u16 = 0xa0;
const PIC_2_DATA: u16 = 0xa1;
const PIC_EOI: u8 = 0x20;

/// Remap the legacy 8259 PICs away from CPU exception vectors and unmask only
/// IRQ0 (PIT) and IRQ1 (PS/2 keyboard). Keeping every other line masked makes
/// early interrupt bring-up deterministic on QEMU and real hardware.
pub unsafe fn initialize() {
    // SAFETY: the kernel owns the legacy PIC ports after boot services exit;
    // interrupts are still disabled while their vector table is being installed.
    unsafe {
        write_port(PIC_1_COMMAND, 0x11);
        write_port(PIC_2_COMMAND, 0x11);
        write_port(PIC_1_DATA, PIC_1_OFFSET);
        write_port(PIC_2_DATA, PIC_2_OFFSET);
        write_port(PIC_1_DATA, 0x04);
        write_port(PIC_2_DATA, 0x02);
        write_port(PIC_1_DATA, 0x01);
        write_port(PIC_2_DATA, 0x01);

        // IRQ0/IRQ1 enabled on the master; the slave remains fully masked.
        write_port(PIC_1_DATA, 0b1111_1100);
        write_port(PIC_2_DATA, 0xff);
    }
}

/// Acknowledge an IRQ after its handler has consumed device state.
pub fn end_of_interrupt(interrupt_id: u8) {
    // SAFETY: only the active interrupt handlers call this, and writing EOI is
    // the documented acknowledgement operation for the remapped 8259 PICs.
    unsafe {
        if interrupt_id >= PIC_2_OFFSET {
            write_port(PIC_2_COMMAND, PIC_EOI);
        }
        write_port(PIC_1_COMMAND, PIC_EOI);
    }
}

unsafe fn write_port(port: u16, value: u8) {
    // SAFETY: callers select only a PIC command/data port owned by this module.
    unsafe { Port::<u8>::new(port).write(value) };
}
