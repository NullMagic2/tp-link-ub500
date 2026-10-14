//! Module: magic_usb::bridge
//! Purpose: Relay HCI packets between the USB adapter and Linux's virtual HCI.
//! Created: 2026-10-09
//! Architecture: One thread per USB IN endpoint writes into /dev/vhci; the
//! main thread reads /dev/vhci and writes to USB. No queues in between.
//! Voice (SCO) packets go through sco.rs on the isochronous interface.
//! Every packet can also be appended to a btsnoop file for later analysis.

use crate::{STOP, hci, sco, usb};
use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::os::raw::c_int;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::sync::Mutex;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant, SystemTime};

const O_NONBLOCK: i32 = 0o4000;
const POLLIN: i16 = 1;
const POLL_FAILURE: i16 = 0x08 | 0x10 | 0x20;
const WAIT_MS: u32 = 100;
const H4_BYTES: usize = 1 + 4 + u16::MAX as usize;
const CAPTURE_BYTES: usize = 16 * 1024 * 1024;
const SNOOP_HEADER: &[u8] = b"btsnoop\0\0\0\0\x01\0\0\x03\xea";

#[repr(C)]
struct PollFd {
    fd: c_int,
    events: i16,
    revents: i16,
}

unsafe extern "C" {
    fn poll(fds: *mut PollFd, count: usize, timeout: c_int) -> c_int;
}

/// Signals every other worker on normal return, error or unwinding.
struct StopOnDrop;

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        STOP.store(true, Ordering::Relaxed);
    }
}

struct Capture {
    file: Option<File>,
    remaining: usize,
}

impl Capture {
    fn open(path: &str) -> Result<Self, String> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .map_err(|error| format!("create capture {path}: {error}"))?;
        file.write_all(SNOOP_HEADER)
            .map_err(|error| format!("capture header: {error}"))?;
        Ok(Self {
            file: Some(file),
            remaining: CAPTURE_BYTES - SNOOP_HEADER.len(),
        })
    }

    /// Appends one H4 packet to the capture. btsnoop flags: bit 0 received, bit 1 command/event.
    fn record(&mut self, packet: &[u8], received: bool) {
        let Some(file) = &mut self.file else { return };
        let Some(kind) = packet.first() else { return };
        if 24 + packet.len() > self.remaining {
            self.file = None;
            eprintln!("HCI capture limit reached; capture stopped, relay continues");
            return;
        }
        let stamp = SystemTime::UNIX_EPOCH
            .elapsed()
            .map_or(0, |t| t.as_micros() as u64)
            + 0x00DC_DDB3_0F2F_8000;
        let flags = u32::from(received)
            | if matches!(*kind, hci::COMMAND | hci::EVENT) {
                2
            } else {
                0
            };
        let mut record = Vec::with_capacity(24 + packet.len());
        for field in [packet.len() as u32, packet.len() as u32, flags, 0] {
            record.extend_from_slice(&field.to_be_bytes());
        }
        record.extend_from_slice(&stamp.to_be_bytes());
        record.extend_from_slice(packet);
        if let Err(error) = file.write_all(&record) {
            // A failed diagnostic file must not disconnect a working controller.
            self.file = None;
            eprintln!("HCI capture failed: {error}; capture stopped, relay continues");
        } else {
            self.remaining -= record.len();
        }
    }
}

/// The host side of the relay: /dev/vhci plus an explicit, private capture.
struct Host {
    vhci: File,
    snoop: Option<Mutex<Capture>>,
}

impl Host {
    fn snoop(&self, packet: &[u8], received: bool) -> Result<(), String> {
        if let Some(snoop) = &self.snoop {
            snoop
                .lock()
                .map_err(|_| "HCI capture lock poisoned")?
                .record(packet, received);
        }
        Ok(())
    }

    /// VHCI is record-oriented: one write is one HCI packet.
    fn to_kernel(&self, packet: &[u8]) -> Result<(), String> {
        self.snoop(packet, true)?;
        write_record(&self.vhci, packet)
    }
}

/// Retry interrupted whole records only; never send the remainder of a short write.
fn write_record(mut target: impl Write, packet: &[u8]) -> Result<(), String> {
    loop {
        match target.write(packet) {
            Ok(count) if count == packet.len() => return Ok(()),
            Ok(count) => return Err(format!("short VHCI write: {count}/{}", packet.len())),
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(error) => return Err(format!("write VHCI: {error}")),
        }
    }
}

/// Waits up to `timeout_ms` for one record; zero means nothing arrived.
fn read_record(mut vhci: &File, buffer: &mut [u8], timeout_ms: u32) -> Result<usize, String> {
    let mut fd = PollFd {
        fd: vhci.as_raw_fd(),
        events: POLLIN,
        revents: 0,
    };
    // The pollfd lives on this stack frame for the whole call.
    let ready = unsafe { poll(&mut fd, 1, timeout_ms as c_int) };
    if ready < 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() == ErrorKind::Interrupted {
            return Ok(0);
        }
        return Err(format!("poll VHCI: {error}"));
    }
    if ready == 0 {
        return Ok(0);
    }
    if fd.revents & POLLIN == 0 {
        return if fd.revents & POLL_FAILURE != 0 {
            Err(format!("VHCI poll failure: {:#04x}", fd.revents))
        } else {
            Ok(0)
        };
    }
    match vhci.read(buffer) {
        Ok(0) => Err("virtual HCI closed".into()),
        Ok(count) => Ok(count),
        Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) => {
            Ok(0)
        }
        Err(error) => Err(format!("read VHCI: {error}")),
    }
}

fn register(vhci: &File, record: &mut [u8]) -> Result<u16, String> {
    write_record(vhci, &[0xff, 0x00])?;
    // Read the whole record before checking its size: a small buffer discards
    // the suffix and could turn an oversized acknowledgment into a valid prefix.
    if read_record(vhci, record, 2000)? != 4 || record[..2] != [0xff, 0x00] {
        return Err("VHCI did not acknowledge controller creation".into());
    }
    Ok(u16::from_le_bytes([record[2], record[3]]))
}

fn receive(
    device: &usb::Device,
    event: bool,
    buffer: &mut [u8],
    budget: &mut crate::recovery::Budget,
    framer: &mut hci::Framer,
) -> Result<usize, String> {
    let result = if event {
        device.events(buffer, WAIT_MS)
    } else {
        device.acl_in(buffer, WAIT_MS)
    };
    match result {
        Ok(count) => {
            if count > 0 {
                budget.data_received();
            }
            Ok(count)
        }
        Err(error) if error.ends_with("libusb error -9") && budget.retry() => {
            // The synchronous transfer has returned; clear-halt is outside callbacks.
            framer.reset();
            device.clear_receive_stall(event)?;
            std::thread::sleep(Duration::from_millis(50));
            Ok(0)
        }
        Err(error) => Err(error),
    }
}

fn relay_events(
    device: &usb::Device,
    host: &Host,
    voice: std::pin::Pin<&sco::Voice>,
) -> Result<(), String> {
    let mut framer = hci::Framer::new(hci::Kind::Event);
    let mut buffer = [0; usb::EVENT_BYTES];
    let mut affected = false;
    let mut reported = 0;
    let mut budget = crate::recovery::Budget::default();
    while !STOP.load(Ordering::Relaxed) {
        let count = receive(device, true, &mut buffer, &mut budget, &mut framer)?;
        for mut event in framer.push(&buffer[..count]) {
            if device.adapter == crate::adapter::Adapter::Csr8510 {
                if let Some(message) = hci::compatibility_notice(&event, &mut reported) {
                    eprintln!("{message}");
                }
                hci::filter(&mut event, device.revision, &mut affected);
            }
            event.insert(0, hci::EVENT);
            host.to_kernel(&event)?;
            let event = &event[1..];
            if let Some(line) = hci::describe(event) {
                println!("{line}");
            }
            voice.observe(event)?;
        }
    }
    Ok(())
}

fn relay_acl_in(device: &usb::Device, host: &Host) -> Result<(), String> {
    let mut framer = hci::Framer::new(hci::Kind::Acl);
    let mut buffer = [0; usb::ACL_BYTES];
    let mut budget = crate::recovery::Budget::default();
    while !STOP.load(Ordering::Relaxed) {
        let count = receive(device, false, &mut buffer, &mut budget, &mut framer)?;
        for mut packet in framer.push(&buffer[..count]) {
            if device.adapter == crate::adapter::Adapter::Csr8510
                && hci::declare_service_changed_indicate(&mut packet)
            {
                println!("Workaround: Service Changed declared indicatable for BlueZ");
            }
            packet.insert(0, hci::ACL);
            host.to_kernel(&packet)?;
        }
    }
    Ok(())
}

fn relay_outgoing(
    device: &usb::Device,
    host: &Host,
    voice: std::pin::Pin<&sco::Voice>,
    deadline: Option<Instant>,
    record: &mut [u8],
) -> Result<(), String> {
    while !STOP.load(Ordering::Relaxed) && deadline.is_none_or(|deadline| Instant::now() < deadline)
    {
        let count = read_record(&host.vhci, record, WAIT_MS)?;
        if count != 0 {
            host.snoop(&record[..count], false)?;
            if matches!(record[0], hci::COMMAND | hci::ACL | hci::SCO)
                && !hci::valid_packet(record[0], &record[1..count])
            {
                return Err(format!(
                    "malformed outgoing H4 type {:#04x}: {count} bytes",
                    record[0]
                ));
            }
        }
        match record[..count] {
            [] => {}
            [hci::COMMAND, lo, hi, ..] => {
                device.command(&mut record[1..count]).map_err(|error| {
                    format!("HCI opcode {:#06x}: {error}", u16::from_le_bytes([lo, hi]))
                })?
            }
            [hci::ACL, ..] => device.acl_out(&mut record[1..count])?,
            [hci::SCO, ..] => voice.send(&record[1..count])?,
            [kind, ..] => return Err(format!("unsupported outgoing H4 packet type {kind:#04x}")),
        }
    }
    Ok(())
}

/// Registers a virtual controller and relays until the deadline, a signal or an error.
/// Closing /dev/vhci unregisters the controller before Device's Drop restores btusb.
pub fn serve(port: &str, seconds: Option<u64>, snoop_path: Option<&str>) -> Result<(), String> {
    serve_as(port, seconds, snoop_path, None)
}

pub fn serve_as(
    port: &str,
    seconds: Option<u64>,
    snoop_path: Option<&str>,
    required: Option<crate::adapter::Adapter>,
) -> Result<(), String> {
    let deadline = seconds
        .map(|seconds| {
            Instant::now()
                .checked_add(Duration::from_secs(seconds))
                .ok_or("--seconds exceeds monotonic clock range")
        })
        .transpose()?;
    let snoop = snoop_path.map(Capture::open).transpose()?.map(Mutex::new);
    let device = usb::Device::open_as(port, required)?;
    let result = (|| {
        crate::realtek::initialize(&device)?;
        let vhci = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(O_NONBLOCK)
            .open("/dev/vhci")
            .map_err(|error| {
                crate::access_error(
                    "/dev/vhci",
                    error,
                    "try sudo modprobe hci_vhci; the distro kernel must provide CONFIG_BT_HCIVHCI",
                )
            })?;
        let mut record = vec![0; H4_BYTES];
        let index = register(&vhci, &mut record)?;
        println!(
            "Relay registered as hci{}; {}; capture {}",
            index,
            seconds.map_or("until stopped".into(), |seconds| format!("{seconds}s")),
            snoop_path.unwrap_or("off"),
        );
        let host = Host { vhci, snoop };
        let voice_error = Mutex::new(None);
        let sink = |packet: &[u8]| {
            if let Err(error) = host.to_kernel(packet) {
                if let Ok(mut first) = voice_error.lock() {
                    first.get_or_insert(error);
                }
                STOP.store(true, Ordering::Relaxed);
            }
        };
        let voice = std::pin::pin!(sco::Voice::new(&device, &sink));
        let voice = voice.as_ref();
        std::thread::scope(|scope| {
            let _stop = StopOnDrop;
            let events = scope.spawn(|| {
                let _stop = StopOnDrop;
                relay_events(&device, &host, voice)
            });
            let acl = scope.spawn(|| {
                let _stop = StopOnDrop;
                relay_acl_in(&device, &host)
            });
            let pump = scope.spawn(|| {
                let _stop = StopOnDrop;
                voice.pump()
            });
            let outgoing = {
                let _stop = StopOnDrop;
                relay_outgoing(&device, &host, voice, deadline, &mut record)
            };
            // Join every worker before choosing an error; no early ? can skip a join.
            let events = events
                .join()
                .map_err(|_| "USB event reader panicked".to_owned())
                .and_then(|result| result);
            let acl = acl
                .join()
                .map_err(|_| "USB ACL reader panicked".to_owned())
                .and_then(|result| result);
            let pump = pump
                .join()
                .map_err(|_| "voice pump panicked".to_owned())
                .and_then(|result| result);
            let shutdown = voice.shutdown();
            let voice_result = voice_error
                .lock()
                .map_err(|_| "voice error lock poisoned".to_owned())?
                .take()
                .map_or(Ok(()), Err);
            events
                .and(acl)
                .and(pump)
                .and(voice_result)
                .and(outgoing)
                .and(shutdown)
        })
    })();
    device.finish(result)
}

#[cfg(test)]
#[path = "../tests/bridge/mod.rs"]
mod tests;
