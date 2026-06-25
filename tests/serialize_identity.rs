//! Parsing then serializing without mutation must reproduce the input file
//! byte-for-byte, validating the canvas serializer against the full layout.

mod fixtures;

use patchelf_rs::{parser, serialize};

#[test]
fn identity_roundtrip() {
    if !fixtures::zig_available() {
        eprintln!("skipping: zig not on PATH");
        return;
    }
    for path in fixtures::samples() {
        let data = std::fs::read(&path).unwrap();
        let img = parser::parse(&data).unwrap_or_else(|e| panic!("parse {path:?}: {e}"));
        let out =
            serialize::write(&img, data.clone()).unwrap_or_else(|e| panic!("write {path:?}: {e}"));
        assert_eq!(out, data, "identity mismatch for {path:?}");
    }
}
