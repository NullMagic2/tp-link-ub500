//! Wait for enumeration on the explicitly selected port without taking ownership.
use crate::{STOP, adapter::Adapter};
use std::{
    fs,
    path::Path,
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

fn ready(path: &Path, required: Option<Adapter>) -> Option<String> {
    let read = |name| {
        fs::read_to_string(path.join(name))
            .ok()
            .map(|s| s.trim().to_owned())
    };
    let vendor = u16::from_str_radix(&read("idVendor")?, 16).ok()?;
    let product = u16::from_str_radix(&read("idProduct")?, 16).ok()?;
    let adapter = Adapter::identify(vendor, product).ok()?;
    if required.is_some_and(|r| r != adapter) || read("bConfigurationValue")? == "0" {
        return None;
    }
    let bus = read("busnum")?;
    let address = read("devnum")?;
    if !path.join("descriptors").exists() {
        return None;
    }
    Some(format!("{bus}:{address}:{vendor}:{product}"))
}

pub fn wait(port: &str, required: Option<Adapter>, seconds: Option<u64>) -> Result<bool, String> {
    if !crate::ownership::valid_port(port) {
        return Err("use a USB port name such as 1-4".into());
    }
    let path = Path::new("/sys/bus/usb/devices").join(port);
    let started = Instant::now();
    let mut previous = None;
    println!("Waiting for a supported Bluetooth adapter on USB {port}");
    while !STOP.load(Ordering::Relaxed) {
        if seconds.is_some_and(|s| started.elapsed() >= Duration::from_secs(s)) {
            return Ok(false);
        }
        let current = ready(&path, required);
        // Require two matching samples so partial enumeration or another swap
        // cannot immediately trigger a claim. Device::open verifies again.
        if current.is_some() && current == previous {
            return Ok(true);
        }
        previous = current;
        std::thread::sleep(Duration::from_millis(250));
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn absent_unsupported_and_swapped_devices() {
        let path = std::env::temp_dir().join(format!("magic-usb-hotplug-{}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        assert!(ready(&path, None).is_none());
        for (key, value) in [
            ("idVendor", "0a12"),
            ("idProduct", "0001"),
            ("bConfigurationValue", "1"),
            ("busnum", "1"),
            ("devnum", "5"),
            ("descriptors", "cached"),
        ] {
            fs::write(path.join(key), value).unwrap();
        }
        let csr = ready(&path, None).unwrap();
        assert!(ready(&path, Some(Adapter::Ub500)).is_none());
        fs::write(path.join("idVendor"), "2357").unwrap();
        fs::write(path.join("idProduct"), "0604").unwrap();
        fs::write(path.join("devnum"), "6").unwrap();
        assert_ne!(ready(&path, None).unwrap(), csr);
        fs::write(path.join("idVendor"), "1234").unwrap();
        assert!(ready(&path, None).is_none());
        fs::remove_dir_all(path).unwrap();
    }
}
