use bootloader_api::info::{MemoryRegion, MemoryRegionKind, MemoryRegions};
use x86_64::PhysAddr;
use x86_64::VirtAddr;
use x86_64::registers::control::Cr3;
use x86_64::structures::paging::{
    FrameAllocator, Mapper, OffsetPageTable, Page, PageSize, PageTableFlags, PhysFrame, Size4KiB,
    mapper::MapToError, page_table::PageTable,
};

use crate::allocator::ALLOCATOR;

/// An unused canonical virtual range reserved for dynamically allocated Rust values.
pub const HEAP_START: usize = 0x_5555_0000_0000;
pub const HEAP_SIZE: usize = 2 * 1024 * 1024;

/// Creates a mapper for the active page tables using the bootloader's HHDM.
///
/// # Safety
///
/// The caller must pass the physical-memory offset supplied by the same
/// bootloader that created the active page tables. The returned mapper provides
/// mutable access to those page tables and must not be duplicated.
pub unsafe fn init(physical_memory_offset: VirtAddr) -> OffsetPageTable<'static> {
    let level_4_table = unsafe { active_level_4_table(physical_memory_offset) };
    unsafe { OffsetPageTable::new(level_4_table, physical_memory_offset) }
}

unsafe fn active_level_4_table(physical_memory_offset: VirtAddr) -> &'static mut PageTable {
    let (level_4_table_frame, _) = Cr3::read();
    let physical_address = level_4_table_frame.start_address();
    let virtual_address = physical_memory_offset + physical_address.as_u64();
    let page_table_ptr: *mut PageTable = virtual_address.as_mut_ptr();

    // SAFETY: the HHDM translates the CR3 physical address to the active L4
    // table. This reference is kept exclusively inside the mapper during setup.
    unsafe { &mut *page_table_ptr }
}

/// A monotonic allocator over memory ranges marked `Usable` by UEFI.
pub struct BootInfoFrameAllocator {
    memory_regions: &'static [MemoryRegion],
    next: usize,
}

impl BootInfoFrameAllocator {
    /// # Safety
    ///
    /// The supplied map must come from `BootInfo` and remain valid for the
    /// kernel lifetime. No other allocator may hand out its usable frames.
    pub unsafe fn init(memory_regions: &'static MemoryRegions) -> Self {
        Self {
            memory_regions: &memory_regions[..],
            next: 0,
        }
    }

    pub fn usable_frames(&self) -> impl Iterator<Item = PhysFrame> + '_ {
        self.memory_regions
            .iter()
            .filter(|region| region.kind == MemoryRegionKind::Usable)
            .flat_map(|region| {
                let start = align_up(region.start, Size4KiB::SIZE);
                let end = region.end & !(Size4KiB::SIZE - 1);
                (start..end).step_by(Size4KiB::SIZE as usize)
            })
            .map(|address| PhysFrame::containing_address(PhysAddr::new(address)))
    }

    pub fn allocated_frames(&self) -> usize {
        self.next
    }

    pub fn usable_frame_count(&self) -> usize {
        self.usable_frames().count()
    }
}

unsafe impl FrameAllocator<Size4KiB> for BootInfoFrameAllocator {
    fn allocate_frame(&mut self) -> Option<PhysFrame> {
        let frame = self.usable_frames().nth(self.next);
        self.next += 1;
        frame
    }
}

/// Maps the initial heap and enables the global allocator.
pub fn init_heap(
    mapper: &mut OffsetPageTable<'static>,
    frame_allocator: &mut BootInfoFrameAllocator,
) -> Result<(), MapToError<Size4KiB>> {
    let heap_start = VirtAddr::new(HEAP_START as u64);
    let heap_end = heap_start + (HEAP_SIZE - 1) as u64;
    let start_page = Page::containing_address(heap_start);
    let end_page = Page::containing_address(heap_end);
    let page_range = Page::range_inclusive(start_page, end_page);

    for page in page_range {
        let frame = frame_allocator
            .allocate_frame()
            .ok_or(MapToError::FrameAllocationFailed)?;
        // Heap data never needs instruction fetch permission. Keeping NX set
        // catches accidental jumps through corrupted heap pointers instead of
        // treating writable data as executable code.
        let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE;
        // SAFETY: every heap page receives a distinct frame returned from the
        // UEFI `Usable` set, and the range was reserved exclusively for this heap.
        unsafe { mapper.map_to(page, frame, flags, frame_allocator) }?.flush();
    }

    // SAFETY: the page range above is now present and writable, and this is the
    // sole initialization point for the global allocator.
    unsafe { ALLOCATOR.init(HEAP_START, HEAP_SIZE) };
    Ok(())
}

const fn align_up(value: u64, alignment: u64) -> u64 {
    (value + alignment - 1) & !(alignment - 1)
}
