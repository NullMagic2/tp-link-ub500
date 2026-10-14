//! Module: magic_usb.realtek
//! Purpose: Initialize the UB 500's RTL8761BU using distro firmware.
//! Created: 2026-10-13
//! Architecture: Checked epatch parsing and synchronous HCI before VHCI starts.
//! Protocol reference: Linux btrtl.c; no firmware is bundled.

use crate::{STOP, adapter::Adapter, hci, usb::Device};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

const FRAGMENT_BYTES: usize = 252;
const MAX_FIRMWARE_BYTES: usize = 1024 * 1024;

/// This runs before relay threads; only one command is outstanding.
fn command(device: &Device, opcode: u16, parameters: &[u8]) -> Result<Vec<u8>, String> {
    device.command(&mut hci::command(opcode, parameters)?)?;
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut framer = hci::Framer::new(hci::Kind::Event);
    let mut buffer = [0u8; 272];
    while Instant::now() < deadline && !STOP.load(Ordering::Relaxed) {
        let count = device.events(&mut buffer, 100)?;
        for event in framer.push(&buffer[..count]) {
            if !hci::valid_packet(hci::EVENT, &event) {
                return Err("malformed firmware event".into());
            }
            if hci::completed_opcode(&event) == Some(opcode) {
                if event.len() < 6 || event[2] == 0 || event[5] != 0 {
                    return Err(format!("firmware command {opcode:#06x} rejected"));
                }
                return Ok(event[5..].to_vec());
            }
            if let Some(reply) = hci::probe_reply(&event, opcode) {
                reply?;
            }
        }
    }
    Err(format!(
        "firmware command {opcode:#06x} timed out; not replayed"
    ))
}

pub fn patch(firmware: &[u8], rom: u8, config: &[u8]) -> Result<Vec<u8>, String> {
    let bad = || "invalid or incompatible RTL8761BU firmware".to_string();
    if firmware.len() < 21
        || firmware.len() > MAX_FIRMWARE_BYTES
        || &firmware[..8] != b"Realtech"
        || firmware[firmware.len() - 4..] != [0x51, 4, 0xfd, 0x77]
    {
        return Err(bad());
    }
    let patches = usize::from(u16::from_le_bytes([firmware[12], firmware[13]]));
    let table_end = 14 + patches * 8;
    if table_end > firmware.len() - 7 || patches == 0 {
        return Err(bad());
    }
    // Extension instructions are encoded backwards, with opcode and length at the end.
    let mut cursor = firmware.len() - 4;
    let mut project = None;
    while cursor >= table_end + 3 {
        let opcode = firmware[cursor - 1];
        let len = usize::from(firmware[cursor - 2]);
        if opcode == 0xff {
            break;
        }
        if len == 0 || cursor < table_end + 2 + len {
            return Err(bad());
        }
        if opcode == 0 && len == 1 {
            project = Some(firmware[cursor - 3]);
            break;
        }
        cursor -= len + 2;
    }
    if project != Some(14) {
        return Err(bad());
    }
    let mut selected = None;
    for index in 0..patches {
        let at = 14 + 2 * index;
        if u16::from_le_bytes([firmware[at], firmware[at + 1]]) != u16::from(rom) + 1 {
            continue;
        }
        if selected.is_some() {
            return Err(bad());
        }
        let length_at = 14 + 2 * patches + 2 * index;
        let offset_at = 14 + 4 * patches + 4 * index;
        let len = usize::from(u16::from_le_bytes([
            firmware[length_at],
            firmware[length_at + 1],
        ]));
        let at =
            u32::from_le_bytes(firmware[offset_at..offset_at + 4].try_into().unwrap()) as usize;
        if len < 4
            || at < table_end
            || at
                .checked_add(len)
                .is_none_or(|end| end > firmware.len() - 7)
        {
            return Err(bad());
        }
        selected = Some((at, len));
    }
    let (at, len) = selected.ok_or_else(bad)?;
    if !config.is_empty()
        && (config.len() < 6
            || config[..4] != [0x55, 0xab, 0x23, 0x87]
            || usize::from(u16::from_le_bytes([config[4], config[5]])) + 6 != config.len())
    {
        return Err(bad());
    }
    let mut bytes = firmware[at..at + len - 4].to_vec();
    bytes.extend_from_slice(&firmware[8..12]);
    bytes.extend_from_slice(config);
    Ok(bytes)
}

pub fn fragments(bytes: &[u8]) -> Vec<Vec<u8>> {
    // Realtek wraps 0x7f to 1. An exact multiple needs a final empty fragment.
    let mut sequence = 0u8;
    (0..=bytes.len() / FRAGMENT_BYTES)
        .map(|index| {
            let mut tag = sequence;
            sequence = if sequence == 0x7f { 1 } else { sequence + 1 };
            let at = index * FRAGMENT_BYTES;
            let end = (at + FRAGMENT_BYTES).min(bytes.len());
            if index == bytes.len() / FRAGMENT_BYTES {
                tag |= 0x80;
            }
            let mut fragment = vec![tag];
            fragment.extend_from_slice(&bytes[at..end]);
            fragment
        })
        .collect()
}

fn firmware(name: &str, optional: bool) -> Result<Vec<u8>, String> {
    let path = std::path::Path::new("/usr/lib/firmware/rtl_bt").join(name);
    match std::fs::read(&path) {
        Ok(bytes) if bytes.len() <= MAX_FIRMWARE_BYTES => return Ok(bytes),
        Ok(_) => return Err("firmware file exceeds size limit".into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
        Err(e) => return Err(format!("read distro firmware: {e}")),
    }
    let compressed = path.with_extension("bin.zst");
    let data = match std::fs::read(compressed) {
        Ok(bytes) => bytes,
        Err(e) if optional && e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("read distro firmware: {e}")),
    };
    if data.len() > MAX_FIRMWARE_BYTES {
        return Err("compressed firmware exceeds size limit".into());
    }
    // Fixed-capacity output bounds decompression even for a corrupt header.
    unsafe extern "C" {
        fn ZSTD_decompress(dst: *mut u8, capacity: usize, src: *const u8, size: usize) -> usize;
        fn ZSTD_isError(code: usize) -> u32;
    }
    let mut out = vec![0u8; MAX_FIRMWARE_BYTES];
    let size = unsafe { ZSTD_decompress(out.as_mut_ptr(), out.len(), data.as_ptr(), data.len()) };
    if unsafe { ZSTD_isError(size) } != 0 || size > out.len() {
        return Err("distro firmware decompression failed".into());
    }
    out.truncate(size);
    Ok(out)
}

pub fn initialize(device: &Device) -> Result<(), String> {
    if device.adapter != Adapter::Ub500 {
        return Ok(());
    }
    let version = command(device, hci::READ_VERSION, &[])?;
    if version.len() != 9
        || version[1] != 0x0a
        || u16::from_le_bytes([version[5], version[6]]) != 93
    {
        return Err("UB 500 controller is not the supported RTL8761BU".into());
    }
    let subversion = u16::from_le_bytes([version[7], version[8]]);
    if subversion != 0x8761 {
        println!("Realtek firmware already active ({subversion:#06x})");
        return Ok(());
    }
    if version[2..4] != [0x0b, 0] {
        return Err("unsupported Realtek ROM revision".into());
    }
    let rom = command(device, 0xfc6d, &[])?;
    if rom.len() != 2 {
        return Err("invalid Realtek ROM reply".into());
    }
    let bytes = patch(
        &firmware("rtl8761bu_fw.bin", false)?,
        rom[1],
        &firmware("rtl8761bu_config.bin", true)?,
    )?;
    println!("Loading distro RTL8761BU firmware: {} bytes", bytes.len());
    for fragment in fragments(&bytes) {
        let reply = command(device, 0xfc20, &fragment)?;
        if reply.len() != 2 || reply[1] != fragment[0] {
            return Err("Realtek firmware fragment acknowledgment mismatch".into());
        }
    }
    let version = command(device, hci::READ_VERSION, &[])?;
    if version.len() != 9 || version[7..9] == [0x61, 0x87] {
        return Err("Realtek firmware did not activate".into());
    }
    println!("Realtek firmware initialized by magic-usb");
    Ok(())
}

#[cfg(test)]
#[path = "../tests/realtek/mod.rs"]
mod tests;
