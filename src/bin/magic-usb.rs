//! Module: magic_usb.binary
//! Purpose: Run the shared USB relay.
//! Created: 2026-10-13
//! Architecture: CLI delegates transport to the shared library.
fn main() {
    if let Err(error) = magic_usb::cli::run() {
        eprintln!("magic-usb: {error}");
        std::process::exit(1);
    }
}
