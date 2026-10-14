//! Module: magic_usb::sco
//! Purpose: Carry SCO voice packets over the isochronous endpoints of interface 1.
//! Created: 2026-10-10
//! Architecture: One locked RX/TX ownership list; libusb callbacks retire transfers.
//! Link changes and shutdown cancel, pump, then release the completed owners.
//! The bridge tells us when voice links come and go and hands us packets to send.

use crate::{STOP, hci, usb};
use std::ffi::c_void;
use std::marker::PhantomPinned;
use std::os::raw::{c_int, c_uint};
use std::pin::Pin;
use std::sync::atomic::Ordering;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

const ISO_INTERFACE: c_int = 1;
const ISOCHRONOUS: u8 = 1;
const COMPLETED: c_int = 0;
const NOT_FOUND: c_int = -5;
const INTERRUPTED: c_int = -10;
const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
/// Frames per receive transfer and transfers in flight, as btusb does it.
const FRAMES: usize = 10;
const RX_TRANSFERS: usize = 2;
/// Alternate settings and their packet sizes: 2 = 17 bytes for CVSD 16-bit, 1 = 9 bytes
/// for transparent (wideband) air mode on adapters without the 63-byte alternate 6.
const ALT_CVSD: (c_int, usize) = (2, 17);
const ALT_TRANSPARENT: (c_int, usize) = (1, 9);
const AIR_MODE_TRANSPARENT: u8 = 0x03;

#[repr(C)]
struct IsoPacket {
    length: c_uint,
    actual_length: c_uint,
    status: c_int,
}

// struct libusb_transfer; the iso descriptors follow the fixed part in memory.
#[repr(C)]
struct Transfer {
    dev_handle: *mut c_void,
    flags: u8,
    endpoint: u8,
    kind: u8,
    timeout: c_uint,
    status: c_int,
    length: c_int,
    actual_length: c_int,
    callback: Option<extern "C" fn(*mut Transfer)>,
    user_data: *mut c_void,
    buffer: *mut u8,
    num_iso_packets: c_int,
    iso: [IsoPacket; 0],
}

#[repr(C)]
struct Timeval {
    sec: i64,
    usec: i64,
}

unsafe extern "C" {
    #[cfg(not(test))]
    fn libusb_alloc_transfer(iso_packets: c_int) -> *mut Transfer;
    #[cfg(not(test))]
    fn libusb_free_transfer(transfer: *mut Transfer);
    #[cfg(not(test))]
    fn libusb_submit_transfer(transfer: *mut Transfer) -> c_int;
    #[cfg(not(test))]
    fn libusb_cancel_transfer(transfer: *mut Transfer) -> c_int;
    #[cfg(not(test))]
    fn libusb_set_interface_alt_setting(handle: *mut c_void, interface: c_int, alt: c_int)
    -> c_int;
    #[cfg(not(test))]
    fn libusb_handle_events_timeout_completed(
        context: *mut c_void,
        timeout: *const Timeval,
        completed: *mut c_int,
    ) -> c_int;
}

#[cfg(test)]
use tests::{
    libusb_alloc_transfer, libusb_cancel_transfer, libusb_free_transfer,
    libusb_handle_events_timeout_completed, libusb_set_interface_alt_setting,
    libusb_submit_transfer,
};

/// Owns a transfer and buffer until submission fails or its callback retires it.
struct Pending {
    transfer: *mut Transfer,
    buffer: Box<[u8]>,
    packet: usize,
}

impl Drop for Pending {
    fn drop(&mut self) {
        // Only unsubmitted transfers or completed callbacks remove this owner.
        unsafe { libusb_free_transfer(self.transfer) };
    }
}

struct State {
    alt: (c_int, usize),
    handles: Vec<u16>,
    transfers: Vec<Pending>,
    framer: hci::Framer,
    closed: bool,
    error: Option<String>,
}

impl State {
    fn fail(&mut self, error: String) {
        self.error.get_or_insert(error);
        STOP.store(true, Ordering::Relaxed);
    }
}

pub struct Voice<'a> {
    device: &'a usb::Device,
    sink: &'a (dyn Fn(&[u8]) + Sync),
    state: Mutex<State>,
    // Serializes link changes and shutdown; callbacks never take this mutex.
    lifecycle: Mutex<()>,
    // libusb stores this owner’s address until every callback has retired.
    _pinned: PhantomPinned,
}

fn fill(
    transfer: *mut Transfer,
    endpoint: u8,
    buffer: &mut [u8],
    packet: usize,
    callback: extern "C" fn(*mut Transfer),
    user_data: *mut c_void,
    handle: *mut c_void,
) {
    // Caller guarantees the transfer was allocated with enough iso descriptors.
    unsafe {
        let count = buffer.len().div_ceil(packet);
        (*transfer).dev_handle = handle;
        (*transfer).endpoint = endpoint;
        (*transfer).kind = ISOCHRONOUS;
        (*transfer).timeout = 0;
        (*transfer).length = buffer.len() as c_int;
        (*transfer).buffer = buffer.as_mut_ptr();
        (*transfer).num_iso_packets = count as c_int;
        (*transfer).callback = Some(callback);
        (*transfer).user_data = user_data;
        let descriptors = (*transfer).iso.as_mut_ptr();
        for index in 0..count {
            let remaining = buffer.len() - index * packet;
            (*descriptors.add(index)).length = remaining.min(packet) as c_uint;
        }
    }
}

extern "C" fn completed(transfer: *mut Transfer) {
    // The registry retains Voice and this buffer through the entire callback.
    let voice = unsafe { &*((*transfer).user_data as *const Voice) };
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| voice.complete(transfer))).is_err()
    {
        voice.state().fail("voice callback panicked".into());
        voice.retire(transfer, false);
    }
}

impl<'a> Voice<'a> {
    pub fn new(device: &'a usb::Device, sink: &'a (dyn Fn(&[u8]) + Sync)) -> Self {
        Self {
            device,
            sink,
            state: Mutex::new(State {
                alt: (0, 0),
                handles: Vec::new(),
                transfers: Vec::new(),
                framer: hci::Framer::new(hci::Kind::Sco),
                closed: false,
                error: None,
            }),
            lifecycle: Mutex::new(()),
            _pinned: PhantomPinned,
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|poisoned| {
            let mut state = poisoned.into_inner();
            self.state.clear_poison();
            state.fail("voice state poisoned".into());
            state
        })
    }

    fn events(&self) {
        let timeout = Timeval {
            sec: 0,
            usec: 100_000,
        };
        // Callbacks can also run on synchronous USB workers. Never hold State here.
        let result = unsafe {
            libusb_handle_events_timeout_completed(
                self.device.context(),
                &timeout,
                std::ptr::null_mut(),
            )
        };
        if result < 0 && result != INTERRUPTED {
            self.state()
                .fail(format!("voice event handling: libusb error {result}"));
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Keeps pumping through cancellation, even after the last link disappears.
    pub fn pump(&self) -> Result<(), String> {
        while !STOP.load(Ordering::Relaxed) {
            if self.state().transfers.is_empty() {
                std::thread::sleep(Duration::from_millis(100));
            } else {
                self.events();
            }
        }
        self.shutdown()
    }

    pub fn observe(self: Pin<&Self>, event: &[u8]) -> Result<(), String> {
        match event {
            [hci::SYNC_CONNECT_COMPLETE, 17, 0, lo, hi, .., air_mode] => self.start(
                u16::from_le_bytes([*lo, *hi]),
                *air_mode == AIR_MODE_TRANSPARENT,
            ),
            [hci::CONNECT_COMPLETE, 11, 0, lo, hi, _, _, _, _, _, _, 0, _] => {
                self.start(u16::from_le_bytes([*lo, *hi]), false)
            }
            [hci::DISCONNECT_COMPLETE, 4, 0, lo, hi, _] => {
                self.stop(u16::from_le_bytes([*lo, *hi]))
            }
            _ => Ok(()),
        }
    }

    fn submit(
        self: Pin<&Self>,
        state: &mut State,
        endpoint: u8,
        buffer: Box<[u8]>,
    ) -> Result<(), String> {
        let packet = state.alt.1;
        let transfer = unsafe { libusb_alloc_transfer(buffer.len().div_ceil(packet) as c_int) };
        if transfer.is_null() {
            return Err("allocate voice transfer".into());
        }
        let mut pending = Pending {
            transfer,
            buffer,
            packet,
        };
        fill(
            transfer,
            endpoint,
            &mut pending.buffer,
            packet,
            completed,
            self.get_ref() as *const Self as *mut c_void,
            self.device.handle(),
        );
        // Register before submission; another USB thread may dispatch immediately.
        state.transfers.push(pending);
        let result = unsafe { libusb_submit_transfer(transfer) };
        if result < 0 {
            state.transfers.pop(); // Failed submission owes no callback.
            return Err(format!(
                "submit voice endpoint {endpoint:#04x}: libusb error {result}"
            ));
        }
        Ok(())
    }

    fn start(self: Pin<&Self>, handle: u16, transparent: bool) -> Result<(), String> {
        let _lifecycle = self
            .lifecycle
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        {
            let mut state = self.state();
            if state.closed || STOP.load(Ordering::Relaxed) {
                return Ok(());
            }
            if state.handles.contains(&handle) {
                return Ok(());
            }
            if state.alt.0 != 0 {
                state.handles.push(handle);
                return Ok(());
            }
        }
        let alt = if transparent && self.device.adapter == crate::adapter::Adapter::Ub500 {
            (3, 25)
        } else if transparent {
            ALT_TRANSPARENT
        } else {
            ALT_CVSD
        };
        self.device.claim_voice()?;
        // No State lock across synchronous USB operations.
        let result =
            unsafe { libusb_set_interface_alt_setting(self.device.handle(), ISO_INTERFACE, alt.0) };
        if result < 0 {
            return Err(format!(
                "select voice alternate setting {}: libusb error {result}",
                alt.0
            ));
        }
        let mut state = self.state();
        state.alt = alt;
        state.handles.push(handle);
        for _ in 0..RX_TRANSFERS {
            self.submit(
                &mut state,
                self.device.endpoints.voice_in,
                vec![0; alt.1 * FRAMES].into_boxed_slice(),
            )?;
        }
        println!(
            "Voice link {handle:#x} started on alternate setting {}",
            alt.0
        );
        Ok(())
    }

    fn stop(&self, handle: u16) -> Result<(), String> {
        let _lifecycle = self
            .lifecycle
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut state = self.state();
        state.handles.retain(|&h| h != handle);
        if state.alt.0 == 0 || !state.handles.is_empty() {
            return Ok(());
        }
        drop(state);
        self.drain(false)?;
        println!("Voice link {handle:#x} stopped");
        Ok(())
    }

    /// Stops submission, cancels RX and TX, then retains all owners until callbacks retire.
    /// The lifecycle lock is held by the caller; callbacks only need State.
    fn drain(&self, closing: bool) -> Result<(), String> {
        let mut state = self.state();
        state.closed |= closing;
        let had_alt = state.alt.0 != 0;
        state.alt = (0, 0);
        state.handles.clear();
        for index in 0..state.transfers.len() {
            let result = unsafe { libusb_cancel_transfer(state.transfers[index].transfer) };
            // NOT_FOUND may mean a completion is already queued: still await callback.
            if result < 0 && result != NOT_FOUND {
                state.fail(format!("cancel voice transfer: libusb error {result}"));
            }
        }
        drop(state);
        let deadline = Instant::now() + DRAIN_TIMEOUT;
        while !self.state().transfers.is_empty() {
            if Instant::now() >= deadline {
                // Returning would let libusb access destroyed Voice/buffers/context.
                eprintln!(
                    "voice callbacks still pending at drain deadline; aborting relay without freeing live transfers"
                );
                std::process::abort();
            }
            self.events();
        }
        if had_alt {
            let result =
                unsafe { libusb_set_interface_alt_setting(self.device.handle(), ISO_INTERFACE, 0) };
            if result < 0 {
                self.state().fail(format!(
                    "reset voice alternate setting: libusb error {result}"
                ));
            }
        }
        let mut state = self.state();
        state.framer = hci::Framer::new(hci::Kind::Sco);
        state.error.take().map_or(Ok(()), Err)
    }

    pub fn shutdown(&self) -> Result<(), String> {
        let _lifecycle = self
            .lifecycle
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.drain(true)
    }

    fn complete(&self, transfer: *mut Transfer) {
        let packets = {
            let mut state = self.state();
            let Some(index) = state
                .transfers
                .iter()
                .position(|pending| pending.transfer == transfer)
            else {
                return;
            };
            let active = state.alt.0 != 0 && !state.closed && !STOP.load(Ordering::Relaxed);
            if !active {
                Vec::new()
            } else if unsafe { (*transfer).status } != COMPLETED {
                state.fail(format!("voice transfer completion status {}", unsafe {
                    (*transfer).status
                }));
                Vec::new()
            } else {
                let State {
                    transfers, framer, ..
                } = &mut *state;
                let pending = &transfers[index];
                let mut packets = Vec::new();
                // Descriptor count/lengths must fit the original allocation and owned buffer.
                let result = (|| -> Result<(), String> {
                    let count = pending.buffer.len().div_ceil(pending.packet);
                    if unsafe { (*transfer).num_iso_packets } != count as c_int {
                        return Err("invalid voice descriptor count".into());
                    }
                    let mut offset = 0;
                    for frame in 0..count {
                        let descriptor = unsafe { &*(*transfer).iso.as_ptr().add(frame) };
                        let length = descriptor.length as usize;
                        let actual = descriptor.actual_length as usize;
                        if length > pending.packet
                            || actual > length
                            || offset + length > pending.buffer.len()
                        {
                            return Err("invalid voice frame length".into());
                        }
                        if descriptor.status != COMPLETED {
                            return Err(format!(
                                "voice frame completion status {}",
                                descriptor.status
                            ));
                        }
                        if unsafe { (*transfer).endpoint } == self.device.endpoints.voice_in {
                            packets.extend(framer.push(&pending.buffer[offset..offset + actual]));
                        }
                        offset += length;
                    }
                    Ok(())
                })();
                if let Err(error) = result {
                    state.fail(error);
                    packets.clear();
                }
                packets
            }
        };
        // Keep the transfer registered while calling the sink, without holding State.
        for mut packet in packets {
            packet.insert(0, hci::SCO);
            (self.sink)(&packet);
        }
        self.retire(transfer, true);
    }

    fn retire(&self, transfer: *mut Transfer, resubmit: bool) {
        let mut state = self.state();
        let Some(index) = state
            .transfers
            .iter()
            .position(|pending| pending.transfer == transfer)
        else {
            return;
        };
        if resubmit
            && state.alt.0 != 0
            && !state.closed
            && !STOP.load(Ordering::Relaxed)
            && unsafe { (*transfer).endpoint } == self.device.endpoints.voice_in
        {
            let result = unsafe { libusb_submit_transfer(transfer) };
            if result == 0 {
                return;
            }
            state.fail(format!("resubmit voice receive: libusb error {result}"));
        }
        state.transfers.swap_remove(index);
    }

    pub fn send(self: Pin<&Self>, packet: &[u8]) -> Result<(), String> {
        let mut state = self.state();
        if state.alt.0 == 0 || state.closed || STOP.load(Ordering::Relaxed) {
            return Ok(());
        }
        self.submit(
            &mut state,
            self.device.endpoints.voice_out,
            packet.to_vec().into_boxed_slice(),
        )
    }
}

impl Drop for Voice<'_> {
    fn drop(&mut self) {
        // Also drains partial startup and unwinding before the borrowed Device can close.
        if let Err(error) = self.shutdown() {
            eprintln!("voice shutdown: {error}");
        }
    }
}

// All transfer ownership is locked; callbacks may run on any USB event-handling thread.
unsafe impl Send for State {}

#[cfg(test)]
#[path = "../tests/sco/mod.rs"]
mod tests;
