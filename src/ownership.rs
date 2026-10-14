//! Module: magic_usb.ownership
//! Purpose: Restore only btusb bindings that existed before service takeover.
//! Created: 2026-10-13
//! Architecture: Root-only runtime journal and attachment-generation checks.

use crate::adapter::Adapter;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

#[derive(Debug, PartialEq, Eq)]
pub struct Ownership {
    pub vendor: u16,
    pub product: u16,
    pub bus: u16,
    pub address: u16,
    pub configuration: u16,
    pub interfaces: u8,
}

pub fn valid_port(port: &str) -> bool {
    let Some((bus, chain)) = port.split_once('-') else {
        return false;
    };
    let number = |s: &str| {
        !s.is_empty()
            && s.bytes().all(|b| b.is_ascii_digit())
            && s.parse::<u16>().is_ok_and(|n| n > 0)
    };
    number(bus) && chain.split('.').all(number)
}

fn number(root: &Path, name: &str, radix: u32) -> Result<u16, String> {
    let value = fs::read_to_string(root.join(name))
        .map_err(|e| format!("read USB ownership metadata: {e}"))?;
    u16::from_str_radix(value.trim(), radix).map_err(|_| "invalid USB ownership metadata".into())
}

impl Ownership {
    pub fn capture(root: &Path, port: &str) -> Result<Self, String> {
        if !valid_port(port) {
            return Err("invalid USB port".into());
        }
        let device = root.join(port);
        let mut state = Self {
            vendor: number(&device, "idVendor", 16)?,
            product: number(&device, "idProduct", 16)?,
            bus: number(&device, "busnum", 10)?,
            address: number(&device, "devnum", 10)?,
            configuration: number(&device, "bConfigurationValue", 10)?,
            interfaces: 0,
        };
        Adapter::identify(state.vendor, state.product)?;
        for index in [0, 1] {
            let driver = root
                .join(format!("{port}:{}.{index}", state.configuration))
                .join("driver");
            match fs::read_link(driver) {
                Ok(path) if path.file_name().is_some_and(|name| name == "btusb") => {
                    state.interfaces |= 1 << index
                }
                Ok(_) => return Err("selected adapter belongs to another USB driver".into()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                Err(e) => return Err(format!("read USB driver ownership: {e}")),
            }
        }
        Ok(state)
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        let text = format!(
            "1 {} {} {} {} {} {}\n",
            self.vendor, self.product, self.bus, self.address, self.configuration, self.interfaces
        );
        // The systemd runtime directory is root-only. Never follow a symlink or
        // overwrite an unfinished journal from a previous service invocation.
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .map_err(|e| format!("save USB ownership state: {e}"))?;
        if let Err(e) = file
            .write_all(text.as_bytes())
            .and_then(|_| file.sync_all())
        {
            let _ = fs::remove_file(path);
            return Err(format!("save USB ownership state: {e}"));
        }
        Ok(())
    }

    fn load(path: &Path) -> Result<Option<Self>, String> {
        let file = match OpenOptions::new()
            .read(true)
            .custom_flags(0o400000)
            .open(path)
        {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("read USB ownership state: {e}")),
        };
        let mut text = String::new();
        file.take(257)
            .read_to_string(&mut text)
            .map_err(|e| format!("read USB ownership state: {e}"))?;
        if text.len() > 256 {
            return Err("invalid USB ownership state".into());
        }
        let values = text
            .split_whitespace()
            .map(str::parse::<u16>)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| "invalid USB ownership state")?;
        if values.len() != 7 || values[0] != 1 || values[6] > 3 {
            return Err("invalid USB ownership state".into());
        }
        Adapter::identify(values[1], values[2])?;
        Ok(Some(Self {
            vendor: values[1],
            product: values[2],
            bus: values[3],
            address: values[4],
            configuration: values[5],
            interfaces: values[6] as u8,
        }))
    }

    fn same_attachment(&self, root: &Path, port: &str) -> bool {
        let device = root.join(port);
        [
            ("idVendor", 16, self.vendor),
            ("idProduct", 16, self.product),
            ("busnum", 10, self.bus),
            ("devnum", 10, self.address),
            ("bConfigurationValue", 10, self.configuration),
        ]
        .into_iter()
        .all(|(name, radix, value)| number(&device, name, radix) == Ok(value))
    }
}

pub fn state_path(port: &str) -> Result<PathBuf, String> {
    if !valid_port(port) {
        return Err("invalid USB port".into());
    }
    Ok(Path::new("/run")
        .join(format!("magic-usb-{port}"))
        .join("ownership"))
}

/// No journal or no previous btusb owner means no binding operation. A changed
/// attachment, another owner, or an unloaded btusb module is also left alone.
pub fn restore_in(
    root: &Path,
    port: &str,
    state_path: &Path,
    bind_path: &Path,
    mut bind: impl FnMut(&str) -> Result<(), String>,
) -> Result<(), String> {
    if !valid_port(port) {
        return Err("invalid USB port".into());
    }
    let Some(state) = Ownership::load(state_path)? else {
        return Ok(());
    };
    if state.interfaces != 0 && bind_path.exists() && state.same_attachment(root, port) {
        for index in [0, 1] {
            if state.interfaces & (1 << index) == 0 {
                continue;
            }
            let interface = format!("{port}:{}.{index}", state.configuration);
            let path = root.join(&interface);
            if !path.exists() || path.join("driver").exists() {
                continue;
            }
            if !state.same_attachment(root, port) {
                break;
            }
            if let Err(error) = bind(&interface) {
                if !path.exists() || path.join("driver").exists() {
                    continue;
                }
                return Err(error);
            }
        }
    }
    fs::remove_file(state_path).map_err(|e| format!("remove USB ownership state: {e}"))
}

#[cfg(test)]
#[path = "../tests/ownership/mod.rs"]
mod tests;
