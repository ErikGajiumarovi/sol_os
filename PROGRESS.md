# Progress

Last updated: 2026-07-11

## Milestones

- [x] 1. Bootstrap: UEFI boot, framebuffer greeting, clean halt
- [ ] 2. CPU init: GDT, IDT, double-fault IST, page fault, and general-protection fault handlers
- [ ] 3. Memory management: frame allocator, paging, heap, and `alloc`
- [ ] 4. Interrupts and keyboard: interrupt controller, timer, PS/2 input queue
- [ ] 5. Scrolling framebuffer console and global print macros
- [ ] 6. Raw USB mass-storage sectors
- [ ] 7. GPT/FAT32 mount, directory listing, and file reads
- [ ] 8. Interactive shell and required commands

## Host and toolchain

- Development host: Apple Silicon macOS; x86_64 QEMU therefore uses TCG emulation.
- Rust: pinned nightly with `rust-src`, `llvm-tools-preview`, and `x86_64-unknown-none`.
- Boot: rust-osdev `bootloader` 0.11.15, UEFI-only image.
- Disk layout: GPT with an EFI system partition plus a 64 MiB FAT32 `sol-data` partition.

## Build and run

```sh
make image          # writes build/sol-os.img
make run            # graphical QEMU plus serial output
make run-headless   # serial-only QEMU
```

## Verification log

### Milestone 1 — verified 2026-07-11

- `cargo build` completed and wrote `build/sol-os.img`.
- macOS `gpt -r show` reported a valid protective MBR/GPT, EFI partition 1, and Microsoft basic-data partition 2.
- The sector at data-partition LBA 8192 was identified as FAT32 with 131072 sectors, label `SOL_DATA`, and 1009 sectors per FAT.
- QEMU 11.0.2 booted the image through OVMF from `qemu-xhci` + `usb-storage` and emitted `SOL_OS_M1_OK` over COM1.
- A QEMU framebuffer capture showed all three greeting lines on the graphical console.
- QEMU remained running at the clean `hlt` loop until the test harness stopped it; there was no reboot or triple fault.
