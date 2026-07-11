use spin::Once;
use x86_64::VirtAddr;
use x86_64::instructions::segmentation::{CS, DS, ES, SS, Segment};
use x86_64::instructions::tables::load_tss;
use x86_64::structures::gdt::{Descriptor, GlobalDescriptorTable, SegmentSelector};
use x86_64::structures::tss::TaskStateSegment;

pub const DOUBLE_FAULT_IST_INDEX: u16 = 0;
const DOUBLE_FAULT_STACK_SIZE: usize = 5 * 4096;

#[repr(align(16))]
struct InterruptStack {
    _bytes: [u8; DOUBLE_FAULT_STACK_SIZE],
}

static mut DOUBLE_FAULT_STACK: InterruptStack = InterruptStack {
    _bytes: [0; DOUBLE_FAULT_STACK_SIZE],
};
static TSS: Once<TaskStateSegment> = Once::new();
static GDT: Once<(GlobalDescriptorTable, Selectors)> = Once::new();

struct Selectors {
    code: SegmentSelector,
    data: SegmentSelector,
    tss: SegmentSelector,
}

pub fn init() {
    let tss = TSS.call_once(|| {
        let mut tss = TaskStateSegment::new();
        // Taking a raw pointer does not borrow the mutable static. The stack is reserved once,
        // never aliased as Rust data, and is used only by the CPU during double-fault delivery.
        let stack_start = VirtAddr::from_ptr(&raw const DOUBLE_FAULT_STACK);
        let stack_end = stack_start + DOUBLE_FAULT_STACK_SIZE as u64;
        tss.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] = stack_end;
        tss
    });

    let (gdt, selectors) = GDT.call_once(|| {
        let mut gdt = GlobalDescriptorTable::new();
        let code = gdt.append(Descriptor::kernel_code_segment());
        let data = gdt.append(Descriptor::kernel_data_segment());
        let tss = gdt.append(Descriptor::tss_segment(tss));
        (gdt, Selectors { code, data, tss })
    });

    gdt.load();
    // SAFETY: Both selectors were just created from the static GDT, and the referenced TSS
    // remains alive for the lifetime of the kernel.
    unsafe {
        CS::set_reg(selectors.code);
        DS::set_reg(selectors.data);
        ES::set_reg(selectors.data);
        SS::set_reg(selectors.data);
        load_tss(selectors.tss);
    }
}
