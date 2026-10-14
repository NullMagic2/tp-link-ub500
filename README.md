# TP-Link UB500 Rust USB driver

Rust userspace Bluetooth transport for TP-Link UB500 (`2357:0604`), using libusb and Linux VHCI.
BlueZ continues to handle pairing, Bluetooth profiles and networking.

**Current release: 0.1.3**, using the shared `magic-usb` 0.1.3 transport.
This repository includes the complete shared library and CSR client workspace,
so it builds without checking out another repository. The shared transport is
also included in [Magic Hotspot](https://github.com/NullMagic2/magic-hotspot).

## What it does

- Claims one explicitly selected adapter, leaving other adapters available.
- Validates cached USB descriptors before ownership transfer, allowing recovery
  when CSR firmware stops answering descriptor requests.
- Relays HCI commands, events, ACL data and SCO voice records through Linux VHCI.
- Waits through device swaps on the selected port when run through the service.
- Returns only previously owned interfaces to `btusb` on stop, when possible.
- Uses the distro's Realtek firmware for UB500 initialization.

No kernel patch, `CONFIG_RUST`, global `btusb` blacklist or replacement BlueZ
stack is required. The TP-Link implementation here is the current userspace
transport, not the earlier experimental UB500 kernel module.

## Build

Requires Rust with edition 2024 support (tested with the installed stable
toolchain), Cargo, a C linker, libusb 1.0 and libzstd runtimes.

```sh
./build.sh
./bin/magic-usb check
```

The workspace builds `bin/magic-usb` and `bin/csr8510`. The CSR entry point
restricts ownership to CSR devices; the generic entry point supports both IDs.
Source builds are offline and locked. See [operating notes](README.txt).

## Use

For integrated installation, use the distro-matching package from
[Magic Hotspot releases](https://github.com/NullMagic2/magic-hotspot/releases).
It includes both clients and `magic-usb@.service`.

Select your adapter's USB port using `lsusb -t` and `/sys/bus/usb/devices`.
The following uses `1-4` only as an example:

```sh
sudo systemctl start magic-usb@1-4.service
systemctl status magic-usb@1-4.service
sudo systemctl stop magic-usb@1-4.service
```

Installation does not enable the service at boot. Startup is an explicit choice.
Starting takes ownership of that adapter; stopping restores its prior `btusb`
bindings when possible. Existing connections end when an adapter is removed or
ownership changes. Do not run two clients against the same adapter.

## Validation and limits

46 shared transport tests passed. Physical CSR initialization and a live
CSR-to-UB500 swap were verified on Ubuntu 26.04.1, kernel 7.0.0-38-generic.
Bluetooth PAN traffic was previously verified with UB500.

A separate OBEX simulation passed six transfers through BlueZ and virtual
controllers. It bypasses this USB relay and does not establish physical
file-transfer reliability. Audio interoperability, every UB500 revision and
other distro/kernel combinations are not fully validated.

## License

The shared driver and CSR client are **MIT** licensed;
see [COPYING](COPYING). Distro firmware is not bundled in this repository.

Standalone `.deb` packages are available for Ubuntu, Debian and Linux Mint. See [PACKAGING.md](PACKAGING.md) for build targets and validation limits.
