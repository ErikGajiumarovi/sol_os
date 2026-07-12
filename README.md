# Sol OS

Sol OS is a small x86_64 hobby operating system written in `#![no_std]` Rust. It supports UEFI boot only; there is no legacy BIOS path.

## Boot and storage design

Sol OS uses rust-osdev's `bootloader` 0.11.15 because it keeps the UEFI boot path in Rust/Cargo, provides the framebuffer and firmware memory map, and avoids a handwritten UEFI loader. The project carries a small, auditable fork at `vendor/bootloader` for one extra hand-off step.

The resulting disk is a GPT image with two partitions:

- `sol-boot`: an EFI System Partition containing the bootloader and kernel.
- `sol-data`: a 64 MiB FAT32 Basic Data partition containing the files from `disk_files/`.

Before `ExitBootServices`, the fork locates the single `sol-data` partition by its GPT name and Basic Data GUID, reads it through UEFI Block I/O, and passes that read-only snapshot through the bootloader's existing ramdisk fields. The kernel's `RamDiskBlockDevice` and FAT32 reader therefore consume bytes that came from the real FAT32 partition on the boot USB, rather than a filesystem embedded in the EFI partition at build time.

This is deliberately a boot-time snapshot, not a native post-boot USB/xHCI storage driver: it uses 64 MiB of RAM and cannot observe USB changes made after boot. That tradeoff is the current least-complex UEFI hand-off while the kernel remains `no_std`. The QEMU evidence is recorded in `PROGRESS.md`; the physical-laptop procedure below still needs to be performed and recorded by the person with the target hardware.

## Project layout

- `build.rs` builds `disk_files/` into FAT32 and assembles `build/sol-os.img`.
- `src/main.rs` is the host-side QEMU launcher.
- `kernel/` is the `no_std`, `no_main` x86_64 kernel: interrupt setup, memory/heap, framebuffer console, PS/2 input, FAT32 reader, and shell.
- `disk_files/` supplies example files for the FAT32 data partition.
- `vendor/bootloader/` is the local bootloader fork described above.

## Build

The pinned nightly toolchain in `rust-toolchain.toml` installs the required Rust target and components. Install `qemu-system-x86_64` and OVMF separately for running the image.

```sh
make image
```

This is the documented one-command build. It runs `cargo build` and writes `build/sol-os.img`. The current image is a 71 MiB GPT disk image: its EFI partition starts at LBA 2048 and its `sol-data` FAT32 partition starts at LBA 10240 with 131072 sectors.

## Run in QEMU

```sh
make run             # framebuffer window plus COM1 on the terminal
make run-headless    # COM1 only
```

The launcher attaches the image as `qemu-xhci` + `usb-storage`, not as a virtual IDE disk. Set `QEMU`, `OVMF_CODE`, or `OVMF_VARS` if automatic host-path detection does not find the local QEMU/OVMF installation. Run only one QEMU instance per image at a time because QEMU takes a write lock on the image and writable OVMF variables file.

Use the graphical `make run` window for PS/2 keyboard testing. Click the window to focus it after the `sol>` prompt appears, then exercise the shell in this order:

```text
help
echo hello from Sol OS
ls
ls DOCS
cat HELLO.TXT
cat DOCS/ABOUT.TXT
uptime
meminfo
clear
halt
```

The shell accepts ASCII input up to 80 bytes. Use Left/Right, Home/End,
Backspace, and Delete to edit the current command. Up/Down browse the last 16
non-empty commands; pressing Down after the newest entry restores the unfinished
command that was present before history navigation. The graphical framebuffer
shows the current input position with a solid cursor.

Run `reboot` in a separate session because it resets the guest and starts a fresh boot. `make run-headless` is useful for boot/serial logging, but it has no interactive graphical keyboard; use the graphical run for manual input testing or QEMU's monitor `sendkey` commands for automation.

## Flashing a real USB drive

**This destroys data on the selected drive.** Use a spare USB drive, identify it by model and capacity, and never substitute an internal system disk.

1. Build the image with `make image` and close every QEMU instance using it.
2. Write the complete image to the USB device, not to a partition.

   macOS example (replace `diskN` only after checking `diskutil list`):

   ```sh
   diskutil list
   diskutil unmountDisk /dev/diskN
   sudo dd if=build/sol-os.img of=/dev/rdiskN bs=1m
   sync
   diskutil eject /dev/diskN
   ```

   Linux example (replace `sdX` only after checking `lsblk` and unmounting every mounted partition on that USB drive):

   ```sh
   lsblk -o NAME,SIZE,MODEL,TRAN
   sudo dd if=build/sol-os.img of=/dev/sdX bs=4M conv=fsync status=progress
   sudo sgdisk -e /dev/sdX
   sync
   ```

   The image is smaller than most USB drives. `sgdisk -e` relocates its backup GPT to the end of a larger Linux target; it does not format or expand either Sol OS partition. If another host reports backup-GPT geometry after flashing, repair/relocate the backup GPT without creating or formatting partitions.

3. Eject the drive, boot the laptop in UEFI mode, and disable Secure Boot unless a signed boot path has been added. Select the USB's UEFI entry; do not use legacy/CSM boot.
4. Record the exact hardware, firmware settings, boot result, keyboard behavior, and `ls`/`cat` output in `PROGRESS.md` before marking hardware verification complete.

See [PROGRESS.md](PROGRESS.md) for the distinction between compiled implementation and verified runtime milestones.
