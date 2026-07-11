use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

const IMAGE: &str = env!("SOL_OS_IMAGE");

fn main() -> ExitCode {
    let mut headless = false;
    let mut print_image = false;
    for argument in env::args().skip(1) {
        match argument.as_str() {
            "--headless" => headless = true,
            "--print-image" => print_image = true,
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

    match launch_qemu(headless) {
        Ok(status) if status.success() => ExitCode::SUCCESS,
        Ok(status) => ExitCode::from(status.code().unwrap_or(1) as u8),
        Err(error) => {
            eprintln!("failed to launch QEMU: {error}");
            ExitCode::FAILURE
        }
    }
}

fn print_help() {
    println!("Usage: cargo run -- [--headless] [--print-image]");
}

fn launch_qemu(headless: bool) -> std::io::Result<std::process::ExitStatus> {
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
        "q35,accel=tcg",
        "-cpu",
        "max",
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
        "-monitor",
        "none",
        "-no-reboot",
        "-no-shutdown",
    ]);
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
