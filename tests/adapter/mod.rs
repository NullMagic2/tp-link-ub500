//! Module: magic_usb.adapter.tests
//! Purpose: Reject malformed endpoint descriptors and preserve adapter identity.
//! Created: 2026-10-13
//! Architecture: Byte fixtures only, no device access.
use super::*;
pub fn config(ub500: bool) -> Vec<u8> {
    let ep = if ub500 {
        [0x81, 0x82, 2, 0x83, 3]
    } else {
        [0x83, 0x81, 1, 0x82, 2]
    };
    let mut data = vec![9, 2, 0, 0, 2, 1, 0, 0xe0, 50];
    data.extend([9, 4, 0, 0, 3, 0xe0, 1, 1, 0]);
    for (address, kind, packet, interval) in
        [(ep[0], 3, 16, 1), (ep[1], 2, 64, 0), (ep[2], 2, 64, 0)]
    {
        data.extend([7, 5, address, kind, packet, 0, interval]);
    }
    data.extend([9, 4, 1, 1, 2, 0xe0, 1, 1, 0]);
    for address in [ep[3], ep[4]] {
        data.extend([7, 5, address, 1, 9, 0, 1]);
    }
    let len = data.len() as u16;
    data[2..4].copy_from_slice(&len.to_le_bytes());
    data
}
#[test]
fn endpoints_come_from_the_selected_adapter() {
    assert_eq!(endpoints(&config(false)).unwrap(), Endpoints::default());
    assert_eq!(
        endpoints(&config(true)).unwrap(),
        Endpoints {
            event: 0x81,
            acl_in: 0x82,
            acl_out: 2,
            voice_in: 0x83,
            voice_out: 3
        }
    );
    for cut in 0..config(true).len() {
        assert!(endpoints(&config(true)[..cut]).is_err());
    }
    for at in [9, 18, 25, 32, 39, 48, 55] {
        let mut bad = config(true);
        bad[at] = 0;
        assert!(endpoints(&bad).is_err());
    }
}
#[test]
fn wrong_devices_and_ambiguous_endpoints_are_rejected() {
    assert_eq!(Adapter::identify(0x2357, 0x0604).unwrap(), Adapter::Ub500);
    assert_eq!(Adapter::identify(0x0a12, 1).unwrap(), Adapter::Csr8510);
    assert!(Adapter::identify(0x05ac, 0x12a8).is_err());
    let mut bad = config(true);
    bad[34] = 0x82;
    assert!(endpoints(&bad).is_err());
    let mut bad = config(true);
    bad[14] = 0xff;
    assert!(endpoints(&bad).is_err());
}

#[test]
fn cached_configuration_rejects_truncation_and_changed_identity() {
    let mut bytes = vec![0; 18];
    bytes[0] = 18;
    bytes[1] = 1;
    bytes[17] = 1;
    bytes[8..10].copy_from_slice(&0x0a12u16.to_le_bytes());
    bytes[10..12].copy_from_slice(&1u16.to_le_bytes());
    bytes[12..14].copy_from_slice(&0x2520u16.to_le_bytes());
    bytes.extend(config(false));
    assert!(super::configuration(&bytes, 1, 0x0a12, 1, 0x2520).is_ok());
    for end in 0..bytes.len() {
        assert!(super::configuration(&bytes[..end], 1, 0x0a12, 1, 0x2520).is_err());
    }
    assert!(super::configuration(&bytes, 1, 0x2357, 0x0604, 0x2520).is_err());
    assert!(super::configuration(&bytes, 0, 0x0a12, 1, 0x2520).is_err());
    assert!(super::configuration(&bytes, 2, 0x0a12, 1, 0x2520).is_err());
}
