use core::alloc::{GlobalAlloc, Layout};
use core::ptr;
use core::sync::atomic::{AtomicUsize, Ordering};

/// A deliberately small bump allocator for the kernel's first heap.
///
/// It never reuses freed blocks. That is a useful trade-off while bringing up a
/// kernel: allocation has no metadata or pointer chasing, and the allocator can
/// be safely used from the single core even after interrupts are enabled. The
/// shell keeps its allocations bounded; a reusable allocator can replace this
/// implementation without changing callers.
pub struct BumpAllocator {
    start: AtomicUsize,
    end: AtomicUsize,
    next: AtomicUsize,
}

impl BumpAllocator {
    pub const fn new() -> Self {
        Self {
            start: AtomicUsize::new(0),
            end: AtomicUsize::new(0),
            next: AtomicUsize::new(0),
        }
    }

    /// # Safety
    ///
    /// `heap_start..heap_start + heap_size` must be mapped, writable, and not
    /// aliased by any other Rust allocation. Initialization must happen once,
    /// before any allocation is attempted.
    pub unsafe fn init(&self, heap_start: usize, heap_size: usize) {
        let heap_end = heap_start
            .checked_add(heap_size)
            .expect("heap address range overflow");
        assert_eq!(
            self.start.load(Ordering::Relaxed),
            0,
            "heap initialized twice"
        );
        self.start.store(heap_start, Ordering::Relaxed);
        self.end.store(heap_end, Ordering::Relaxed);
        self.next.store(heap_start, Ordering::Relaxed);
    }

    pub fn capacity(&self) -> usize {
        self.end
            .load(Ordering::Relaxed)
            .saturating_sub(self.start.load(Ordering::Relaxed))
    }

    pub fn used(&self) -> usize {
        self.next
            .load(Ordering::Relaxed)
            .saturating_sub(self.start.load(Ordering::Relaxed))
    }
}

unsafe impl GlobalAlloc for BumpAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let end = self.end.load(Ordering::Acquire);
        let align_mask = layout.align() - 1;

        loop {
            let current = self.next.load(Ordering::Relaxed);
            let aligned = match current.checked_add(align_mask) {
                Some(value) => value & !align_mask,
                None => return ptr::null_mut(),
            };
            let allocation_end = match aligned.checked_add(layout.size()) {
                Some(value) => value,
                None => return ptr::null_mut(),
            };
            if current == 0 || allocation_end > end {
                return ptr::null_mut();
            }

            if self
                .next
                .compare_exchange_weak(current, allocation_end, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
            {
                return aligned as *mut u8;
            }
        }
    }

    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {
        // Bump allocation deliberately releases all memory only at reboot.
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let allocation = unsafe { self.alloc(layout) };
        if !allocation.is_null() {
            // SAFETY: `allocation` denotes `layout.size()` bytes just reserved
            // by `alloc`, so clearing exactly that range stays in-bounds.
            unsafe { allocation.write_bytes(0, layout.size()) };
        }
        allocation
    }
}

#[global_allocator]
pub static ALLOCATOR: BumpAllocator = BumpAllocator::new();
