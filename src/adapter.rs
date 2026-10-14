//! Module: magic_usb.adapter
//! Purpose: Identify supported adapters and validate their USB endpoint layout.
//! Created: 2026-10-13
//! Architecture: Pure descriptor parsing; never claims an interface.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Adapter {
    Csr8510,
    Ub500,
}

impl Adapter {
    pub fn identify(vendor: u16, product: u16) -> Result<Self, String> {
        match (vendor, product) {
            (0x0a12, 1) => Ok(Self::Csr8510),
            (0x2357, 0x0604) => Ok(Self::Ub500),
            _ => Err("selected USB device is neither CSR8510 nor TP-Link UB 500".into()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Endpoints {
    pub event: u8,
    pub acl_in: u8,
    pub acl_out: u8,
    pub voice_in: u8,
    pub voice_out: u8,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            event: 0x83,
            acl_in: 0x81,
            acl_out: 1,
            voice_in: 0x82,
            voice_out: 2,
        }
    }
}

/// Only standard Bluetooth HCI interface 0, alternate 0, is a transport candidate.
pub fn endpoints(config: &[u8]) -> Result<Endpoints, String> {
    let bad = || "invalid Bluetooth USB configuration descriptors".to_string();
    if config.len() < 9
        || config[0] != 9
        || config[1] != 2
        || usize::from(u16::from_le_bytes([config[2], config[3]])) != config.len()
    {
        return Err(bad());
    }
    let mut offset = 9;
    let mut selected = false;
    let mut voice = false;
    let mut out = Endpoints {
        event: 0,
        acl_in: 0,
        acl_out: 0,
        voice_in: 0,
        voice_out: 0,
    };
    let mut seen_hci = false;
    while offset < config.len() {
        let rest = &config[offset..];
        if rest.len() < 2 || rest[0] < 2 || rest[0] as usize > rest.len() {
            return Err(bad());
        }
        let d = &rest[..rest[0] as usize];
        if d[1] == 4 {
            if d.len() < 9 {
                return Err(bad());
            }
            selected = d[2] == 0 && d[3] == 0;
            voice = d[2] == 1 && d[3] == 1;
            if selected {
                if seen_hci || d[4] != 3 || d[5..8] != [0xe0, 1, 1] {
                    return Err(bad());
                }
                seen_hci = true;
            }
        } else if d[1] == 5 && (selected || voice) {
            if d.len() < 7
                || d[2] & 0x0f == 0
                || d[2] & 0x70 != 0
                || u16::from_le_bytes([d[4], d[5]]) == 0
            {
                return Err(bad());
            }
            let slot = match (selected, d[3] & 3, d[2] & 0x80 != 0) {
                (true, 3, true) if d[6] != 0 => &mut out.event,
                (true, 2, true) => &mut out.acl_in,
                (true, 2, false) => &mut out.acl_out,
                (false, 1, true) => &mut out.voice_in,
                (false, 1, false) => &mut out.voice_out,
                _ => return Err(bad()),
            };
            if *slot != 0 {
                return Err(bad());
            }
            *slot = d[2];
        }
        offset += d.len();
    }
    if [
        out.event,
        out.acl_in,
        out.acl_out,
        out.voice_in,
        out.voice_out,
    ]
    .contains(&0)
    {
        return Err(bad());
    }
    Ok(out)
}

#[cfg(test)]
#[path = "../tests/adapter/mod.rs"]
pub(crate) mod tests;

/// Select the active configuration from Linux's cached descriptor stream.
/// Identity must match the opened handle, including revision, before claiming it.
pub fn configuration(
    bytes: &[u8],
    active: u8,
    vendor: u16,
    product: u16,
    revision: u16,
) -> Result<&[u8], String> {
    let bad = || "invalid or changed cached USB descriptors".to_owned();
    if bytes.len() < 18 || bytes[0..2] != [18, 1] || active == 0 {
        return Err(bad());
    }
    for (offset, expected) in [(8, vendor), (10, product), (12, revision)] {
        if u16::from_le_bytes([bytes[offset], bytes[offset + 1]]) != expected {
            return Err(bad());
        }
    }
    let mut offset = 18;
    let mut selected = None;
    for _ in 0..bytes[17] {
        let rest = &bytes[offset..];
        if rest.len() < 9 || rest[0..2] != [9, 2] {
            return Err(bad());
        }
        let length = usize::from(u16::from_le_bytes([rest[2], rest[3]]));
        if length < 9 || length > rest.len() {
            return Err(bad());
        }
        if rest[5] == active {
            if selected.is_some() {
                return Err(bad());
            }
            selected = Some(&rest[..length]);
        }
        offset += length;
    }
    if offset != bytes.len() {
        return Err(bad());
    }
    selected.ok_or_else(bad)
}
