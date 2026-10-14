//! Module: csr8510::test_support
//! Purpose: Serialize tests that change the process-wide stop flag.
//! Created: 2026-10-11
//! Architecture: One shared test-only mutex; no runtime transport state.

pub static STOP_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
