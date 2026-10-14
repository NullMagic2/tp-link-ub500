//! Module: csr8510::sco::tests
//! Purpose: Regression tests and fixtures for sco.
//! Created: 2026-10-11
//! Architecture: Test-only child module; private production helpers remain accessible.

use super::*;

use std::collections::{HashSet, VecDeque};
use std::sync::{Barrier, OnceLock};

#[derive(Default)]
struct Backend {
    allocated: HashSet<usize>,
    submitted: HashSet<usize>,
    ready: VecDeque<(usize, c_int)>,
    freed: usize,
    callbacks: usize,
    cancellations: usize,
    submits: usize,
    fail_submit: usize,
    cancel_error: c_int,
    event_error: c_int,
    alt_error: c_int,
    delay_events: usize,
    stalled: bool,
    resets: usize,
}

fn backend() -> MutexGuard<'static, Backend> {
    static BACKEND: OnceLock<Mutex<Backend>> = OnceLock::new();
    BACKEND
        .get_or_init(|| Mutex::new(Backend::default()))
        .lock()
        .unwrap()
}

fn fixture() -> MutexGuard<'static, ()> {
    let serial = crate::STOP_TEST_LOCK.lock().unwrap();
    assert!(backend().allocated.is_empty());
    *backend() = Backend::default();
    STOP.store(false, Ordering::Relaxed);
    serial
}

unsafe extern "C" {
    #[link_name = "libusb_alloc_transfer"]
    fn allocate_real(frames: c_int) -> *mut Transfer;
    #[link_name = "libusb_free_transfer"]
    fn free_real(transfer: *mut Transfer);
}

// Real libusb allocations, fake USB scheduling: no physical device is opened.
pub(super) unsafe fn libusb_alloc_transfer(frames: c_int) -> *mut Transfer {
    let transfer = unsafe { allocate_real(frames) };
    assert!(backend().allocated.insert(transfer as usize));
    transfer
}

pub(super) unsafe fn libusb_free_transfer(transfer: *mut Transfer) {
    let mut backend = backend();
    assert!(
        !backend.submitted.contains(&(transfer as usize)),
        "freed before callback"
    );
    assert!(
        backend.allocated.remove(&(transfer as usize)),
        "double free"
    );
    backend.freed += 1;
    drop(backend);
    unsafe { free_real(transfer) };
}

pub(super) unsafe fn libusb_submit_transfer(transfer: *mut Transfer) -> c_int {
    let mut backend = backend();
    assert!(backend.allocated.contains(&(transfer as usize)));
    backend.submits += 1;
    if backend.submits == backend.fail_submit {
        return -1;
    }
    assert!(backend.submitted.insert(transfer as usize));
    0
}

pub(super) unsafe fn libusb_cancel_transfer(transfer: *mut Transfer) -> c_int {
    let mut backend = backend();
    assert!(backend.allocated.contains(&(transfer as usize)));
    backend.cancellations += 1;
    if !backend.submitted.contains(&(transfer as usize)) {
        return NOT_FOUND; // Callback already executing on another USB worker.
    }
    if !backend
        .ready
        .iter()
        .any(|&(pointer, _)| pointer == transfer as usize)
    {
        backend.ready.push_back((transfer as usize, 3));
    }
    backend.cancel_error
}

pub(super) unsafe fn libusb_set_interface_alt_setting(
    _: *mut c_void,
    interface: c_int,
    alt: c_int,
) -> c_int {
    assert_eq!(interface, ISO_INTERFACE);
    let mut backend = backend();
    if alt == 0 {
        assert!(
            backend.allocated.is_empty(),
            "alt reset before callbacks finished"
        );
        backend.resets += 1;
        return backend.alt_error;
    }
    0
}

pub(super) unsafe fn libusb_handle_events_timeout_completed(
    _: *mut c_void,
    _: *const Timeval,
    _: *mut c_int,
) -> c_int {
    let completion = {
        let mut backend = backend();
        if backend.event_error != 0 {
            return std::mem::take(&mut backend.event_error);
        }
        if backend.stalled {
            return 0;
        }
        if backend.delay_events != 0 {
            backend.delay_events -= 1;
            // Cancel acceptance has not released any owners or buffers.
            assert_eq!(backend.freed, 0);
            return 0;
        }
        let completion = backend.ready.pop_front();
        if let Some((pointer, _)) = completion {
            assert!(backend.submitted.remove(&pointer));
            backend.callbacks += 1;
        }
        completion
    };
    if let Some((pointer, status)) = completion {
        let transfer = pointer as *mut Transfer;
        unsafe {
            (*transfer).status = status;
            ((*transfer).callback.unwrap())(transfer);
        }
    }
    0
}

fn receive_packet(voice: &Voice<'_>) -> *mut Transfer {
    let mut state = voice.state();
    let pending = &mut state.transfers[0];
    pending.buffer[..4].copy_from_slice(&[0x34, 0, 1, 0xaa]);
    unsafe {
        (*pending.transfer)
            .iso
            .as_mut_ptr()
            .as_mut()
            .unwrap()
            .actual_length = 4;
    }
    backend()
        .ready
        .push_back((pending.transfer as usize, COMPLETED));
    pending.transfer
}

#[test]
fn disconnect_drains_receive_and_send_then_allows_restart() {
    let _serial = fixture();
    let device = usb::Device::test_device();
    let voice = std::pin::pin!(Voice::new(&device, &|_| {}));
    let voice = voice.as_ref();
    voice.start(0x34, false).unwrap();
    voice.start(0x34, false).unwrap(); // Duplicate event must not duplicate ownership.
    voice.start(0x35, false).unwrap();
    voice.send(&[0x34, 0, 1, 0xaa]).unwrap();
    voice
        .observe(&[hci::DISCONNECT_COMPLETE, 4, 1, 0x34, 0, 0])
        .unwrap();
    assert_eq!(voice.state().handles.len(), 2); // Failed disconnection is not teardown.
    voice.stop(0x34).unwrap();
    assert_eq!(voice.state().transfers.len(), 3);
    backend().delay_events = 2;
    voice.stop(0x35).unwrap();
    assert!(voice.state().transfers.is_empty());
    {
        let backend = backend();
        assert_eq!((backend.freed, backend.callbacks), (3, 3));
    }
    assert_eq!(backend().resets, 1);
    voice.start(0x36, true).unwrap();
    voice.shutdown().unwrap();
    assert_eq!(backend().freed, 5);
    voice.shutdown().unwrap();
    voice.start(0x37, false).unwrap();
    voice.send(&[0x34, 0, 1, 0xaa]).unwrap();
    assert_eq!(backend().submits, 5);
    assert_eq!(backend().cancellations, 5);
}

#[test]
fn partial_startup_and_drop_drain_the_successful_submission() {
    let _serial = fixture();
    let device = usb::Device::test_device();
    {
        let voice = std::pin::pin!(Voice::new(&device, &|_| {}));
        let voice = voice.as_ref();
        backend().fail_submit = 2;
        assert!(
            voice
                .start(0x34, false)
                .unwrap_err()
                .contains("submit voice")
        );
        assert_eq!(voice.state().transfers.len(), 1);
        assert_eq!(backend().freed, 1); // Only the unsubmitted transfer freed locally.
    }
    assert!(backend().allocated.is_empty());
    {
        let backend = backend();
        assert_eq!((backend.freed, backend.callbacks), (2, 1));
    }
}

#[test]
fn receive_resubmits_but_global_stop_cancels_and_drains() {
    let _serial = fixture();
    let device = usb::Device::test_device();
    let packets = Mutex::new(Vec::new());
    let sink = |packet: &[u8]| packets.lock().unwrap().push(packet.to_vec());
    let voice = std::pin::pin!(Voice::new(&device, &sink));
    let voice = voice.as_ref();
    voice.start(0x34, false).unwrap();
    receive_packet(&voice);
    voice.events();
    assert_eq!(
        *packets.lock().unwrap(),
        vec![vec![hci::SCO, 0x34, 0, 1, 0xaa]]
    );
    assert_eq!(backend().submits, 3);
    voice.send(&[0x34, 0, 1, 0xaa]).unwrap();
    STOP.store(true, Ordering::Relaxed);
    voice.pump().unwrap();
    assert!(voice.state().transfers.is_empty());
    assert_eq!(backend().freed, 3);
    assert_eq!(backend().submits, 4); // Cancellation did not resubmit RX.
}

#[test]
fn callback_stays_registered_while_sink_runs_on_another_usb_worker() {
    let _serial = fixture();
    let device = usb::Device::test_device();
    let entered = Barrier::new(2);
    let release = Barrier::new(2);
    let slot = std::sync::atomic::AtomicPtr::<Voice>::new(std::ptr::null_mut());
    let sink = |_: &[u8]| {
        // The slot points at the scoped Voice until every worker below has joined.
        let voice = unsafe { &*slot.load(Ordering::Relaxed) };
        assert_eq!(voice.state().transfers.len(), 2); // Sink can lock State.
        entered.wait();
        release.wait();
    };
    let voice = std::pin::pin!(Voice::new(&device, &sink));
    let voice = voice.as_ref();
    slot.store(
        voice.get_ref() as *const Voice as *mut Voice,
        Ordering::Relaxed,
    );
    voice.start(0x34, false).unwrap();
    receive_packet(&voice);
    std::thread::scope(|scope| {
        let callback = scope.spawn(|| voice.events());
        entered.wait();
        let (tx, rx) = std::sync::mpsc::channel();
        let voice = &voice;
        let drain = scope.spawn(move || {
            voice.shutdown().unwrap();
            tx.send(()).unwrap();
        });
        let returned_early = rx.recv_timeout(Duration::from_millis(20)).is_ok();
        release.wait();
        callback.join().unwrap();
        drain.join().unwrap();
        assert!(
            !returned_early,
            "shutdown returned while sink still used Voice"
        );
    });
    assert!(backend().allocated.is_empty());
}

#[test]
fn callback_panic_and_invalid_lengths_retire_without_unwinding_into_libusb() {
    let _serial = fixture();
    let device = usb::Device::test_device();
    for panic in [false, true] {
        STOP.store(false, Ordering::Relaxed);
        let sink = |_: &[u8]| {
            if panic {
                panic!("sink fixture");
            }
        };
        let voice = std::pin::pin!(Voice::new(&device, &sink));
        let voice = voice.as_ref();
        voice.start(0x34, false).unwrap();
        let transfer = receive_packet(&voice);
        if !panic {
            unsafe {
                (*(*transfer).iso.as_mut_ptr()).actual_length = 18;
            }
        }
        voice.events();
        assert!(STOP.load(Ordering::Relaxed));
        assert_eq!(voice.state().transfers.len(), 1);
        let error = voice.shutdown().unwrap_err();
        assert!(
            error.contains(if panic {
                "callback panicked"
            } else {
                "frame length"
            }),
            "{error}"
        );
        assert!(backend().allocated.is_empty());
    }
}

#[test]
fn cancellation_not_found_event_errors_and_resubmit_errors_keep_ownership() {
    let _serial = fixture();
    let device = usb::Device::test_device();
    {
        let voice = std::pin::pin!(Voice::new(&device, &|_| {}));
        let voice = voice.as_ref();
        voice.start(0x34, false).unwrap();
        backend().cancel_error = NOT_FOUND;
        backend().event_error = -1;
        let error = voice.shutdown().unwrap_err();
        assert!(error.contains("voice event handling"));
        assert!(backend().allocated.is_empty());
        assert_eq!(backend().callbacks, 2);
    }
    STOP.store(false, Ordering::Relaxed);
    *backend() = Backend::default();
    let voice = std::pin::pin!(Voice::new(&device, &|_| {}));
    let voice = voice.as_ref();
    voice.start(0x34, false).unwrap();
    backend().fail_submit = 3;
    receive_packet(&voice);
    voice.events();
    assert!(STOP.load(Ordering::Relaxed));
    assert!(voice.pump().unwrap_err().contains("resubmit voice receive"));
    assert!(backend().allocated.is_empty());
}

#[test]
fn poisoned_state_and_unwinding_still_drain_before_device_drop() {
    let _serial = fixture();
    let device = usb::Device::test_device();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let voice = std::pin::pin!(Voice::new(&device, &|_| {}));
        let voice = voice.as_ref();
        voice.start(0x34, false).unwrap();
        let _state = voice.state();
        panic!("worker fixture with pending RX");
    }));
    assert!(result.is_err());
    assert!(STOP.load(Ordering::Relaxed));
    assert!(backend().allocated.is_empty());
    assert_eq!(backend().callbacks, 2);
}

#[test]
fn cancel_and_alt_reset_failures_are_reported_after_all_callbacks_retire() {
    let _serial = fixture();
    let device = usb::Device::test_device();
    let voice = std::pin::pin!(Voice::new(&device, &|_| {}));
    let voice = voice.as_ref();
    voice.start(0x34, false).unwrap();
    backend().cancel_error = -4;
    backend().alt_error = -1;
    let error = voice.shutdown().unwrap_err();
    assert!(error.contains("cancel voice transfer")); // Preserve the first error.
    assert!(backend().allocated.is_empty());
    assert_eq!(backend().callbacks, 2);
    assert_eq!(backend().resets, 1);
    voice.shutdown().unwrap();
}

#[test]
fn stalled_drain_aborts_instead_of_dropping_pending_owners() {
    const CHILD: &str = "CSR8510_TEST_STALLED_DRAIN";
    if std::env::var_os(CHILD).is_some() {
        unsafe extern "C" {
            fn setrlimit(resource: c_int, limit: *const [u64; 2]) -> c_int;
        }
        // Linux x86_64 RLIMIT_CORE; the intentional child abort must not write a core.
        assert_eq!(unsafe { setrlimit(4, &[0, 0]) }, 0);
        let _serial = fixture();
        let device = usb::Device::test_device();
        let voice = std::pin::pin!(Voice::new(&device, &|_| {}));
        let voice = voice.as_ref();
        voice.start(0x34, false).unwrap();
        backend().stalled = true;
        voice.shutdown().unwrap();
        panic!("shutdown returned with outstanding callbacks");
    }
    use std::os::unix::process::ExitStatusExt;
    // Under an explicit old loader /proc/self/exe names the loader, not this test.
    let binary = std::env::args_os().next().unwrap();
    let mut command =
        if let Some(runner) = std::env::var_os("CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER") {
            let mut command = std::process::Command::new(runner);
            command.arg(binary);
            command
        } else {
            std::process::Command::new(binary)
        };
    let result = command
        .args([
            "--exact",
            "sco::tests::stalled_drain_aborts_instead_of_dropping_pending_owners",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .output()
        .unwrap();
    assert_eq!(
        result.status.signal(),
        Some(6),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        String::from_utf8_lossy(&result.stderr)
            .contains("aborting relay without freeing live transfers")
    );
}

#[test]
fn transfer_layout_matches_libusb_1_0() {
    let _serial = fixture();
    // Offsets printed by offsetof() from libusb.h v1.0.27 on x86_64: callback 32,
    // user_data 40, buffer 48, num_iso_packets 56, iso_packet_desc 60, descriptor 12.
    assert_eq!(std::mem::offset_of!(Transfer, callback), 32);
    assert_eq!(std::mem::offset_of!(Transfer, num_iso_packets), 56);
    assert_eq!(std::mem::offset_of!(Transfer, iso), 60);
    assert_eq!(std::mem::size_of::<IsoPacket>(), 12);
    // libusb_alloc_transfer hands back a zeroed structure the caller fills in.
    let transfer = unsafe { libusb_alloc_transfer(10) };
    assert!(!transfer.is_null());
    unsafe {
        assert!((*transfer).dev_handle.is_null() && (*transfer).buffer.is_null());
        assert_eq!((*transfer).num_iso_packets, 0);
        let descriptors = (*transfer).iso.as_mut_ptr();
        (*descriptors.add(9)).length = 17;
        assert_eq!((*descriptors.add(9)).length, 17);
        libusb_free_transfer(transfer);
    }
}
