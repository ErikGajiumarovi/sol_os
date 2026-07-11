use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

const IMAGE: &str = env!("SOL_OS_IMAGE");

fn main() -> ExitCode {
    let mut headless = false;
    let mut print_image = false;
    let mut monitor = None;
    let mut arguments = env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--headless" => headless = true,
            "--print-image" => print_image = true,
            "--monitor" => match arguments.next() {
                Some(path) => monitor = Some(PathBuf::from(path)),
                None => {
                    eprintln!("--monitor requires a UNIX socket path");
                    return ExitCode::from(2);
                }
            },
            "--help" | "-h" => {
                print_help();
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("unknown argument: {other}");
                print_help();
                return ExitCode::from(2);
            }
        }
    }

    if print_image {
        println!("{IMAGE}");
        return ExitCode::SUCCESS;
    }

    match launch_qemu(headless, monitor.as_deref()) {
        Ok(status) if status.success() => ExitCode::SUCCESS,
        Ok(status) => ExitCode::from(status.code().unwrap_or(1) as u8),
        Err(error) => {
            eprintln!("failed to launch QEMU: {error}");
            ExitCode::FAILURE
        }
    }
}

fn print_help() {
    println!("Usage: cargo run -- [--headless] [--monitor PATH] [--print-image]");
}

fn launch_qemu(
    headless: bool,
    monitor: Option<&Path>,
) -> std::io::Result<std::process::ExitStatus> {
    let code = firmware_path(
        "OVMF_CODE",
        &[
            "/opt/homebrew/share/qemu/edk2-x86_64-code.fd",
            "/usr/share/OVMF/OVMF_CODE.fd",
            "/usr/share/edk2/x64/OVMF_CODE.fd",
            "/usr/share/edk2/ovmf/OVMF_CODE.fd",
        ],
    )?;
    let vars_template = firmware_path(
        "OVMF_VARS",
        &[
            "/opt/homebrew/share/qemu/edk2-i386-vars.fd",
            "/usr/share/OVMF/OVMF_VARS.fd",
            "/usr/share/edk2/x64/OVMF_VARS.fd",
            "/usr/share/edk2/ovmf/OVMF_VARS.fd",
        ],
    )?;

    let build_dir = Path::new(IMAGE).parent().expect("image has no parent");
    let vars = build_dir.join("OVMF_VARS.fd");
    fs::copy(vars_template, &vars)?;

    let mut qemu = Command::new(env::var_os("QEMU").unwrap_or_else(|| "qemu-system-x86_64".into()));
    qemu.args([
        "-machine",
        "q35,accel=tcg,i8042=on,pic=on,pit=on",
        "-cpu",
        "max",
        "-smp",
        "1",
        "-m",
        "512M",
        "-drive",
    ]);
    qemu.arg(format!(
        "if=pflash,unit=0,format=raw,readonly=on,file={}",
        code.display()
    ));
    qemu.arg("-drive").arg(format!(
        "if=pflash,unit=1,format=raw,file={}",
        vars.display()
    ));
    qemu.args([
        "-drive",
        &format!("if=none,id=boot,format=raw,file={IMAGE}"),
    ]);
    qemu.args([
        "-device",
        "qemu-xhci,id=xhci",
        "-device",
        "usb-storage,drive=boot,bus=xhci.0,bootindex=1",
        "-serial",
        "stdio",
    ]);
    if let Some(monitor) = monitor {
        let _ = fs::remove_file(monitor);
        qemu.arg("-monitor")
            .arg(format!("unix:{},server=on,wait=off", monitor.display()));
    } else {
        qemu.args(["-monitor", "none"]);
    }
    if headless {
        qemu.args(["-display", "none"]);
    }
    qemu.status()
}

fn firmware_path(variable: &str, candidates: &[&str]) -> std::io::Result<PathBuf> {
    if let Some(value) = env::var_os(variable) {
        let path = PathBuf::from(value);
        if path.is_file() {
            return Ok(path);
        }
    }
    candidates
        .iter()
        .map(PathBuf::from)
        .find(|path| path.is_file())
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("{variable} firmware file was not found; set {variable}"),
            )
        })
}
