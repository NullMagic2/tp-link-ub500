//! Module: magic_usb.build
//! Purpose: Link distro USB and firmware decompression runtimes.
//! Created: 2026-10-13
//! Architecture: Dynamic system libraries, shared by both clients.
fn main() {
    println!("cargo:rustc-link-lib=dylib:+verbatim=libusb-1.0.so.0");
    println!("cargo:rustc-link-lib=dylib:+verbatim=libzstd.so.1");
}
