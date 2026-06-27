//! set-rpath benchmark: parse, rewrite `DT_RPATH`, serialize. Needs `zig` on
//! PATH (the dev shell provides it) to build the fixture.

use std::hint::black_box;

#[path = "../tests/fixtures/mod.rs"]
mod fixtures;

fn fixture_bytes() -> Vec<u8> {
    assert!(fixtures::zig_available(), "set_rpath bench needs `zig`");
    std::fs::read(fixtures::sample("exe-dyn-le")).unwrap()
}

// The fixture's DT_RPATH is "/opt/custom/lib": a shorter value reuses the slot,
// a longer one forces a relayout.
const IN_PLACE: &str = "/opt/custom/x";
const GROW: &str = "/opt/some/much/longer/replacement/rpath/dir";

/// Run a single op through the full parse -> apply -> serialize pipeline.
fn run(data: &[u8], op: &formatelf::cli::Operation) {
    let mods = formatelf::ops::Modifiers::default();
    let mut image = formatelf::parser::parse(data).unwrap();
    let mut report = formatelf::ops::Report::default();
    formatelf::ops::apply(&mut image, op, &mods, &mut report).unwrap();
    let mut out = Vec::new();
    formatelf::layout::finalize(&mut image, data, None, false, false, &mut out).unwrap();
    black_box(out);
}

#[divan::bench(args = [IN_PLACE, GROW])]
fn set_rpath(bencher: divan::Bencher, rpath: &str) {
    let data = fixture_bytes();
    bencher.bench_local(|| run(&data, &formatelf::cli::Operation::SetRpath(rpath.into())));
}

#[divan::bench]
fn add_needed(bencher: divan::Bencher) {
    let data = fixture_bytes();
    bencher.bench_local(|| {
        run(
            &data,
            &formatelf::cli::Operation::AddNeeded("libextra.so".into()),
        );
    });
}

#[divan::bench]
fn set_soname(bencher: divan::Bencher) {
    let data = fixture_bytes();
    bencher.bench_local(|| {
        run(
            &data,
            &formatelf::cli::Operation::SetSoname("libfoo.so.1".into()),
        );
    });
}

#[divan::bench]
fn set_interpreter(bencher: divan::Bencher) {
    let data = fixture_bytes();
    bencher.bench_local(|| {
        run(
            &data,
            &formatelf::cli::Operation::SetInterpreter("/lib/ld.so".into()),
        );
    });
}

/// Isolates the shared parse + serialize floor every mutating op pays.
#[divan::bench]
fn parse_only(bencher: divan::Bencher) {
    let data = fixture_bytes();
    bencher.bench_local(|| black_box(formatelf::parser::parse(&data).unwrap()));
}

fn main() {
    divan::main();
}
