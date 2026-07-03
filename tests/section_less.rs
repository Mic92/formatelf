//! A section-less binary (sstrip drops the section header table) must parse and
//! verify from program headers alone, and mutation must fail gracefully rather
//! than panic — reference patchelf also rejects it ("no section headers"). The
//! fixture comes from the `toolchains` shell as `TC_STRIPPED`; skips when unset.

mod fixtures;

use fixtures::ours;
use std::process::Command;

#[test]
fn stripped_parses_verifies_and_rejects_mutation() {
    let Ok(path) = std::env::var("TC_STRIPPED") else {
        return;
    };
    let bytes = std::fs::read(&path).unwrap();
    let img = formatelf::parser::parse(&bytes).expect("section-less parse");
    assert!(img.shdrs.is_empty(), "fixture still has section headers");
    formatelf::verify::run(&img, bytes.len() as u64).expect("section-less verify");

    // Mutation needs .dynstr, which lives in a missing section, so it must fail
    // with a non-zero exit, not crash.
    let st = Command::new(ours())
        .args(["--set-rpath", "/opt/x"])
        .arg(&path)
        .status()
        .unwrap();
    assert!(
        !st.success(),
        "mutation of a section-less binary should fail"
    );
}
