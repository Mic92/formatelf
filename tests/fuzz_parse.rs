//! Fuzz-style no-panic properties for the read path. `parse`, and the
//! `verify`/`finalize` round-trip on whatever parses, must never panic on
//! hostile or corrupted input -- only return `Err`. proptest reports and
//! shrinks any panic.

mod fixtures;

use fixtures::{sample, zig_available};
use proptest::prelude::*;
use proptest::test_runner::{Config, TestCaseError, TestRunner};

fn run<S: Strategy>(cases: u32, s: S, f: impl Fn(S::Value) -> Result<(), TestCaseError>) {
    TestRunner::new(Config::with_cases(cases))
        .run(&s, f)
        .unwrap();
}

/// Drive the full read path; a hostile input either fails to parse or survives
/// verification and re-serialization without panicking.
fn exercise(data: &[u8]) {
    if let Ok(mut img) = patchelf_rs::parser::parse(data) {
        let _ = patchelf_rs::verify::run(&img);
        let _ = patchelf_rs::layout::finalize(&mut img, data, None, false, false);
    }
}

#[test]
fn parse_never_panics_on_arbitrary_bytes() {
    run(512, prop::collection::vec(any::<u8>(), 0..2048), |bytes| {
        exercise(&bytes);
        Ok(())
    });
}

#[test]
fn parse_survives_corrupted_fixtures() {
    if !zig_available() {
        eprintln!("skipping: zig not on PATH");
        return;
    }
    // Structure-aware fuzzing: real headers reach far deeper code than random
    // bytes, while flips and truncation corrupt offsets, counts, and sizes.
    for name in ["exe-dyn-le", "exe-nopie-le", "so-soname-le", "obj-x86_64"] {
        let base = std::fs::read(sample(name)).unwrap();
        let len = base.len();
        let edits = prop::collection::vec((0..len, any::<u8>()), 0..32);
        run(256, (edits, 0..=len), |(edits, trunc)| {
            let mut data = base.clone();
            for (i, b) in edits {
                data[i] = b;
            }
            data.truncate(trunc);
            exercise(&data);
            Ok(())
        });
    }
}
