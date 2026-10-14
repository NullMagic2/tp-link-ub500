//! Module: csr8510::hci::tests
//! Purpose: Regression tests and fixtures for hci.
//! Created: 2026-10-11
//! Architecture: Test-only child module; private production helpers remain accessible.

use super::*;

const RESET_COMPLETE: &[u8] = &[0x0e, 0x04, 0x01, 0x03, 0x0c, 0x00];
const ACL_PACKET: &[u8] = &[0x01, 0x20, 0x03, 0x00, 0xaa, 0xbb, 0xcc];
const SCO_PACKET: &[u8] = &[0x02, 0x00, 0x02, 0xdd, 0xee];
const COMMAND_PACKET: &[u8] = &[0x03, 0x0c, 0x00];
const SERVICE_CHANGED: [u8; 17] = [0x30, 0x20, 13, 0, 9, 0, 4, 0, 9, 7, 1, 0, 0, 2, 0, 5, 0x2a];

#[test]
fn keyboard_fix_preserves_continuations_and_incomplete_pdus() {
    let unchanged = |mut packet: Vec<u8>| {
        let original = packet.clone();
        assert!(!declare_service_changed_indicate(&mut packet));
        assert_eq!(packet, original);
    };
    // Media continuation deliberately resembles the entire ATT declaration.
    for flags in [0x10, 0x30] {
        let mut packet = SERVICE_CHANGED.to_vec();
        packet[1] = flags;
        unchanged(packet);
    }
    for length in 0..SERVICE_CHANGED.len() {
        unchanged(SERVICE_CHANGED[..length].to_vec());
    }
    for offset in [2, 4] {
        let mut packet = SERVICE_CHANGED.to_vec();
        packet[offset] += 1;
        unchanged(packet);
    }
    let mut packet = SERVICE_CHANGED.to_vec();
    packet.push(0);
    packet[2] += 1;
    packet[4] += 1;
    unchanged(packet);

    for flags in [0x00, 0x20] {
        let mut packet = SERVICE_CHANGED;
        packet[1] = flags;
        let mut expected = packet;
        expected[12] = 0x20;
        assert!(declare_service_changed_indicate(&mut packet));
        assert_eq!(packet, expected);
        assert!(!declare_service_changed_indicate(&mut packet));
    }
}

#[test]
fn framing_survives_every_split_and_coalescing() {
    for (kind, packet) in [
        (Kind::Event, RESET_COMPLETE),
        (Kind::Acl, ACL_PACKET),
        (Kind::Sco, SCO_PACKET),
        (Kind::Command, COMMAND_PACKET),
    ] {
        for split in 0..=packet.len() {
            let mut framer = Framer::new(kind);
            let mut packets = framer.push(&packet[..split]);
            packets.extend(framer.push(&packet[split..]));
            assert_eq!(packets, vec![packet]);
        }
        assert_eq!(Framer::new(kind).push(&packet.repeat(2)).len(), 2);
    }
    assert_eq!(completed_opcode(RESET_COMPLETE), Some(0x0c03));
}

#[test]
fn wire_lengths_reject_truncation_padding_and_command_overflow() {
    for (kind, packet) in [
        (EVENT, RESET_COMPLETE),
        (ACL, ACL_PACKET),
        (SCO, SCO_PACKET),
        (COMMAND, COMMAND_PACKET),
    ] {
        assert!(valid_packet(kind, packet));
        for cut in 0..packet.len() {
            assert!(!valid_packet(kind, &packet[..cut]));
        }
        let mut padded = packet.to_vec();
        padded.push(0);
        assert!(!valid_packet(kind, &padded));
    }
    assert!(!valid_packet(0xff, COMMAND_PACKET));
    for length in [0, 255] {
        let packet = command(RESET, &vec![0; length]).unwrap();
        assert!(valid_packet(COMMAND, &packet));
    }
    assert!(command(RESET, &[0; 256]).is_err());
    // ACL length is u16, not its low byte; don't impose an invented MTU.
    let mut acl = vec![0; 4 + usize::from(u16::MAX)];
    acl[2..4].copy_from_slice(&u16::MAX.to_le_bytes());
    assert!(valid_packet(ACL, &acl));
}

#[test]
fn probe_reports_failure_acceptance_and_uninterpreted_vendor_data() {
    assert_eq!(
        probe_reply(RESET_COMPLETE, RESET),
        Some(Ok("Command Complete: success"))
    );
    for event in [&[0x0e, 4, 1, 3, 0x0c, 1][..], &[0x0e, 3, 1, 3, 0x0c]] {
        assert!(probe_reply(event, RESET).unwrap().is_err());
    }
    // Exact short replies observed on this controller, without invented bytes.
    for (opcode, event) in [
        (READ_VOICE_SETTING, &[0x0e, 5, 1, 0x25, 0x0c, 0, 0][..]),
        (READ_PAGE_SCAN_TYPE, &[0x0e, 4, 1, 0x46, 0x0c, 0]),
    ] {
        assert!(probe_reply(event, opcode).unwrap().is_err());
    }
    let status = [COMMAND_STATUS, 4, 0, 0, 5, 4];
    assert_eq!(
        probe_reply(&status, 0x0405),
        Some(Ok(
            "Command Status: accepted; operation completion not verified"
        ))
    );
    let mut failed = status;
    failed[2] = 0x0c;
    assert!(probe_reply(&failed, 0x0405).unwrap().is_err());
    assert_eq!(probe_reply(&status, RESET), None);
    assert_eq!(probe_reply(&status[..5], 0x0405), None);
    let vendor = [COMMAND_COMPLETE, 4, 0, 0, 0xfc, 0xff];
    assert_eq!(
        probe_reply(&vendor, 0xfc00),
        Some(Ok(
            "raw Command Complete; return parameters not interpreted"
        ))
    );
    let credits = [COMMAND_COMPLETE, 3, 0, 0, 0];
    assert_eq!(completed_opcode(&credits), Some(0));
    let mut reset = RESET_COMPLETE.to_vec();
    reset[2] = 0;
    assert_eq!(
        probe_reply(&reset, RESET),
        Some(Ok("Command Complete: success"))
    );
}

#[test]
fn malformed_events_cannot_identify_firmware_or_trigger_link_actions() {
    let mut version = vec![COMMAND_COMPLETE, 11, 1, 0x01, 0x10];
    version.extend_from_slice(&OBSERVED_VERSION);
    let mut affected = false;
    filter(&mut version, OBSERVED_REVISION, &mut affected);
    assert!(!affected);
    assert_eq!(completed_opcode(&version), None);
    assert_eq!(describe(&[CONNECT_COMPLETE, 11, 0, 0x48, 0]), None);
    let mut commands = vec![0xff; 70];
    commands[..6].copy_from_slice(&[COMMAND_COMPLETE, 67, 1, 0x02, 0x10, 0]);
    let original = commands.clone();
    filter(&mut commands, OBSERVED_REVISION, &mut true);
    assert_eq!(commands, original);
}

#[test]
fn command_bits_are_withdrawn_only_for_the_captured_fingerprint() {
    let mut version = vec![COMMAND_COMPLETE, 12, 1, 0x01, 0x10];
    version.extend_from_slice(&OBSERVED_VERSION);
    let mut commands = vec![0xff; 70];
    commands[..6].copy_from_slice(&[COMMAND_COMPLETE, 68, 1, 0x02, 0x10, 0]);
    let original = commands.clone();
    let mut affected = false;
    filter(&mut commands, OBSERVED_REVISION, &mut affected);
    assert_eq!(commands, original);
    filter(&mut version, 0x1234, &mut affected);
    assert!(!affected);
    filter(&mut version, OBSERVED_REVISION, &mut affected);
    assert!(affected);
    filter(&mut commands, OBSERVED_REVISION, &mut affected);
    assert_eq!(commands[15], 0xfb);
    assert_eq!(commands[19], 0xfe);
}

#[test]
fn unsupported_short_replies_are_reported_once_without_changing_bytes() {
    let voice = [COMMAND_COMPLETE, 5, 1, 0x25, 0x0c, 0, 0];
    let page = [COMMAND_COMPLETE, 4, 1, 0x46, 0x0c, 0];
    let mut reported = 0;
    for event in [&voice[..], &page[..]] {
        let original = event.to_vec();
        assert!(
            compatibility_notice(event, &mut reported)
                .unwrap()
                .contains("forwarded unchanged")
        );
        assert_eq!(event, original);
        assert_eq!(compatibility_notice(event, &mut reported), None);
    }
    // Wire truncation, failure status, missing status, complete/oversized return,
    // and unrelated commands must not be misreported as the observed short success.
    for event in [
        &voice[..6],
        &[COMMAND_COMPLETE, 5, 1, 0x25, 0x0c, 1, 0],
        &[COMMAND_COMPLETE, 3, 1, 0x25, 0x0c],
        &[COMMAND_COMPLETE, 6, 1, 0x25, 0x0c, 0, 0, 0],
        &[COMMAND_COMPLETE, 7, 1, 0x25, 0x0c, 0, 0, 0, 0],
        RESET_COMPLETE,
    ] {
        let mut reported = 0;
        assert_eq!(compatibility_notice(event, &mut reported), None);
        assert_eq!(reported, 0);
    }
}
