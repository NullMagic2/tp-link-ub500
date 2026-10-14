//! Module: magic_usb::hci
//! Purpose: Frame HCI packets and apply the few fixes this firmware needs.
//! Created: 2026-10-09
//! Architecture: USB supplies fragments; the bridge owns sequencing.

pub const COMMAND: u8 = 0x01;
pub const ACL: u8 = 0x02;
pub const SCO: u8 = 0x03;
pub const EVENT: u8 = 0x04;

pub const READ_VERSION: u16 = 0x1001;
pub const READ_COMMANDS: u16 = 0x1002;
const RESET: u16 = 0x0c03;
const READ_VOICE_SETTING: u16 = 0x0c25;
const READ_PAGE_SCAN_TYPE: u16 = 0x0c46;

pub const COMMAND_COMPLETE: u8 = 0x0e;
const COMMAND_STATUS: u8 = 0x0f;
pub const CONNECT_COMPLETE: u8 = 0x03;
pub const DISCONNECT_COMPLETE: u8 = 0x05;
pub const ROLE_CHANGE: u8 = 0x12;
pub const MODE_CHANGE: u8 = 0x14;
pub const SYNC_CONNECT_COMPLETE: u8 = 0x2c;

const OBSERVED_REVISION: u16 = 0x2520;
// Read Local Version return parameters captured on this adapter, including status.
const OBSERVED_VERSION: [u8; 9] = [0x00, 0x09, 0x20, 0x31, 0x09, 0x0a, 0x00, 0xbb, 0x22];

/// Builds an HCI command packet: opcode, parameter length, parameters.
pub fn command(opcode: u16, parameters: &[u8]) -> Result<Vec<u8>, String> {
    let length =
        u8::try_from(parameters.len()).map_err(|_| "HCI parameters longer than 255 bytes")?;
    let mut packet = opcode.to_le_bytes().to_vec();
    packet.push(length);
    packet.extend_from_slice(parameters);
    Ok(packet)
}

#[derive(Clone, Copy)]
pub enum Kind {
    Command,
    Event,
    Acl,
    Sco,
}

fn packet_size(kind: Kind, packet: &[u8]) -> Option<usize> {
    Some(match (kind, packet) {
        (Kind::Command | Kind::Sco, [_, _, length, ..]) => 3 + usize::from(*length),
        (Kind::Event, [_, length, ..]) => 2 + usize::from(*length),
        (Kind::Acl, [_, _, lo, hi, ..]) => 4 + usize::from(u16::from_le_bytes([*lo, *hi])),
        _ => return None,
    })
}

/// Checks one complete packet without its H4 type byte. Payloads stay opaque.
pub fn valid_packet(kind: u8, packet: &[u8]) -> bool {
    let kind = match kind {
        COMMAND => Kind::Command,
        ACL => Kind::Acl,
        SCO => Kind::Sco,
        EVENT => Kind::Event,
        _ => return false,
    };
    packet_size(kind, packet) == Some(packet.len())
}

/// Reassembles complete packets from USB transfers, which do not respect packet
/// boundaries. Headers: event 2 bytes (code, length), ACL 4 (handle, u16 length),
/// SCO 3 (handle, length).
pub struct Framer {
    pending: Vec<u8>,
    kind: Kind,
}

impl Framer {
    pub fn new(kind: Kind) -> Self {
        Self {
            pending: Vec::new(),
            kind,
        }
    }

    pub fn reset(&mut self) {
        self.pending.clear();
    }

    pub fn push(&mut self, bytes: &[u8]) -> Vec<Vec<u8>> {
        self.pending.extend_from_slice(bytes);
        let mut packets = Vec::new();
        loop {
            let Some(size) = packet_size(self.kind, &self.pending) else {
                break;
            };
            if self.pending.len() < size {
                break;
            }
            packets.push(self.pending.drain(..size).collect());
        }
        packets
    }
}

/// Opcode of a Command Complete event, if it is one.
pub fn completed_opcode(event: &[u8]) -> Option<u16> {
    if !valid_packet(EVENT, event) {
        return None;
    }
    match event {
        [COMMAND_COMPLETE, _, _, lo, hi, ..] => Some(u16::from_le_bytes([*lo, *hi])),
        _ => None,
    }
}

/// Interprets a matching probe reply, not an asynchronous operation's outcome.
/// Zero command credits and status-less opcode 0000 notifications are legal.
/// Unknown return parameters, including vendor replies, remain explicitly raw.
pub fn probe_reply(event: &[u8], opcode: u16) -> Option<Result<&'static str, String>> {
    if !valid_packet(EVENT, event) {
        return None;
    }
    let (status, expected) = match event {
        [COMMAND_COMPLETE, ..] if completed_opcode(event) == Some(opcode) => {
            let expected = match opcode {
                RESET => 1,
                READ_VERSION => 9,
                READ_COMMANDS => 65,
                READ_VOICE_SETTING => 3,
                READ_PAGE_SCAN_TYPE => 2,
                _ => {
                    return Some(Ok(
                        "raw Command Complete; return parameters not interpreted",
                    ));
                }
            };
            let Some(status) = event.get(5) else {
                return Some(Err("Command Complete is missing return status".into()));
            };
            (*status, Some(expected))
        }
        [COMMAND_STATUS, 4, status, _, lo, hi] if u16::from_le_bytes([*lo, *hi]) == opcode => {
            (*status, None)
        }
        _ => return None,
    };
    Some(if status != 0 {
        Err(format!("HCI opcode {opcode:#06x}: status {status:#04x}"))
    } else if let Some(expected) = expected {
        let actual = event.len() - 5;
        if actual != expected {
            Err(format!(
                "HCI opcode {opcode:#06x}: expected {expected} return bytes, received {actual}"
            ))
        } else {
            Ok("Command Complete: success")
        }
    } else {
        Ok("Command Status: accepted; operation completion not verified")
    })
}

/// Reports an observed unsupported success reply once per opcode; never edits it.
pub fn compatibility_notice(event: &[u8], reported: &mut u8) -> Option<String> {
    let opcode = completed_opcode(event)?;
    let (bit, expected) = match opcode {
        READ_VOICE_SETTING => (1, 3),
        READ_PAGE_SCAN_TYPE => (2, 2),
        _ => return None,
    };
    if event.get(5) != Some(&0) || event.len() - 5 >= expected || *reported & bit != 0 {
        return None;
    }
    *reported |= bit;
    Some(format!(
        "Unsupported controller reply {opcode:#06x}: received {} of {expected} return bytes; forwarded unchanged. Kernels requiring this read may fail initialization; no measured repair is available.",
        event.len() - 5
    ))
}

/// Withdraws the two commands this firmware advertises but answers short
/// (Read Voice Setting 0x0c25 and Read Page Scan Type 0x0c46), so kernels honoring
/// the bitmap skip them. Only for the exact captured USB/firmware fingerprint.
pub fn filter(event: &mut [u8], revision: u16, affected: &mut bool) {
    match completed_opcode(event) {
        Some(READ_VERSION) => {
            *affected = revision == OBSERVED_REVISION && event[5..] == OBSERVED_VERSION
        }
        Some(READ_COMMANDS) if *affected && event.len() == 70 && event[5] == 0 => {
            // Bitmap starts at byte 6: octet 9 bit 2 and octet 13 bit 0.
            event[6 + 9] &= !0x04;
            event[6 + 13] &= !0x01;
        }
        _ => {}
    }
}

/// Some cheap BLE keyboards declare Service Changed (0x2a05) without the indicate
/// property, which BlueZ treats as fatal and never starts its keyboard plugin.
/// Adds the property in the ATT Read By Type Response carrying that declaration.
/// Returns true when the packet was changed.
pub fn declare_service_changed_indicate(acl: &mut [u8]) -> bool {
    // ACL header 4, L2CAP header 4 (length, CID 0x0004 = ATT), then the ATT PDU:
    // opcode 0x09, pair length 7, [handle, properties, value handle, uuid16]...
    // Continuation bytes are payload, not another L2CAP header. Only edit a
    // complete, unfragmented ATT response with whole declaration entries.
    if !valid_packet(ACL, acl)
        || acl.len() < 17
        || !matches!((acl[1] >> 4) & 3, 0 | 2)
        || usize::from(u16::from_le_bytes([acl[4], acl[5]])) + 8 != acl.len()
        || acl[6..8] != [0x04, 0x00]
        || acl[8..10] != [0x09, 7]
        || (acl.len() - 10) % 7 != 0
    {
        return false;
    }
    let mut changed = false;
    for entry in acl[10..].chunks_exact_mut(7) {
        if entry[5..7] == [0x05, 0x2a] && entry[2] & 0x20 == 0 {
            entry[2] |= 0x20;
            changed = true;
        }
    }
    changed
}

/// One journal line for the link events that matter when a stream dies.
pub fn describe(event: &[u8]) -> Option<String> {
    if !valid_packet(EVENT, event) {
        return None;
    }
    Some(match event {
        [CONNECT_COMPLETE, 11, status, lo, hi, ..] => {
            format!(
                "Connect handle={:#x} status={status:#04x}",
                u16::from_le_bytes([*lo, *hi])
            )
        }
        [DISCONNECT_COMPLETE, 4, _, lo, hi, reason] => {
            format!(
                "Disconnect handle={:#x} reason={reason:#04x}",
                u16::from_le_bytes([*lo, *hi])
            )
        }
        [ROLE_CHANGE, 8, status, .., role] => {
            format!(
                "Role change status={status:#04x} role={}",
                if *role == 0 { "central" } else { "peripheral" }
            )
        }
        [MODE_CHANGE, 6, status, lo, hi, mode, ilo, ihi] => format!(
            "Mode change handle={:#x} status={status:#04x} mode={mode} interval={} slots",
            u16::from_le_bytes([*lo, *hi]),
            u16::from_le_bytes([*ilo, *ihi])
        ),
        _ => return None,
    })
}

#[cfg(test)]
#[path = "../tests/hci/mod.rs"]
mod tests;
