//! Overlay bytes past everything the ELF describes are found by seeking back
//! from EOF, so a relayout must keep them last.

mod fixtures;

use fixtures::{assert_runs, copy, ours, out, patch, zig_available};
use formatelf::parser;
use std::fs;

/// A long rpath grows `.dynstr`, which is what forces a relayout.
fn long_rpath() -> String {
    "/nix/store/".to_string() + &"r".repeat(200) + "/lib"
}

#[test]
fn an_overlay_stays_at_the_file_end() {
    if !zig_available() {
        return;
    }
    let overlay = b"SFX-ARCHIVE-PAYLOAD-FOUND-BY-SEEKING-BACK-FROM-EOF";
    let file = copy("exe-nopie-le", "overlay");
    let mut data = fs::read(&file).unwrap();
    data.extend_from_slice(overlay);
    fs::write(&file, &data).unwrap();
    let before = data.len();

    let rpath = long_rpath();
    patch(&file, &["--set-rpath", &rpath]);

    let data = fs::read(&file).unwrap();
    assert!(
        data.ends_with(overlay),
        "the overlay is no longer the last thing in the file"
    );
    assert!(
        data.len() > before,
        "nothing was relaid out, so the test proves nothing"
    );
    assert_eq!(out(ours(), "--print-rpath", &file).trim(), rpath);
    assert_runs(&file);
}

/// Without an overlay the cheaper append path is still taken.
#[test]
fn a_file_without_an_overlay_still_appends() {
    if !zig_available() {
        return;
    }
    let file = copy("exe-nopie-le", "no-overlay");
    let before = fs::metadata(&file).unwrap().len();

    patch(&file, &["--set-rpath", &long_rpath()]);

    let data = fs::read(&file).unwrap();
    let img = parser::parse(&data).unwrap();
    assert!(
        img.ehdr.shoff >= before,
        "section header table at {:#x} landed below the original end {before:#x}, \
         so the append path was not taken",
        img.ehdr.shoff
    );
    assert_runs(&file);
}
