//! Module: magic_usb.realtek.tests
//! Purpose: Check firmware bounds, chip selection and fragment sequencing.
//! Created: 2026-10-13
//! Architecture: Synthetic epatch data, no copyrighted firmware fixtures.
use super::*;
fn fixture() -> Vec<u8> {
    let mut fw = b"Realtech".to_vec();
    fw.extend([1, 2, 3, 4, 1, 0, 2, 0, 8, 0, 22, 0, 0, 0]);
    fw.extend([11, 12, 13, 14, 0, 0, 0, 0, 14, 1, 0, 0x51, 4, 0xfd, 0x77]);
    fw
}
#[test]
fn patch_selects_rom_and_replaces_version() {
    let fw = fixture();
    assert_eq!(
        patch(&fw, 1, &[0x55, 0xab, 0x23, 0x87, 0, 0]).unwrap(),
        [11, 12, 13, 14, 1, 2, 3, 4, 0x55, 0xab, 0x23, 0x87, 0, 0]
    );
    assert!(patch(&fw, 0, &[]).is_err());
    for cut in 0..fw.len() {
        assert!(patch(&fw[..cut], 1, &[]).is_err());
    }
    for at in [0, 12, 16, 18, 30, 36] {
        let mut bad = fw.clone();
        bad[at] = 0xff;
        assert!(patch(&bad, 1, &[]).is_err());
    }
    assert!(patch(&fw, 1, &[0x55, 0xab, 0x23, 0x87, 1, 0]).is_err());
}
#[test]
fn fragments_keep_data_and_mark_end_including_exact_multiples() {
    for size in [0, 1, 252, 253, 252 * 128, 252 * 130] {
        let data = vec![42; size];
        let parts = fragments(&data);
        assert!(parts.iter().all(|p| p.len() <= 253));
        assert!(parts[..parts.len() - 1].iter().all(|p| p[0] & 0x80 == 0));
        assert!(parts.last().unwrap()[0] & 0x80 != 0);
        assert_eq!(
            parts
                .iter()
                .flat_map(|p| p[1..].iter().copied())
                .collect::<Vec<_>>(),
            data
        );
        if size >= 252 * 128 {
            assert_eq!(parts[127][0], 0x7f);
            assert_eq!(parts[128][0] & 0x7f, 1);
        }
    }
}
