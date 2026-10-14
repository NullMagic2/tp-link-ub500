# Licensing scope and provenance

The project owner authorizes MIT licensing for this release of the authored
Rust transport, adapter clients, tests and packaging. Earlier published GPL
releases remain available under their original terms.

The review found no bundled Linux or BlueZ implementation files. `realtek.rs`
uses Linux btrtl.c as a protocol reference for firmware signatures, field
layouts and HCI command values. The Rust implementation uses its own checked
parsing, bounded buffers and control flow. That reference is retained for
attribution; it is not a license grant for Linux source. No Linux source or
Realtek firmware is distributed with these driver packages.

External libraries retain their licenses: libusb is LGPL-2.1-or-later and
libzstd offers BSD/GPL terms. Packages dynamically use distro libraries rather
than vendoring them. Distro firmware retains its supplier's license.

Magic Hotspot's separate vendored usbmuxd engine is outside this MIT scope and
retains upstream GPL-2.0-or-3.0 terms. This review records the source and
provenance evidence available in this project; it does not relicense any
third-party component.
