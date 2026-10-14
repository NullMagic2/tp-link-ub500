//! Module: csr8510.client
//! Purpose: Share magic-usb transport while keeping the CSR driver entry point.
//! Created: 2026-10-13
//! Architecture: Adapter-specific CLI delegates to the common Rust library.
fn main() {
    if let Err(error) = magic_usb::cli::run_for(Some(magic_usb::adapter::Adapter::Csr8510)) {
        eprintln!("csr8510: {error}");
        std::process::exit(1);
    }
}
