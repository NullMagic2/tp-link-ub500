//! Module: magic_usb.example.check_firmware
//! Purpose: Validate separately extracted distro firmware without loading USB.
//! Created: 2026-10-13
//! Architecture: Calls the shared parser; does not bundle firmware.
fn main() {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 4 {
        eprintln!("usage: check_firmware ROM-VERSION FIRMWARE.bin CONFIG.bin");
        std::process::exit(2);
    }
    let result = (|| -> Result<usize, String> {
        let rom = args[1].parse::<u8>().map_err(|e| e.to_string())?;
        let fw = std::fs::read(&args[2]).map_err(|e| e.to_string())?;
        let config = std::fs::read(&args[3]).map_err(|e| e.to_string())?;
        magic_usb::realtek::patch(&fw, rom, &config).map(|bytes| bytes.len())
    })();
    match result {
        Ok(size) => println!("Validated firmware payload: {size} bytes"),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}
