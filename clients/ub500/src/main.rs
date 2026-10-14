//! Module: ub500.client
//! Purpose: Restrict the standalone driver to supported TP-Link adapters.
//! Architecture: Delegate transport to the shared magic-usb library.
fn main() {
    if let Err(error) = magic_usb::cli::run_for(Some(magic_usb::adapter::Adapter::Ub500)) {
        eprintln!("tp-link-ub500: {error}");
        std::process::exit(1);
    }
}
