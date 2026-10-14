//! Module: csr8510::main::tests
//! Purpose: Keep prerequisite diagnostics specific to the observed error.
//! Created: 2026-10-11
//! Architecture: Tests format errors without opening USB or creating VHCI devices.

use super::*;

#[test]
fn access_errors_only_suggest_relevant_actions() {
    use std::io::{Error, ErrorKind};
    let missing = access_error(
        "/dev/vhci",
        Error::from(ErrorKind::NotFound),
        "load hci_vhci",
    );
    assert!(missing.contains("load hci_vhci") && !missing.contains("as root"));
    let denied = access_error(
        "/dev/vhci",
        Error::from(ErrorKind::PermissionDenied),
        "load hci_vhci",
    );
    assert!(denied.contains("as root") && !denied.contains("load hci_vhci"));
    let other = access_error(
        "/dev/vhci",
        Error::new(ErrorKind::Other, "original error"),
        "load hci_vhci",
    );
    assert_eq!(other, "/dev/vhci: original error");
}
