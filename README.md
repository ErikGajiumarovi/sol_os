# Sol OS

Sol OS is a small x86_64 hobby operating system written in `#![no_std]` Rust. It targets UEFI firmware only.

## Boot architecture

The project uses rust-osdev's `bootloader` crate (0.11.15). It keeps the boot path entirely in Rust/Cargo, supplies the UEFI framebuffer and firmware memory map to the kernel, and avoids maintaining a handwritten UEFI loader. The final image is GPT-partitioned: the first partition is the bootloader's EFI system partition and the second is a real FAT32 data volume intended for the kernel's storage stack.

## Build and run

The current development host uses Rust nightly and QEMU 11 with OVMF. Build the bootable image with:

```sh
make image
```

The result is `build/sol-os.img`. Run it as a removable USB mass-storage device with either `make run` (graphical framebuffer plus serial) or `make run-headless` (serial only). Set `OVMF_CODE`, `OVMF_VARS`, or `QEMU` if automatic host-path detection does not match your installation.

## Current checkpoint

Milestone 1 is verified under QEMU 11.0.2 with OVMF. Both the captured framebuffer and COM1 log contain:

```text
Hello from kernel
Sol OS milestone 1: x86_64 UEFI framebuffer online
The kernel is halted cleanly.
```

The CPU then remains in `hlt` without rebooting or triple-faulting. See [PROGRESS.md](PROGRESS.md) for the remaining milestones and verification evidence.
