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
    if let Ok(mut img) = formatelf::parser::parse(data) {
        let _ = formatelf::verify::run(&img);
        let _ = formatelf::layout::finalize(&mut img, data, None, false, false);
    }
}

/// Regression: an extended section count (section 0's sh_size under the
/// SHN_XINDEX escape) is attacker-controlled and was fed straight into
/// Vec::with_capacity, aborting on a multi-gigabyte allocation. A count that
/// cannot fit in the file must be rejected, not allocated.
#[test]
fn huge_section_count_is_rejected() {
    let mut data = vec![0u8; 128];
    data[..4].copy_from_slice(&[0x7f, b'E', b'L', b'F']);
    data[4] = 2; // ELFCLASS64
    data[5] = 1; // ELFDATA2LSB
    data[6] = 1; // EV_CURRENT
    let put16 = |d: &mut [u8], at: usize, v: u16| d[at..at + 2].copy_from_slice(&v.to_le_bytes());
    let put64 = |d: &mut [u8], at: usize, v: u64| d[at..at + 8].copy_from_slice(&v.to_le_bytes());
    put16(&mut data, 16, 2); // e_type = ET_EXEC
    put16(&mut data, 18, 62); // e_machine = x86_64
    put64(&mut data, 40, 64); // e_shoff -> section 0 at byte 64
    put16(&mut data, 58, 64); // e_shentsize
    put16(&mut data, 60, 0); // e_shnum = 0 triggers the escape
    put64(&mut data, 64 + 32, u64::MAX); // section 0 sh_size = absurd count

    assert!(formatelf::parser::parse(&data).is_err());
}

/// Regression: verify computed PT_LOAD memory ranges as `vaddr + memsz`, which
/// overflowed (debug panic / release wraparound) on a segment with an absurd
/// address. Verification runs on untrusted parsed data and must stay panic-free.
#[test]
fn verify_does_not_overflow_on_huge_segment() {
    let mut data = vec![0u8; 120];
    data[..4].copy_from_slice(&[0x7f, b'E', b'L', b'F']);
    data[4] = 2; // ELFCLASS64
    data[5] = 1; // ELFDATA2LSB
    data[6] = 1; // EV_CURRENT
    let put16 = |d: &mut [u8], at: usize, v: u16| d[at..at + 2].copy_from_slice(&v.to_le_bytes());
    let put32 = |d: &mut [u8], at: usize, v: u32| d[at..at + 4].copy_from_slice(&v.to_le_bytes());
    let put64 = |d: &mut [u8], at: usize, v: u64| d[at..at + 8].copy_from_slice(&v.to_le_bytes());
    put16(&mut data, 16, 2); // e_type = ET_EXEC
    put16(&mut data, 18, 62); // e_machine = x86_64
    put64(&mut data, 32, 64); // e_phoff -> phdr at byte 64
    put16(&mut data, 54, 56); // e_phentsize
    put16(&mut data, 56, 1); // e_phnum = 1
    put32(&mut data, 64, 1); // p_type = PT_LOAD
    put32(&mut data, 68, 4); // p_flags = PF_R
    put64(&mut data, 64 + 16, u64::MAX - 0xff); // p_vaddr
    put64(&mut data, 64 + 40, 0x1000); // p_memsz -> vaddr + memsz overflows

    let img = formatelf::parser::parse(&data).expect("header parses");
    let _ = formatelf::verify::run(&img); // must return, not panic
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
