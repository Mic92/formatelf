//! Fuzz the argument parser. parse() handles untrusted argv (including
//! numeric options and @file values) and must return Err, never panic.
#![no_main]

use arbitrary::Unstructured;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let mut u = Unstructured::new(data);
    let Ok(args) = u.arbitrary::<Vec<String>>() else {
        return;
    };
    // Drop the leading '@' so @file indirection never touches the filesystem;
    // this target fuzzes the parser, not file IO.
    let args = args
        .into_iter()
        .map(|a| std::ffi::OsString::from(a.trim_start_matches('@')));
    let _ = formatelf::cli::parse(args);
});
