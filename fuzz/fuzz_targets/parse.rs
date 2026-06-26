//! Coverage-guided fuzzing of the read path: parse() consumes untrusted bytes
//! and must fail gracefully, never panic. Whatever parses is run through
//! verify and finalize to catch panics on the write path too.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(mut img) = patchelf_rs::parser::parse(data) {
        let _ = patchelf_rs::verify::run(&img);
        let _ = patchelf_rs::layout::finalize(&mut img, data, None, false, false);
    }
});
