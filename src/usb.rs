//! Module: magic_usb::usb
//! Purpose: Own one supported Bluetooth adapter through libusb.
//! Created: 2026-10-09
//! Architecture: libusb owns USB syscalls; Rust owns buffers and handle lifetime.

use crate::adapter::{Adapter, Endpoints};
use std::ffi::c_void;
use std::fs::{self, File, OpenOptions};
use std::os::raw::{c_int, c_uint};
use std::os::unix::io::AsRawFd;
use std::path::Path;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};

const INTERFACE: c_int = 0;
const VOICE_INTERFACE: c_int = 1;
const LIBUSB_TIMEOUT: c_int = -7;
const LIBUSB_NO_DEVICE: c_int = -4;
const COMMAND_TIMEOUT_MS: c_uint = 1000;
/// Largest HCI event (257 bytes) rounded up to the 16-byte interrupt packet size.
pub const EVENT_BYTES: usize = 272;
pub const ACL_BYTES: usize = 1024;

#[derive(Default)]
#[repr(C)]
struct Descriptor {
    length: u8,
    kind: u8,
    usb: u16,
    class: u8,
    subclass: u8,
    protocol: u8,
    packet_size: u8,
    vendor: u16,
    product: u16,
    revision: u16,
    manufacturer: u8,
    product_string: u8,
    serial: u8,
    configurations: u8,
}

#[cfg(not(test))]
unsafe extern "C" {
    fn libusb_init(context: *mut *mut c_void) -> c_int;
    fn libusb_exit(context: *mut c_void);
    fn libusb_wrap_sys_device(context: *mut c_void, fd: isize, handle: *mut *mut c_void) -> c_int;
    fn libusb_close(handle: *mut c_void);
    fn libusb_get_device(handle: *mut c_void) -> *mut c_void;
    fn libusb_get_bus_number(device: *mut c_void) -> u8;
    fn libusb_get_device_address(device: *mut c_void) -> u8;
    fn libusb_get_device_descriptor(device: *mut c_void, descriptor: *mut Descriptor) -> c_int;
    fn libusb_reset_device(handle: *mut c_void) -> c_int;
    fn libusb_kernel_driver_active(handle: *mut c_void, interface: c_int) -> c_int;
    fn libusb_detach_kernel_driver(handle: *mut c_void, interface: c_int) -> c_int;
    fn libusb_attach_kernel_driver(handle: *mut c_void, interface: c_int) -> c_int;
    fn libusb_claim_interface(handle: *mut c_void, interface: c_int) -> c_int;
    fn libusb_release_interface(handle: *mut c_void, interface: c_int) -> c_int;
    fn libusb_clear_halt(handle: *mut c_void, endpoint: u8) -> c_int;
    fn flock(fd: c_int, operation: c_int) -> c_int;
    fn libusb_control_transfer(
        handle: *mut c_void,
        kind: u8,
        request: u8,
        value: u16,
        index: u16,
        data: *mut u8,
        length: u16,
        timeout: c_uint,
    ) -> c_int;
    fn libusb_interrupt_transfer(
        handle: *mut c_void,
        endpoint: u8,
        data: *mut u8,
        length: c_int,
        actual: *mut c_int,
        timeout: c_uint,
    ) -> c_int;
    fn libusb_bulk_transfer(
        handle: *mut c_void,
        endpoint: u8,
        data: *mut u8,
        length: c_int,
        actual: *mut c_int,
        timeout: c_uint,
    ) -> c_int;
}

#[cfg(test)]
use tests::*;

pub struct Device {
    context: *mut c_void,
    handle: *mut c_void,
    detached: bool,
    claimed: bool,
    removed: AtomicBool,
    /// Both HCI and voice interfaces belong to this transport while it runs.
    voice_claimed: AtomicBool,
    voice_detached: AtomicBool,
    /// bcdDevice from the opened device descriptor; the HCI fixes are gated on it.
    pub revision: u16,
    pub adapter: Adapter,
    pub endpoints: Endpoints,
    // Keeping usbfs open holds a runtime-PM reference, so the adapter never autosuspends.
    _file: File,
}

// libusb permits synchronous transfers from several threads on one open handle.
unsafe impl Sync for Device {}

fn attribute(path: &Path, name: &str) -> Result<String, String> {
    fs::read_to_string(path.join(name))
        .map(|value| value.trim().to_owned())
        .map_err(|error| format!("{}: {error}", path.join(name).display()))
}

/// Resolves a sysfs port name such as 1-4 to its usbfs node; identity is checked after opening.
fn identify(port: &str) -> Result<String, String> {
    if !crate::ownership::valid_port(port) {
        return Err("use a USB port name such as 1-4".into());
    }
    let path = Path::new("/sys/bus/usb/devices").join(port);
    let bus: u16 = attribute(&path, "busnum")?
        .parse()
        .map_err(|_| "invalid busnum")?;
    let device: u16 = attribute(&path, "devnum")?
        .parse()
        .map_err(|_| "invalid devnum")?;
    Ok(format!("/dev/bus/usb/{bus:03}/{device:03}"))
}

impl Device {
    /// Detaches btusb, claims the HCI interface and bus-resets the adapter.
    /// Only the CSR profile is reset; Realtek keeps its current firmware.
    pub fn open(port: &str) -> Result<Self, String> {
        Self::open_as(port, None)
    }

    pub fn open_as(port: &str, required: Option<Adapter>) -> Result<Self, String> {
        let node = identify(port)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&node)
            .map_err(|error| {
                crate::access_error(&node, error, "adapter absent; check the selected USB port")
            })?;
        #[cfg(not(test))]
        if unsafe { flock(file.as_raw_fd(), 2 | 4) } != 0 {
            return Err("selected adapter is already owned by another magic-usb client".into());
        }
        Self::from_file_as(file, port, required)
    }

    /// Configures the opened descriptor before any detach, claim or reset.
    #[cfg(test)]
    fn from_file(file: File, port: &str) -> Result<Self, String> {
        Self::from_file_as(file, port, None)
    }

    fn from_file_as(file: File, port: &str, required: Option<Adapter>) -> Result<Self, String> {
        let mut device = Self {
            context: ptr::null_mut(),
            handle: ptr::null_mut(),
            detached: false,
            claimed: false,
            removed: AtomicBool::new(false),
            voice_claimed: AtomicBool::new(false),
            voice_detached: AtomicBool::new(false),
            revision: 0,
            adapter: Adapter::Csr8510,
            endpoints: Endpoints::default(),
            _file: file,
        };
        // Output pointers and the borrowed fd stay valid until Drop.
        unsafe {
            let result = libusb_init(&mut device.context);
            device.check(result, "initialize USB")?;
            let result = libusb_wrap_sys_device(
                device.context,
                device._file.as_raw_fd() as isize,
                &mut device.handle,
            );
            device.check(result, "open USB device")?;
            device.verify(port)?;
            if required.is_some_and(|required| required != device.adapter) {
                return Err("selected adapter does not match this driver client".into());
            }
            #[cfg(not(test))]
            if std::env::var_os("MAGIC_USB_SERVICE").as_deref() == Some(std::ffi::OsStr::new("1")) {
                let state =
                    crate::ownership::Ownership::capture(Path::new("/sys/bus/usb/devices"), port)?;
                let opened = libusb_get_device(device.handle);
                if state.bus != u16::from(libusb_get_bus_number(opened))
                    || state.address != u16::from(libusb_get_device_address(opened))
                {
                    return Err("USB attachment changed before ownership transfer".into());
                }
                state.save(&crate::ownership::state_path(port)?)?;
            } else {
                #[cfg(not(test))]
                let _ =
                    crate::ownership::Ownership::capture(Path::new("/sys/bus/usb/devices"), port)?;
            }
            if device.check(
                libusb_kernel_driver_active(device.handle, INTERFACE),
                "check kernel driver",
            )? != 0
            {
                device.check(
                    libusb_detach_kernel_driver(device.handle, INTERFACE),
                    "detach kernel driver",
                )?;
                device.detached = true;
            }
            device.check(
                libusb_claim_interface(device.handle, INTERFACE),
                "claim HCI interface",
            )?;
            device.claimed = true;
            // The CSR clone needs a reset; Realtek retains its firmware across handoff.
            device.claim_voice()?;
            if device.adapter == Adapter::Csr8510 {
                device.check(libusb_reset_device(device.handle), "USB reset")?;
            }
        }
        Ok(device)
    }

    fn check(&self, result: c_int, what: &str) -> Result<c_int, String> {
        if result == LIBUSB_NO_DEVICE {
            self.removed.store(true, Ordering::Relaxed);
            Err(format!("{what}: USB adapter removed"))
        } else if result < 0 {
            Err(format!("{what}: libusb error {result}"))
        } else {
            Ok(result)
        }
    }

    fn verify(&mut self, port: &str) -> Result<(), String> {
        let mut descriptor = Descriptor::default();
        unsafe {
            let device = libusb_get_device(self.handle);
            if device.is_null() {
                return Err("opened USB handle has no device".into());
            }
            self.check(
                libusb_get_device_descriptor(device, &mut descriptor),
                "read USB identity",
            )?;
            if descriptor.length != 18
                || descriptor.kind != 1
                || Adapter::identify(descriptor.vendor, descriptor.product).is_err()
            {
                return Err("opened USB device is not a supported Bluetooth adapter".into());
            }
        }
        self.revision = descriptor.revision;
        self.adapter = Adapter::identify(descriptor.vendor, descriptor.product)?;
        let (descriptors, active) = cached_descriptors(port)?;
        let config = crate::adapter::configuration(
            &descriptors,
            active,
            descriptor.vendor,
            descriptor.product,
            descriptor.revision,
        )?;
        self.endpoints = crate::adapter::endpoints(config)?;
        println!("USB {port} {:?} bcd={:04x}", self.adapter, self.revision);
        Ok(())
    }

    pub(crate) fn handle(&self) -> *mut c_void {
        self.handle
    }

    /// Claims the isochronous voice interface; idempotent, given back on Drop.
    pub(crate) fn claim_voice(&self) -> Result<(), String> {
        if self.voice_claimed.load(Ordering::Relaxed) {
            return Ok(());
        }
        unsafe {
            if self.check(
                libusb_kernel_driver_active(self.handle, VOICE_INTERFACE),
                "check voice driver",
            )? != 0
            {
                self.check(
                    libusb_detach_kernel_driver(self.handle, VOICE_INTERFACE),
                    "detach voice driver",
                )?;
                self.voice_detached.store(true, Ordering::Relaxed);
            }
            self.check(
                libusb_claim_interface(self.handle, VOICE_INTERFACE),
                "claim voice interface",
            )?;
        }
        self.voice_claimed.store(true, Ordering::Relaxed);
        Ok(())
    }

    pub(crate) fn context(&self) -> *mut c_void {
        self.context
    }

    /// Sends one HCI command packet (opcode, length, parameters) on the control endpoint.
    /// A timeout leaves command acceptance unknown, so never replay it.
    pub fn command(&self, packet: &mut [u8]) -> Result<(), String> {
        let length = u16::try_from(packet.len()).map_err(|_| "command too long")?;
        // libusb only borrows the buffer for the duration of the synchronous call.
        let result = unsafe {
            libusb_control_transfer(
                self.handle,
                0x20,
                0,
                0,
                INTERFACE as u16,
                packet.as_mut_ptr(),
                length,
                COMMAND_TIMEOUT_MS,
            )
        };
        let sent = self.check(result, "USB command")?;
        if sent as usize != packet.len() {
            return Err(format!("short USB command: {sent}/{}", packet.len()));
        }
        Ok(())
    }

    pub fn acl_out(&self, packet: &mut [u8]) -> Result<(), String> {
        let mut actual = 0;
        self.check(
            unsafe {
                libusb_bulk_transfer(
                    self.handle,
                    self.endpoints.acl_out,
                    packet.as_mut_ptr(),
                    packet.len() as c_int,
                    &mut actual,
                    COMMAND_TIMEOUT_MS,
                )
            },
            "USB ACL OUT",
        )?;
        if actual as usize != packet.len() {
            return Err(format!("short USB ACL OUT: {actual}/{}", packet.len()));
        }
        Ok(())
    }

    /// Reads event bytes, preserving partial data on timeout.
    pub fn events(&self, buffer: &mut [u8], timeout_ms: u32) -> Result<usize, String> {
        let mut actual = 0;
        let result = unsafe {
            libusb_interrupt_transfer(
                self.handle,
                self.endpoints.event,
                buffer.as_mut_ptr(),
                buffer.len() as c_int,
                &mut actual,
                timeout_ms,
            )
        };
        self.received(result, actual, buffer.len(), "USB event IN")
    }

    /// Reads ACL bytes, preserving partial data on timeout.
    pub fn acl_in(&self, buffer: &mut [u8], timeout_ms: u32) -> Result<usize, String> {
        let mut actual = 0;
        let result = unsafe {
            libusb_bulk_transfer(
                self.handle,
                self.endpoints.acl_in,
                buffer.as_mut_ptr(),
                buffer.len() as c_int,
                &mut actual,
                timeout_ms,
            )
        };
        self.received(result, actual, buffer.len(), "USB ACL IN")
    }

    fn received(
        &self,
        result: c_int,
        actual: c_int,
        capacity: usize,
        what: &str,
    ) -> Result<usize, String> {
        if result != LIBUSB_TIMEOUT {
            self.check(result, what)?;
        }
        if actual < 0 || actual as usize > capacity {
            return Err(format!(
                "{what}: invalid transfer count {actual}/{capacity}"
            ));
        }
        Ok(actual as usize)
    }
}

impl Device {
    pub fn clear_receive_stall(&self, event: bool) -> Result<(), String> {
        let endpoint = if event {
            self.endpoints.event
        } else {
            self.endpoints.acl_in
        };
        self.check(
            unsafe { libusb_clear_halt(self.handle, endpoint) },
            "clear USB receive stall",
        )
        .map(|_| ())
    }

    /// Called after voice callbacks, workers and VHCI are gone; Drop is a fallback.
    fn close(&mut self) -> Result<(), String> {
        let mut error = None;
        unsafe {
            if !self.handle.is_null() {
                for (interface, claimed) in [
                    (VOICE_INTERFACE, self.voice_claimed.load(Ordering::Relaxed)),
                    (INTERFACE, self.claimed),
                ] {
                    if !self.removed.load(Ordering::Relaxed) && claimed {
                        let result = libusb_release_interface(self.handle, interface);
                        if result == LIBUSB_NO_DEVICE {
                            self.removed.store(true, Ordering::Relaxed);
                        } else if result < 0 {
                            error.get_or_insert(format!(
                                "release USB interface {interface}: libusb error {result}"
                            ));
                        }
                    }
                }
                for (interface, detached) in [
                    (INTERFACE, self.detached),
                    (VOICE_INTERFACE, self.voice_detached.load(Ordering::Relaxed)),
                ] {
                    #[cfg(not(test))]
                    if !Path::new("/sys/module/btusb").exists() {
                        continue;
                    }
                    if !self.removed.load(Ordering::Relaxed) && detached {
                        let active = libusb_kernel_driver_active(self.handle, interface);
                        if active == LIBUSB_NO_DEVICE {
                            self.removed.store(true, Ordering::Relaxed);
                            continue;
                        }
                        if active > 0 {
                            continue;
                        }
                        let result = libusb_attach_kernel_driver(self.handle, interface);
                        if result == LIBUSB_NO_DEVICE {
                            self.removed.store(true, Ordering::Relaxed);
                        } else if result < 0 {
                            error.get_or_insert(format!("reattach USB interface {interface}: libusb error {result}; replug the adapter"));
                        }
                    }
                }
                libusb_close(self.handle);
                self.handle = ptr::null_mut();
            }
            if !self.context.is_null() {
                libusb_exit(self.context);
                self.context = ptr::null_mut();
            }
        }
        error.map_or(Ok(()), Err)
    }

    pub fn finish(mut self, operation: Result<(), String>) -> Result<(), String> {
        match (operation, self.close()) {
            (Err(operation), Err(cleanup)) => {
                Err(format!("{operation}; cleanup failed: {cleanup}"))
            }
            (operation, cleanup) => operation.and(cleanup),
        }
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        if let Err(error) = self.close() {
            eprintln!("USB cleanup failed: {error}");
        }
    }
}

#[cfg(test)]
#[path = "../tests/usb/mod.rs"]
pub(crate) mod tests;

/// Restore recorded bindings only; an absent or inactive btusb is left alone.
pub fn restore(port: &str) -> Result<(), String> {
    let bind = Path::new("/sys/bus/usb/drivers/btusb/bind");
    crate::ownership::restore_in(
        Path::new("/sys/bus/usb/devices"),
        port,
        &crate::ownership::state_path(port)?,
        bind,
        |interface| {
            fs::write(bind, interface).map_err(|e| format!("restore previous btusb ownership: {e}"))
        },
    )
}

// sysfs exposes descriptors cached during enumeration, even when the controller
// stops answering control requests. Validate them before any ownership change.
#[cfg(not(test))]
fn cached_descriptors(port: &str) -> Result<(Vec<u8>, u8), String> {
    let path = Path::new("/sys/bus/usb/devices").join(port);
    let active = attribute(&path, "bConfigurationValue")?
        .parse::<u8>()
        .map_err(|_| "invalid active USB configuration")?;
    let bytes = fs::read(path.join("descriptors"))
        .map_err(|e| format!("read cached USB descriptors: {e}"))?;
    Ok((bytes, active))
}
