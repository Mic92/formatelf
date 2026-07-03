//! Coverage-guided fuzzing of the read path: parse() consumes untrusted bytes
//! and must fail gracefully, never panic. Whatever parses is run through
//! verify and finalize to catch panics on the write path too.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(mut img) = formatelf::parser::parse(data) {
        let _ = formatelf::verify::run(&img, data.len() as u64);
        let _ = formatelf::layout::finalize(&mut img, data, None, false, false, &mut Vec::new());
    }
});
