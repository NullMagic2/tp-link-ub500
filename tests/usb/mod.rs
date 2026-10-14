//! Module: csr8510::usb::tests
//! Purpose: Exercise USB identity, setup failures, removal and ownership cleanup.
//! Created: 2026-10-11
//! Architecture: Thread-local fake USB backend; no physical USB is opened.

use super::*;
use std::cell::RefCell;

struct Backend {
    calls: Vec<String>,
    fail: Option<(&'static str, c_int)>,
    vendor: u16,
    result: c_int,
    actual: c_int,
    kernel_active: bool,
}

impl Default for Backend {
    fn default() -> Self {
        Self {
            calls: Vec::new(),
            fail: None,
            vendor: 0x0a12,
            result: 0,
            actual: 0,
            kernel_active: true,
        }
    }
}

thread_local! { static USB: RefCell<Backend> = RefCell::new(Backend::default()); }

fn call(name: &str) -> c_int {
    USB.with(|usb| {
        let mut usb = usb.borrow_mut();
        usb.calls.push(name.into());
        usb.fail
            .filter(|(operation, _)| *operation == name)
            .map_or(0, |(_, code)| code)
    })
}

fn reset() {
    USB.with(|usb| *usb.borrow_mut() = Backend::default());
}
fn calls() -> Vec<String> {
    USB.with(|usb| usb.borrow().calls.clone())
}
fn open() -> Result<Device, String> {
    Device::from_file(File::open("/dev/null").unwrap(), "1-4")
}

impl Device {
    pub(crate) fn test_device() -> Self {
        Self {
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
            _file: File::open("/dev/null").unwrap(),
        }
    }
}

pub(super) unsafe fn libusb_init(context: *mut *mut c_void) -> c_int {
    let result = call("init");
    if result == 0 {
        unsafe {
            *context = ptr::dangling_mut::<u8>().cast();
        }
    }
    result
}
pub(super) unsafe fn libusb_exit(_: *mut c_void) {
    call("exit");
}
pub(super) unsafe fn libusb_wrap_sys_device(
    _: *mut c_void,
    _: isize,
    handle: *mut *mut c_void,
) -> c_int {
    let result = call("wrap");
    if result == 0 {
        unsafe {
            *handle = ptr::dangling_mut::<u16>().cast();
        }
    }
    result
}
pub(super) unsafe fn libusb_close(_: *mut c_void) {
    call("close");
}
pub(super) unsafe fn libusb_get_device(handle: *mut c_void) -> *mut c_void {
    call("device");
    handle
}
pub(super) unsafe fn libusb_get_device_descriptor(
    _: *mut c_void,
    descriptor: *mut Descriptor,
) -> c_int {
    let result = call("descriptor");
    USB.with(|usb| unsafe {
        *descriptor = Descriptor {
            length: 18,
            kind: 1,
            vendor: usb.borrow().vendor,
            product: 1,
            revision: 0x2520,
            ..Descriptor::default()
        };
    });
    result
}
pub(super) unsafe fn libusb_reset_device(_: *mut c_void) -> c_int {
    call("reset")
}
pub(super) unsafe fn libusb_kernel_driver_active(_: *mut c_void, interface: c_int) -> c_int {
    let result = call(&format!("active{interface}"));
    if result == 0
        && USB.with(|usb| usb.borrow().kernel_active)
        && !calls().iter().any(|c| c == &format!("release{interface}"))
    {
        1
    } else {
        result
    }
}
pub(super) unsafe fn libusb_detach_kernel_driver(_: *mut c_void, interface: c_int) -> c_int {
    call(&format!("detach{interface}"))
}
pub(super) unsafe fn libusb_attach_kernel_driver(_: *mut c_void, interface: c_int) -> c_int {
    call(&format!("attach{interface}"))
}
pub(super) unsafe fn libusb_claim_interface(_: *mut c_void, interface: c_int) -> c_int {
    call(&format!("claim{interface}"))
}
pub(super) unsafe fn libusb_release_interface(_: *mut c_void, interface: c_int) -> c_int {
    call(&format!("release{interface}"))
}
pub(super) unsafe fn libusb_control_transfer(
    _: *mut c_void,
    kind: u8,
    _: u8,
    _: u16,
    _: u16,
    data: *mut u8,
    length: u16,
    _: c_uint,
) -> c_int {
    if kind == 0x80 {
        call("config");
        let config = crate::adapter::tests::config(false);
        assert!(config.len() <= length as usize);
        unsafe {
            std::ptr::copy_nonoverlapping(config.as_ptr(), data, config.len());
        }
        return config.len() as c_int;
    }
    let result = call("command");
    if result == 0 { length as c_int } else { result }
}
pub(super) unsafe fn libusb_interrupt_transfer(
    _: *mut c_void,
    _: u8,
    _: *mut u8,
    _: c_int,
    actual: *mut c_int,
    _: c_uint,
) -> c_int {
    USB.with(|usb| {
        let usb = usb.borrow();
        unsafe {
            *actual = usb.actual;
        }
        usb.result
    })
}
pub(super) unsafe fn libusb_bulk_transfer(
    handle: *mut c_void,
    endpoint: u8,
    data: *mut u8,
    length: c_int,
    actual: *mut c_int,
    timeout: c_uint,
) -> c_int {
    unsafe { libusb_interrupt_transfer(handle, endpoint, data, length, actual, timeout) }
}

#[test]
fn identity_is_checked_before_ownership_changes() {
    reset();
    let device = open().unwrap();
    assert_eq!(device.revision, 0x2520);
    assert_eq!(
        calls(),
        [
            "init",
            "wrap",
            "device",
            "descriptor",
            "config",
            "active0",
            "detach0",
            "claim0",
            "active1",
            "detach1",
            "claim1",
            "reset"
        ]
    );
    device.finish(Ok(())).unwrap();
    for vendor in [0x1234, 0] {
        reset();
        USB.with(|usb| usb.borrow_mut().vendor = vendor);
        assert!(open().is_err());
        assert!(
            !calls()
                .iter()
                .any(|c| c.starts_with("detach") || c.starts_with("claim") || c == "reset")
        );
        assert!(calls().ends_with(&["close".into(), "exit".into()]));
    }
}

#[test]
fn partial_setup_releases_only_acquired_resources() {
    for (operation, release, attach) in [
        ("init", false, false),
        ("wrap", false, false),
        ("descriptor", false, false),
        ("detach0", false, false),
        ("claim0", false, true),
        ("reset", true, true),
    ] {
        reset();
        USB.with(|usb| usb.borrow_mut().fail = Some((operation, -1)));
        assert!(open().is_err());
        assert_eq!(calls().contains(&"release0".into()), release, "{operation}");
        assert_eq!(
            calls().contains(&"active0".into())
                && calls().iter().filter(|c| *c == "active0").count() > 1,
            attach,
            "{operation}"
        );
        assert_eq!(calls().contains(&"exit".into()), operation != "init");
    }
}

#[test]
fn explicit_cleanup_is_ordered_and_runs_once() {
    reset();
    let device = open().unwrap();
    device.claim_voice().unwrap();
    USB.with(|usb| usb.borrow_mut().calls.clear());
    device.finish(Ok(())).unwrap();
    assert_eq!(
        calls(),
        [
            "release1", "release0", "active0", "attach0", "active1", "attach1", "close", "exit"
        ]
    );
}

#[test]
fn disappearance_does_not_attempt_to_rebind_an_absent_device() {
    for operation in ["command", "release0", "attach0"] {
        reset();
        let device = open().unwrap();
        USB.with(|usb| {
            let mut usb = usb.borrow_mut();
            usb.calls.clear();
            usb.fail = Some((operation, LIBUSB_NO_DEVICE));
        });
        if operation == "command" {
            let error = device.command(&mut [3, 12, 0]).unwrap_err();
            assert!(error.contains("removed"));
            device.finish(Err(error)).unwrap_err();
            assert_eq!(calls(), ["command", "close", "exit"]);
        } else {
            device.finish(Ok(())).unwrap();
            assert_eq!(calls().contains(&"attach0".into()), operation == "attach0");
        }
    }
}

#[test]
fn cleanup_failure_does_not_hide_the_operation_failure() {
    reset();
    let device = open().unwrap();
    USB.with(|usb| usb.borrow_mut().fail = Some(("attach0", -6)));
    let error = device.finish(Err("receiver failed".into())).unwrap_err();
    assert!(error.contains("receiver failed") && error.contains("reattach USB interface 0"));
    assert!(calls().ends_with(&["close".into(), "exit".into()]));
}

#[test]
fn receive_counts_are_bounded_and_timeout_keeps_partial_bytes() {
    reset();
    let device = Device::test_device();
    for result in [0, LIBUSB_TIMEOUT] {
        for count in [-1, 0, 3, 4, 5] {
            USB.with(|usb| {
                let mut usb = usb.borrow_mut();
                usb.result = result;
                usb.actual = count;
            });
            let actual = device.events(&mut [0; 4], 10);
            if (0..=4).contains(&count) {
                assert_eq!(actual.unwrap(), count as usize);
            } else {
                assert!(actual.is_err());
            }
        }
    }
    USB.with(|usb| usb.borrow_mut().result = LIBUSB_NO_DEVICE);
    assert!(
        device
            .acl_in(&mut [0; 4], 10)
            .unwrap_err()
            .contains("removed")
    );
    assert!(device.removed.load(Ordering::Relaxed));
}

#[test]
fn failed_voice_claim_during_open_restores_both_interfaces() {
    reset();
    USB.with(|usb| usb.borrow_mut().fail = Some(("claim1", -6)));
    assert!(open().is_err());
    let history = calls();
    assert!(history.contains(&"active1".into()));
    assert!(history.contains(&"release0".into()));
    assert!(!history.contains(&"release1".into()));
}

#[test]
fn fresh_open_after_removal_reacquires_and_restores_interfaces() {
    reset();
    let device = open().unwrap();
    assert!(device.check(LIBUSB_NO_DEVICE, "receive").is_err());
    device.finish(Ok(())).unwrap();
    reset();
    let device = open().unwrap();
    assert!(!device.removed.load(Ordering::Relaxed));
    USB.with(|usb| usb.borrow_mut().calls.clear());
    device.finish(Ok(())).unwrap();
    assert_eq!(
        calls(),
        [
            "release1", "release0", "active0", "attach0", "active1", "attach1", "close", "exit"
        ]
    );
}

pub(super) unsafe fn libusb_clear_halt(_: *mut c_void, _: u8) -> c_int {
    call("clear-halt")
}

pub(crate) fn receive_fault(result: c_int, actual: c_int) {
    USB.with(|usb| {
        let mut state = usb.borrow_mut();
        state.result = result;
        state.actual = actual;
    });
}
pub(crate) fn receive_calls() -> Vec<String> {
    calls()
}
pub(crate) fn reset_receive() {
    reset();
}

#[test]
fn adapter_without_an_active_kernel_driver_is_not_rebound_on_stop() {
    reset();
    USB.with(|usb| usb.borrow_mut().kernel_active = false);
    let device = open().unwrap();
    device.finish(Ok(())).unwrap();
    assert!(
        !calls()
            .iter()
            .any(|call| call.starts_with("detach") || call.starts_with("attach"))
    );
    assert!(calls().contains(&"release0".into()) && calls().contains(&"release1".into()));
}

pub(super) fn cached_descriptors(_: &str) -> Result<(Vec<u8>, u8), String> {
    call("config");
    let mut bytes = vec![0; 18];
    bytes[0] = 18;
    bytes[1] = 1;
    bytes[17] = 1;
    bytes[8..10].copy_from_slice(&0x0a12u16.to_le_bytes());
    bytes[10..12].copy_from_slice(&1u16.to_le_bytes());
    bytes[12..14].copy_from_slice(&0x2520u16.to_le_bytes());
    bytes.extend(crate::adapter::tests::config(false));
    Ok((bytes, 1))
}
