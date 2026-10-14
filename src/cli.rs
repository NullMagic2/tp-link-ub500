//! Module: magic-usb::main
//! Purpose: Command line for the relay service and a raw HCI probe.
//! Created: 2026-10-09
//! Architecture: Parses arguments; usb, hci and bridge own the work.

use crate::{STOP, access_error, bridge, hci, usb};

use std::os::raw::c_int;
use std::os::unix::fs::FileTypeExt;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

const USAGE: &str = "usage: magic-usb --version\n       magic-usb check\n       magic-usb serve USB-PORT [--seconds=N] [--capture=NEW-FILE] [--wait-for-device]\n       magic-usb probe USB-PORT OPCODE-HEX [PARAMETERS-HEX]\n       capture is off by default; explicit btsnoop capture stops at 16 MiB (btmon -r)";

unsafe extern "C" {
    fn signal(number: c_int, handler: extern "C" fn(c_int)) -> usize;
}

extern "C" fn stop(_number: c_int) {
    STOP.store(true, Ordering::Relaxed);
}

fn install_signals() -> Result<(), String> {
    for number in [1, 2, 15] {
        // SIGHUP, SIGINT, SIGTERM. The handler only touches an atomic.
        if unsafe { signal(number, stop) } == usize::MAX {
            return Err(format!(
                "install signal handler: {}",
                std::io::Error::last_os_error()
            ));
        }
    }
    Ok(())
}

fn hex(text: &str) -> Result<Vec<u8>, String> {
    if text.len() % 2 != 0 {
        return Err(format!("odd-length hex: {text}"));
    }
    (0..text.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&text[i..i + 2], 16).map_err(|_| format!("invalid hex: {text}"))
        })
        .collect()
}

/// Sends one command and prints events until its Complete or Status reply arrives.
fn probe(device: &usb::Device, opcode: u16, mut command: Vec<u8>) -> Result<(), String> {
    println!("TX {command:02x?}");
    device.command(&mut command)?;
    let mut framer = hci::Framer::new(hci::Kind::Event);
    let mut buffer = [0; usb::EVENT_BYTES];
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(3) && !STOP.load(Ordering::Relaxed) {
        let count = device.events(&mut buffer, 250)?;
        for event in framer.push(&buffer[..count]) {
            println!("RX {}ms {event:02x?}", start.elapsed().as_millis());
            if let Some(result) = hci::probe_reply(&event, opcode) {
                println!("{}", result?);
                return Ok(());
            }
        }
    }
    Err(format!(
        "opcode {opcode:#06x}: no matching Complete or Status reply within 3 s"
    ))
}

/// Read-only environment information; neither opens USB/VHCI nor infers support.
fn check_environment() -> Result<(), String> {
    let kernel = std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .map_err(|e| format!("read kernel release: {e}"))?;
    let os = std::fs::read_to_string("/etc/os-release")
        .map_err(|e| format!("read /etc/os-release: {e}"))?;
    let name = os
        .lines()
        .find_map(|line| line.strip_prefix("PRETTY_NAME="))
        .unwrap_or("unknown");
    println!(
        "Host: {} · kernel {} · {}",
        name.trim_matches('"'),
        kernel.trim(),
        std::env::consts::ARCH
    );
    let vhci = std::fs::metadata("/dev/vhci").map_err(|e| {
        access_error(
            "/dev/vhci",
            e,
            "try sudo modprobe hci_vhci; the distro kernel must provide CONFIG_BT_HCIVHCI",
        )
    })?;
    if !vhci.file_type().is_char_device() {
        return Err("/dev/vhci is not a character device".into());
    }
    println!(
        "VHCI node present; access and controller initialization not tested. No CONFIG_RUST required."
    );
    Ok(())
}

pub fn run() -> Result<(), String> {
    run_for(None)
}

pub fn run_for(required: Option<crate::adapter::Adapter>) -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() == 2 && args[1] == "--version" {
        println!("magic-usb {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if args.get(1).map(String::as_str) == Some("check") {
        return if args.len() == 2 {
            check_environment()
        } else {
            Err(USAGE.into())
        };
    }
    let (Some(verb), Some(port)) = (args.get(1), args.get(2)) else {
        return Err(USAGE.into());
    };
    match verb.as_str() {
        "serve" => {
            let mut seconds = None;
            let mut capture = None;
            let mut wait = false;
            for argument in &args[3..] {
                if let Some(value) = argument.strip_prefix("--seconds=") {
                    if seconds.is_some() {
                        return Err("duplicate --seconds".into());
                    }
                    seconds = Some(value.parse().map_err(|_| "invalid --seconds")?);
                } else if let Some(value) = argument.strip_prefix("--capture=") {
                    if capture.is_some() || value.is_empty() {
                        return Err("invalid or duplicate --capture".into());
                    }
                    capture = Some(value);
                } else if argument == "--wait-for-device" && !wait {
                    wait = true;
                } else {
                    return Err(USAGE.into());
                }
            }
            install_signals()?;
            if wait && !crate::hotplug::wait(port, required, seconds)? {
                return Ok(());
            }
            bridge::serve_as(port, seconds, capture, required)
        }
        "restore" if args.len() == 3 => crate::usb::restore(port),
        "initialize" if args.len() == 3 => {
            install_signals()?;
            let device = usb::Device::open_as(port, required)?;
            let result = crate::realtek::initialize(&device);
            device.finish(result)
        }
        "probe" if (4..=5).contains(&args.len()) => {
            let opcode =
                u16::from_str_radix(args.get(3).ok_or(USAGE)?, 16).map_err(|_| "invalid opcode")?;
            let parameters = hex(args.get(4).map_or("", String::as_str))?;
            let command = hci::command(opcode, &parameters)?;
            install_signals()?;
            let device = usb::Device::open_as(port, required)?;
            let result = probe(&device, opcode, command);
            device.finish(result)
        }
        _ => Err(USAGE.into()),
    }
}

#[cfg(test)]
#[path = "../tests/main/mod.rs"]
mod tests;
