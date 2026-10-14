//! Module: magic_usb.ownership.tests
//! Purpose: Verify conditional restoration and prevent binding a replacement adapter.
//! Created: 2026-10-13
//! Architecture: Temporary sysfs/runtime fixtures and a fake binding callback.
use super::*;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::sync::atomic::{AtomicUsize, Ordering};
struct Fixture {
    root: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "magic-usb-ownership-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join("1-4")).unwrap();
        for (field, value) in [
            ("idVendor", "2357"),
            ("idProduct", "0604"),
            ("busnum", "1"),
            ("devnum", "114"),
            ("bConfigurationValue", "1"),
        ] {
            fs::write(root.join("1-4").join(field), value).unwrap();
        }
        for index in [0, 1] {
            fs::create_dir_all(root.join(format!("1-4:1.{index}"))).unwrap();
        }
        fs::create_dir(root.join("btusb")).unwrap();
        fs::write(root.join("bind"), "").unwrap();
        Self { root }
    }
    fn owner(&self, index: u8, name: &str) {
        fs::create_dir_all(self.root.join(name)).unwrap();
        symlink(
            self.root.join(name),
            self.root.join(format!("1-4:1.{index}/driver")),
        )
        .unwrap();
    }
    fn journal(&self) -> PathBuf {
        self.root.join("ownership")
    }
    fn restore(&self) -> Vec<String> {
        let mut calls = Vec::new();
        restore_in(
            &self.root,
            "1-4",
            &self.journal(),
            &self.root.join("bind"),
            |name| {
                calls.push(name.into());
                Ok(())
            },
        )
        .unwrap();
        calls
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
#[test]
fn previously_inactive_btusb_is_never_bound() {
    let f = Fixture::new();
    let state = Ownership::capture(&f.root, "1-4").unwrap();
    assert_eq!(state.interfaces, 0);
    state.save(&f.journal()).unwrap();
    assert!(f.restore().is_empty());
    assert!(!f.journal().exists());
    assert!(f.restore().is_empty());
}
#[test]
fn active_btusb_bindings_are_recorded_and_only_those_are_restored() {
    for mask in [1, 2, 3] {
        let f = Fixture::new();
        for index in [0, 1] {
            if mask & (1 << index) != 0 {
                f.owner(index, "btusb");
            }
        }
        let state = Ownership::capture(&f.root, "1-4").unwrap();
        assert_eq!(state.interfaces, mask);
        state.save(&f.journal()).unwrap();
        assert_eq!(
            fs::metadata(f.journal()).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(state.save(&f.journal()).is_err());
        for index in [0, 1] {
            let _ = fs::remove_file(f.root.join(format!("1-4:1.{index}/driver")));
        }
        let expected = (0..2)
            .filter(|i| mask & (1 << i) != 0)
            .map(|i| format!("1-4:1.{i}"))
            .collect::<Vec<_>>();
        assert_eq!(f.restore(), expected);
    }
}
#[test]
fn removal_replacement_unloaded_module_and_new_owner_are_left_alone() {
    for changed in ["devnum", "idProduct", "module", "owner", "removed"] {
        let f = Fixture::new();
        f.owner(0, "btusb");
        let state = Ownership::capture(&f.root, "1-4").unwrap();
        state.save(&f.journal()).unwrap();
        fs::remove_file(f.root.join("1-4:1.0/driver")).unwrap();
        match changed {
            "devnum" => fs::write(f.root.join("1-4/devnum"), "115").unwrap(),
            "idProduct" => fs::write(f.root.join("1-4/idProduct"), "0001").unwrap(),
            "module" => fs::remove_file(f.root.join("bind")).unwrap(),
            "owner" => f.owner(0, "another-driver"),
            _ => fs::remove_dir_all(f.root.join("1-4")).unwrap(),
        }
        assert!(f.restore().is_empty(), "{changed}");
    }
}
#[test]
fn invalid_state_and_symlinks_cannot_cause_binding() {
    let f = Fixture::new();
    for text in [
        "",
        "1 9047 1540 1 114 1 7",
        "2 9047 1540 1 114 1 3",
        "1 1452 4776 1 114 1 3",
        &"x".repeat(300),
    ] {
        fs::write(f.journal(), text).unwrap();
        assert!(
            restore_in(
                &f.root,
                "1-4",
                &f.journal(),
                &f.root.join("bind"),
                |_| panic!("must not bind")
            )
            .is_err()
        );
    }
    fs::remove_file(f.journal()).unwrap();
    symlink(f.root.join("bind"), f.journal()).unwrap();
    assert!(
        restore_in(
            &f.root,
            "1-4",
            &f.journal(),
            &f.root.join("bind"),
            |_| panic!("must not bind")
        )
        .is_err()
    );
    for port in ["", "..", "1-", "1-4/../../", "0-4", "1-4..2"] {
        assert!(!valid_port(port));
    }
    assert!(valid_port("1-4.2"));
}
#[test]
fn failed_restore_retains_state_for_a_later_retry() {
    let f = Fixture::new();
    f.owner(0, "btusb");
    let state = Ownership::capture(&f.root, "1-4").unwrap();
    state.save(&f.journal()).unwrap();
    fs::remove_file(f.root.join("1-4:1.0/driver")).unwrap();
    assert!(
        restore_in(&f.root, "1-4", &f.journal(), &f.root.join("bind"), |_| Err(
            "busy".into()
        ))
        .is_err()
    );
    assert!(f.journal().exists());
    assert_eq!(f.restore(), ["1-4:1.0"]);
}
