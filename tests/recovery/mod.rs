//! Module: magic_usb.recovery.tests
//! Purpose: Verify silent endpoints cannot replenish a failed receive budget.
//! Created: 2026-10-13
//! Architecture: Pure retry state.
use super::*;
#[test]
fn recovery_is_bounded_until_actual_data_arrives() {
    let mut budget = Budget::default();
    for _ in 0..3 {
        assert!(budget.retry());
    }
    for _ in 0..20 {
        assert!(!budget.retry());
    }
    budget.data_received();
    assert!(budget.retry());
}
