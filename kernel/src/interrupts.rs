use spin::Once;
use x86_64::registers::control::Cr2;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};

static IDT: Once<InterruptDescriptorTable> = Once::new();

pub fn init() {
    let idt = IDT.call_once(|| {
        let mut idt = InterruptDescriptorTable::new();
        idt.breakpoint.set_handler_fn(breakpoint_handler);
        idt[crate::pic::TIMER_INTERRUPT_ID].set_handler_fn(timer_interrupt_handler);
        idt[crate::pic::KEYBOARD_INTERRUPT_ID].set_handler_fn(keyboard_interrupt_handler);
        idt.general_protection_fault
            .set_handler_fn(general_protection_fault_handler);
        idt.page_fault.set_handler_fn(page_fault_handler);
        // SAFETY: GDT initialization installs a TSS whose slot at this index points to a
        // dedicated, aligned, static stack before this IDT is loaded.
        unsafe {
            idt.double_fault
                .set_handler_fn(double_fault_handler)
                .set_stack_index(crate::gdt::DOUBLE_FAULT_IST_INDEX);
        }
        idt
    });
    idt.load();
}

/// Enable the legacy hardware interrupts only after GDT, IDT, page tables, and
/// the allocator are ready. Device handlers never allocate or print.
pub fn initialize_hardware() {
    // SAFETY: the IDT has handlers at both remapped PIC vectors and mask setup
    // completes before `sti` makes an IRQ observable by the CPU.
    unsafe { crate::pic::initialize() };
    crate::timer::initialize();
    x86_64::instructions::interrupts::enable();
}

extern "x86-interrupt" fn breakpoint_handler(stack_frame: InterruptStackFrame) {
    crate::serial_println!(
        "EXCEPTION_BREAKPOINT_OK ip={:?}",
        stack_frame.instruction_pointer
    );
}

extern "x86-interrupt" fn timer_interrupt_handler(_stack_frame: InterruptStackFrame) {
    crate::timer::tick();
    crate::pic::end_of_interrupt(crate::pic::TIMER_INTERRUPT_ID);
}

extern "x86-interrupt" fn keyboard_interrupt_handler(_stack_frame: InterruptStackFrame) {
    // SAFETY: port 0x60 is the PS/2 data register. Reading it acknowledges the
    // controller byte before the PIC receives its end-of-interrupt command.
    let scancode = unsafe { x86_64::instructions::port::Port::<u8>::new(0x60).read() };
    crate::keyboard::handle_scancode(scancode);
    crate::pic::end_of_interrupt(crate::pic::KEYBOARD_INTERRUPT_ID);
}

extern "x86-interrupt" fn general_protection_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: u64,
) {
    x86_64::instructions::interrupts::disable();
    crate::serial_println!("EXCEPTION_GENERAL_PROTECTION_FAULT");
    crate::println!("\nGENERAL PROTECTION FAULT");
    crate::println!("error code: {error_code:#x}");
    crate::println!("{stack_frame:#?}");
    crate::hlt_loop();
}

extern "x86-interrupt" fn page_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: PageFaultErrorCode,
) {
    x86_64::instructions::interrupts::disable();
    crate::serial_println!("EXCEPTION_PAGE_FAULT");
    crate::println!("\nPAGE FAULT");
    crate::println!("address: {:#x}", Cr2::read_raw());
    crate::println!("error: {error_code:?}");
    crate::println!("{stack_frame:#?}");
    crate::hlt_loop();
}

extern "x86-interrupt" fn double_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: u64,
) -> ! {
    x86_64::instructions::interrupts::disable();
    crate::serial_println!("EXCEPTION_DOUBLE_FAULT_IST_OK");
    crate::println!("\nDOUBLE FAULT (dedicated IST stack)");
    crate::println!("error code: {error_code:#x}");
    crate::println!("{stack_frame:#?}");
    crate::hlt_loop();
}
