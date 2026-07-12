# Progress

Last updated: 2026-07-12

## Status convention

`[x]` means the milestone has recorded QEMU/OVMF evidence. Physical-hardware validation is listed separately because the exact computer and firmware settings have not yet been recorded.

## Milestones

- [x] 1. Bootstrap: the UEFI image booted from QEMU's `qemu-xhci` + `usb-storage` device, printed the framebuffer greeting, and reached `sol>` without a reset.
- [x] 2. CPU init: QEMU fault images produced `EXCEPTION_PAGE_FAULT`, `EXCEPTION_GENERAL_PROTECTION_FAULT`, and `EXCEPTION_DOUBLE_FAULT_IST_OK`; all three stopped cleanly in their handlers rather than rebooting.
- [x] 3. Memory management: normal boot printed `SOL_OS_M3_OK vec_len=5 box=42 heap_used=64` after mapping the 2 MiB heap and using `Vec`, `String`, and `Box`.
- [x] 4. Interrupts and keyboard: normal boot printed `SOL_OS_M4_READY`; QEMU monitor `sendkey` events entered every recorded shell command through the PS/2 IRQ queue.
- [x] 5. Text console: framebuffer text, scrolling console state, serial mirror, and global `print!`/`println!` macros rendered the boot log and shell. A QEMU `screendump` captured the framebuffer prompt and `cat` result.
- [x] 6. Storage hand-off: the vendored UEFI loader read the real `sol-data` GPT partition through firmware Block I/O before `ExitBootServices` and provided the read-only snapshot to the kernel.
- [x] 7. FAT32: the kernel mounted the FAT32 snapshot, listed root and nested directories, and read `HELLO.TXT` plus `DOCS/ABOUT.TXT`.
- [x] 8. Shell: QEMU keyboard input successfully ran `help`, `echo`, `ls`, `cat`, `clear`, `uptime`, `meminfo`, `reboot`, and `halt`.

## Build and image evidence

- `make image` (`cargo build`) completed on 2026-07-12 and wrote `build/sol-os.img`.
- `cargo fmt --all -- --check` passed.
- The page-fault, GPF, and double-fault feature images each compiled and booted under QEMU.
- The image has a protective MBR/GPT, EFI partition 1 at LBA 2048, and a 64 MiB Basic Data `sol-data` partition at LBA 10240 (131072 512-byte sectors).
- `sol-data` is FAT32 (`SOL_DATA`) and its generated root contains `DOCS` and `HELLO.TXT`.
- `build.rs` watches every current file below `disk_files/` as well as each directory, so editing a payload file causes the FAT32 volume and final image to be rebuilt.

## QEMU verification log

Environment: QEMU 11.0.2, OVMF, `q35` machine, TCG on Apple Silicon, and the image attached through `qemu-xhci` + `usb-storage`.

### Fault handlers

- `cargo run --features fault-test-page -- --headless` printed `FAULT_TEST_PAGE_BEGIN`, `EXCEPTION_PAGE_FAULT`, address `0x444444440000`, and a write fault error code.
- `cargo run --features fault-test-gpf -- --headless` printed `FAULT_TEST_GPF_BEGIN`, `EXCEPTION_GENERAL_PROTECTION_FAULT`, and error code `0x0`.
- `cargo run --features fault-test-double -- --headless` printed `FAULT_TEST_DOUBLE_BEGIN` and `EXCEPTION_DOUBLE_FAULT_IST_OK`; the stack frame showed the intentionally invalid `rsp = 0`, demonstrating delivery on the TSS's dedicated IST stack.

### Normal shell and FAT32

The normal image printed `SOL_OS_M3_OK`, mounted 131072 FAT32 sectors, printed `SOL_OS_M7_OK`, enabled PIT/PS/2 IRQ1, and reached `sol>`. Virtual PS/2 key events then produced these observed results:

- `help` listed all nine commands.
- `echo qemu` printed `qemu`.
- `ls` listed `DOCS` and `HELLO.TXT`; `ls docs` listed `ABOUT.TXT`.
- `cat hello.txt` and `cat docs/about.txt` printed the expected file contents.
- `uptime` advanced to 131 seconds / 13186 PIT ticks; `meminfo` reported usable physical memory, the 2 MiB heap, and consumed frames.
- `clear` emitted the serial clear sequence and redrew the framebuffer prompt.
- `halt` printed `CPU halted.`; QEMU remained `running` until the test harness explicitly quit it.
- `reboot` printed its controller-reset message and QEMU booted through OVMF back to a fresh `sol>` prompt.

To prove the storage path was the real GPT partition rather than an embedded fake filesystem, the already-built image was patched directly at byte 6281728, inside `sol-data` partition 2: the four bytes `real` in `HELLO.TXT` were changed to `LIVE` without rebuilding the image. The subsequent boot log and shell `cat hello.txt` both printed `Hello from the LIVE FAT32 data partition.` The exact original bytes and image SHA-256 were then restored.

No normal-shell QEMU session exhibited a triple fault, crash, or unsolicited reboot. The only intentional faults were the three dedicated fault-test images above.

## Build and run

```sh
make image
make run
```

Use the graphical QEMU window for manual keyboard testing. `make run-headless` is appropriate for serial boot logs; QEMU monitor `sendkey` commands can automate PS/2 input in a headless test.

## Physical-hardware validation

Manual validation reported a successful UEFI boot from the generated USB image, working keyboard input, and successful `ls`/`cat` use in the shell. The complete flashing procedure remains in `README.md`. Add the computer model, firmware settings, and observed command output here when available.
