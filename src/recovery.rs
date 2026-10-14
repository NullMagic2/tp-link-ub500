//! Module: magic_usb.recovery
//! Purpose: Bound endpoint stall recovery without replaying outgoing packets.
//! Created: 2026-10-13
//! Architecture: One budget per receiving endpoint, restored by incoming data.
#[derive(Default)]
pub struct Budget {
    failures: u8,
}
impl Budget {
    pub fn retry(&mut self) -> bool {
        if self.failures == 3 {
            return false;
        }
        self.failures += 1;
        true
    }
    pub fn data_received(&mut self) {
        self.failures = 0;
    }
}
#[cfg(test)]
#[path = "../tests/recovery/mod.rs"]
mod tests;
