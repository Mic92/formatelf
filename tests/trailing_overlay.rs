//! Overlay bytes appended past everything the ELF references (self-extracting
//! archives, signing tools) must survive an unmutated parse/serialize round
//! trip untouched.

mod fixtures;

use formatelf::{parser, serialize};

#[test]
fn identity_preserves_trailing_overlay() {
    if !fixtures::zig_available() {
        return;
    }
    let mut data = std::fs::read(fixtures::sample("exe-dyn-le")).unwrap();
    data.extend_from_slice(b"TRAILING-OVERLAY-DATA-1234567890");

    let img = parser::parse(&data).unwrap();
    let out = serialize::write(&img, &data, data.len() as u64).unwrap();

    assert_eq!(out, data);
}
