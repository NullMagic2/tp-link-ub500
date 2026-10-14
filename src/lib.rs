//! Module: magic_usb
//! Purpose: Shared Bluetooth USB transport for magic-usb and the CSR driver.
//! Created: 2026-10-13
//! Architecture: libusb transports records; Linux VHCI and BlueZ own Bluetooth.
pub mod adapter;
pub mod bridge;
pub mod cli;
pub mod hci;
pub mod hotplug;
pub mod ownership;
pub mod realtek;
pub mod recovery;
pub mod sco;
pub mod usb;
pub static STOP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
#[cfg(test)]
#[path = "../tests/support/mod.rs"]
mod test_support;
#[cfg(test)]
pub use test_support::STOP_TEST_LOCK;
/// Adds an action only when the actual error supports it.
pub fn access_error(path: &str, error: std::io::Error, missing: &str) -> String {
    let hint = match error.kind() {
        std::io::ErrorKind::NotFound => missing,
        std::io::ErrorKind::PermissionDenied => {
            "grant device access or run the selected service as root"
        }
        _ => "",
    };
    format!(
        "{path}: {error}{}",
        if hint.is_empty() {
            String::new()
        } else {
            format!("; {hint}")
        }
    )
}
