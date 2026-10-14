Magic USB 0.1.2 — bundled in Magic Hotspot 0.2.23

Shared Rust userspace USB transport for TP-Link UB 500 (2357:0604) and
CSR8510 (0a12:0001). magic-usb and clients/csr8510 use the same Rust library;
the CSR compatibility entry point retains the measured CSR firmware corrections.
No CONFIG_RUST, kernel patch, global btusb blacklist or replacement Bluetooth
stack is required. libusb and libzstd are distro runtime dependencies.

One adapter's USB interfaces have one owner at a time. Explicitly starting
magic-usb on its port transfers that adapter from btusb to the userspace relay.
Other adapters remain available through btusb. Linux VHCI exposes the selected
controller to BlueZ, so existing pairing keys and NetworkManager PAN profiles
remain in their usual stores. Normal stop restores btusb only when it previously owned this adapter.
If btusb was not bound, stopping magic-usb only releases its own USB claims;
the adapter remains unclaimed. Neither path starts or reloads btusb.
A root-only runtime ownership journal makes post-stop crash cleanup conditional
on the original bindings and the same physical attachment generation. It skips
absent state, initially unbound interfaces, removed/replaced devices, an unloaded
btusb module, and interfaces now owned by another driver. Failed restoration
retains the journal for retry. Other adapters and the BlueZ process stay running.
Connections on the selected adapter can reset during ownership handoff.
The original installed CSR executable is not overwritten by Magic Hotspot.

Build and run:
  ./build.sh
  sudo ./bin/magic-usb serve 1-4
  sudo ./bin/magic-usb serve 1-4 --seconds=30
  ./bin/magic-usb check
  ./clean_build.sh                  (keeps bin/ and outputs/)

Install the parent Magic Hotspot package and explicitly select a port:
  cd ..
  sudo apt install ./outputs/magic-hotspot_0.2.23-1ubuntu26.04_amd64.deb
  sudo systemctl start magic-usb@1-4.service
  systemctl status magic-usb@1-4.service
  sudo systemctl stop magic-usb@1-4.service

Installation does not enable startup. To opt into startup for a selected port:
  sudo systemctl enable magic-usb@1-4.service
The instance conflicts with csr8510@1-4.service, preventing two service owners.
There is no global btusb change. Stop magic-usb before using a program that
requires direct btusb access to that same adapter. The service waits when the selected port is empty or holds an unsupported device.
Startup failures retry with backoff from 3 to 30 seconds; adapter swaps do not
permanently exhaust a restart limit.

The Magic Hotspot package places a shared CSR client at /usr/libexec/magic-usb/csr8510.
It is built from clients/csr8510 and links the common crate with path ../..
without duplicating transport source. It restricts the opened device to CSR.
Existing CSR service commands can use this client after stopping the old owner;
the generic magic-usb service also supports CSR directly.

Protocol behavior:
- Validate opened device identity and configuration before detaching anything.
- Select endpoints from descriptors, not from the USB product's marketing name.
- Preserve standard HCI command lengths, statuses, credits and framing.
- Do not apply CSR packet corrections to the Realtek adapter.
- Read RTL8761BU epatch/config firmware from /usr/lib/firmware/rtl_bt; decompress
  .bin.zst with a bounded output. Firmware bytes are not included in this source.
- Initialize Realtek ROM via checked HCI fragments before exposing VHCI. A warm
  adapter retains loaded firmware; unsupported controller revisions fail clearly.
- Retry a stalled event or ACL receive endpoint three times with 50 ms delay.
  Only received data resets the budget. Outgoing commands/data are not replayed.
- Scope worker lifetimes, cancel/drain SCO callbacks, close VHCI before releasing
  USB claims, then restore btusb interface 0 and its companion voice interface.
- Capture is off. --capture=NEW-FILE creates a private, exclusive btsnoop file
  and stops recording at 16 MiB. Raw captures may contain pairing keys.

Verification on 13 October 2026, Ubuntu 26.04.1 / kernel 7.0.0-38:
44 Rust tests passed, including firmware bounds, descriptor validation, framing,
partial USB acquisition, disconnect/reopen, callback drain and driver cleanup.
Both release clients build offline against runtime libraries. A 35-second
physical UB 500 run registered VHCI, initialized BlueZ, powered the controller,
and supported discovery. Both USB interfaces returned to btusb on normal exit.
The installed system service also started successfully; its stop/start and
restoration were verified. The parser validated the distro firmware/config for
ROM version 1 and produced the expected 30,210-byte payload.
The iPhone's USB Internet connection was independently verified with bound and
ordinary HTTPS requests returning HTTP 200 before the hotspot was disabled.

Limits: cold ROM firmware upload, physical CSR use through this extracted library,
headset voice, real endpoint stalls, suspend/resume, forced-kill cleanup and
Bluetooth hotspot Internet need hardware validation. Existing parser/lifecycle
tests do not prove those behaviors. Hotspot was disabled at the user's request;
no Bluetooth Internet claim is made. clippy is unavailable in the local toolchain.

Source references:
https://github.com/torvalds/linux/blob/v7.0/drivers/bluetooth/btrtl.c
https://github.com/torvalds/linux/blob/v7.0/drivers/bluetooth/hci_vhci.c
https://libusb.sourceforge.io/api-1.0/group__libusb__syncio.html
Core transport and tests are adapted from the user's CSR8510 1.0.0 project.
License: GPL-2.0-only; COPYING contains the license text.

0.1.1 ownership validation:
Normal stop and forced process exit restore only interfaces previously bound to
btusb. Starting with both interfaces unbound, normal stop and forced exit leave
them unbound. All four physical UB 500 cases passed; the BlueZ daemon PID stayed
unchanged. An interrupted firmware startup in the forced-exit stress check left
the controller unresponsive. A reset of this USB device recovered it; the service
was then restarted and the powered controller verified. Cold firmware loading
and automatic recovery of interrupted loading remain experimental.

Packaging integration:
This folder is a component of Magic Hotspot. Its build.sh builds the transport
and CSR client; the parent build.sh and package.sh ship both in the single
Magic Hotspot .deb. There is no separate magic-usb package to install. The
service still runs independently of the GTK window, and retains explicit port
selection and conditional btusb restoration. Parent clean_build.sh also clears
this component target directory. Runtime commands and service names stay compatible.

Service mode waits through swaps on the selected port and backs off after startup faults. Cached USB configuration validation allows CSR recovery before live control requests. Pairings remain specific to each controller; swapping adapters ends existing connections.
