# Distribution packages

| Package suffix | Build environment | Intended target |
| --- | --- | --- |
| ubuntu26.04 | Ubuntu 26.04 | Ubuntu 26.04 amd64 |
| ubuntu24.04 | Ubuntu 24.04 | Ubuntu 24.04 amd64 |
| debian13 | Debian 13 | Debian 13 amd64 |
| mint22 | Ubuntu 24.04 (Mint 22's base) | Linux Mint 22.x amd64 |

Dependencies are derived from the ELF binaries against the build distro's
library metadata. Mint packages are base-built and dependency-tested in Ubuntu
24.04; they have not been tested in a full Mint desktop or on Mint hardware.
No hardware passthrough is used in CI. Ubuntu 26.04 hardware testing is separate.

Build the Cargo workspace with `./build.sh`, then run:

```
python3 package.py csr8510
python3 package.py tp-link-ub500
```

`PACKAGE_TARGET=mint22` is accepted only on Ubuntu 24.04 or Mint 22.x.
Install the matching package using `sudo apt install ./<package>.deb`.
Services are not automatically started or enabled. Choose the USB port:

```
sudo systemctl start csr8510@1-4.service
# or:
sudo systemctl start tp-link-ub500@1-4.service
```

Each standalone package installs in its own `/usr/libexec/<driver>/` directory.
Both may coexist with Magic Hotspot, whose integrated transport remains in its
own paths. Service conflicts prevent two transports taking the same USB port.
Stop returns only previously bound interfaces to btusb when possible. The runtime
ownership journal is shared per port; service conflicts serialize access.

The TP-Link package does not bundle firmware. Install the distro's Realtek
Bluetooth firmware (`linux-firmware` on Ubuntu/Mint, `firmware-realtek` from
Debian's non-free-firmware component). Firmware retains its upstream license.
