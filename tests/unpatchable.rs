//! Binaries that carry no `.dynamic` (a section-less sstripped exe, and a
//! statically linked Free Pascal exe from its own linker) must still parse and
//! verify from the headers alone, and mutation must fail gracefully rather than
//! panic — reference patchelf likewise refuses them. Fixtures come from the
//! `toolchains` shell as `TC_STRIPPED` / `TC_PASCAL`; skip when unset.

mod fixtures;

use fixtures::ours;
use std::process::Command;

/// Parse + verify must succeed, and `--set-rpath` must fail cleanly (non-zero
/// exit, no crash) because there is no `.dynamic`/`.dynstr` to edit.
fn assert_unpatchable(var: &str, section_less: bool) {
    let Ok(path) = std::env::var(var) else {
        return;
    };
    let bytes = std::fs::read(&path).unwrap();
    let img = formatelf::parser::parse(&bytes).expect("parse");
    if section_less {
        assert!(img.shdrs.is_empty(), "expected no section headers");
    }
    formatelf::verify::run(&img, bytes.len() as u64).expect("verify");

    let st = Command::new(ours())
        .args(["--set-rpath", "/opt/x"])
        .arg(&path)
        .status()
        .unwrap();
    assert!(!st.success(), "{var}: mutation should fail, not crash");
}

#[test]
fn stripped_section_less() {
    assert_unpatchable("TC_STRIPPED", true);
}

#[test]
fn static_pascal() {
    assert_unpatchable("TC_PASCAL", false);
}
