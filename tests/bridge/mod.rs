//! Module: csr8510::bridge::tests
//! Purpose: Regression tests and fixtures for bridge.
//! Created: 2026-10-11
//! Architecture: Test-only child module; private production helpers remain accessible.

use super::*;
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixDatagram;
use std::sync::atomic::AtomicUsize;

fn record_pair() -> (File, UnixDatagram) {
    let (socket, peer) = UnixDatagram::pair().unwrap();
    socket.set_nonblocking(true).unwrap();
    let fd: OwnedFd = socket.into();
    (File::from(fd), peer)
}

fn capture_path() -> String {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    std::env::temp_dir()
        .join(format!(
            "csr8510-capture-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
        .to_str()
        .unwrap()
        .to_owned()
}

#[test]
fn poll_timeout_readiness_and_closed_peer_are_distinct() {
    let (file, peer) = record_pair();
    let mut bytes = [0; 8];
    assert_eq!(read_record(&file, &mut bytes, 0).unwrap(), 0);
    peer.send(&[1, 2, 3]).unwrap();
    assert_eq!(read_record(&file, &mut bytes, 0).unwrap(), 3);
    assert_eq!(&bytes[..3], &[1, 2, 3]);
    unsafe extern "C" {
        fn pipe2(fds: *mut c_int, flags: c_int) -> c_int;
    }
    let mut fds = [-1; 2];
    // Successful pipe2 creates two owned descriptors; each File closes one.
    assert_eq!(unsafe { pipe2(fds.as_mut_ptr(), O_NONBLOCK) }, 0);
    let file = unsafe { File::from_raw_fd(fds[0]) };
    let mut peer = unsafe { File::from_raw_fd(fds[1]) };
    peer.write_all(&[4, 5]).unwrap();
    drop(peer);
    // A hangup may accompany readable data; consume it before reporting closure.
    assert_eq!(read_record(&file, &mut bytes, 0).unwrap(), 2);
    assert!(read_record(&file, &mut bytes, 0).is_err());
}

#[test]
fn registration_requires_the_entire_acknowledgment() {
    for ack in [
        &[0xff, 0, 0x34, 0x12][..],
        &[0xff, 0, 0x34, 0x12, 0],
        &[0xff, 0, 0x34],
        &[0xfe, 0, 0x34, 0x12],
    ] {
        let (file, peer) = record_pair();
        peer.send(ack).unwrap();
        let mut record = vec![0; H4_BYTES];
        let result = register(&file, &mut record);
        if ack.len() == 4 && ack[0] == 0xff {
            assert_eq!(result, Ok(0x1234));
        } else {
            assert!(result.is_err());
        }
        let mut request = [0; 8];
        assert_eq!(peer.recv(&mut request).unwrap(), 2);
        assert_eq!(&request[..2], &[0xff, 0]);
    }
}

#[test]
fn record_writes_retry_interruptions_but_never_partial_remainders() {
    struct Writer {
        attempts: Vec<Vec<u8>>,
        interrupted: bool,
        count: usize,
    }
    impl Write for Writer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.attempts.push(bytes.to_vec());
            if self.interrupted {
                self.interrupted = false;
                Err(ErrorKind::Interrupted.into())
            } else {
                Ok(self.count)
            }
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let packet = [0xff, 0];
    let mut interrupted = Writer {
        attempts: Vec::new(),
        interrupted: true,
        count: 2,
    };
    write_record(&mut interrupted, &packet).unwrap();
    assert_eq!(interrupted.attempts, vec![packet, packet]);
    for count in [0, 1] {
        let mut short = Writer {
            attempts: Vec::new(),
            interrupted: false,
            count,
        };
        assert!(write_record(&mut short, &packet).is_err());
        assert_eq!(short.attempts, vec![packet]);
    }
}

#[test]
fn capture_is_private_exclusive_and_stops_on_a_complete_record_boundary() {
    let path = capture_path();
    let mut capture = Capture::open(&path).unwrap();
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(Capture::open(&path).is_err());
    let packet = [hci::EVENT, 0x0e, 4, 1, 3, 0x0c, 0];
    capture.remaining = 24 + packet.len();
    capture.record(&packet, true);
    capture.record(&packet, true);
    assert!(capture.file.is_none());
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(&bytes[..16], SNOOP_HEADER);
    assert_eq!(bytes.len(), 16 + 24 + packet.len());
    assert_eq!(
        u32::from_be_bytes(bytes[16..20].try_into().unwrap()),
        packet.len() as u32
    );
    assert_eq!(u32::from_be_bytes(bytes[24..28].try_into().unwrap()), 3);
    assert_eq!(&bytes[40..], &packet);
    capture.record(&packet, true);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn capture_failure_disables_recording_without_repeated_writes() {
    let path = capture_path();
    drop(Capture::open(&path).unwrap());
    let mut capture = Capture {
        file: Some(File::open(&path).unwrap()),
        remaining: CAPTURE_BYTES,
    };
    capture.record(&[hci::COMMAND, 3, 0x0c, 0], false);
    assert!(capture.file.is_none());
    capture.record(&[hci::COMMAND, 3, 0x0c, 0], false);
    assert_eq!(std::fs::read(&path).unwrap(), SNOOP_HEADER);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn worker_return_and_unwind_request_stop() {
    let _serial = crate::STOP_TEST_LOCK.lock().unwrap();
    for panic in [false, true] {
        STOP.store(false, Ordering::Relaxed);
        let result = std::thread::spawn(move || {
            let _stop = StopOnDrop;
            if panic {
                panic!("injected reader failure");
            }
        })
        .join();
        assert_eq!(result.is_err(), panic);
        assert!(STOP.load(Ordering::Relaxed));
    }
}

#[test]
fn stalled_receive_clears_endpoint_and_discards_partial_frame_with_a_limit() {
    let _serial = crate::STOP_TEST_LOCK.lock().unwrap();
    usb::tests::reset_receive();
    let device = usb::Device::test_device();
    let mut budget = crate::recovery::Budget::default();
    let mut framer = hci::Framer::new(hci::Kind::Event);
    assert!(framer.push(&[0x0e, 4, 1]).is_empty());
    usb::tests::receive_fault(-9, 0);
    for _ in 0..3 {
        assert_eq!(
            receive(&device, true, &mut [0; 272], &mut budget, &mut framer).unwrap(),
            0
        );
    }
    assert!(receive(&device, true, &mut [0; 272], &mut budget, &mut framer).is_err());
    assert_eq!(
        usb::tests::receive_calls()
            .iter()
            .filter(|c| *c == "clear-halt")
            .count(),
        3
    );
    assert_eq!(framer.push(&[0x0e, 4, 1, 3, 12, 0]).len(), 1);
    usb::tests::receive_fault(-7, 0);
    assert_eq!(
        receive(&device, true, &mut [0; 272], &mut budget, &mut framer).unwrap(),
        0
    );
    usb::tests::receive_fault(-9, 0);
    assert!(receive(&device, true, &mut [0; 272], &mut budget, &mut framer).is_err());
    usb::tests::receive_fault(0, 1);
    assert_eq!(
        receive(&device, true, &mut [0; 272], &mut budget, &mut framer).unwrap(),
        1
    );
    usb::tests::receive_fault(-9, 0);
    assert_eq!(
        receive(&device, true, &mut [0; 272], &mut budget, &mut framer).unwrap(),
        0
    );
}
