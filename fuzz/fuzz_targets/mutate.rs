//! Coverage-guided fuzzing of the write path. Apply an arbitrary sequence of
//! operations to a real base binary, then serialize, re-parse, and verify. The
//! ops, layout engine, and serializer must never panic, and our own output
//! must always parse back.
#![no_main]

use std::collections::BTreeMap;
use std::sync::OnceLock;

use arbitrary::Unstructured;
use libfuzzer_sys::fuzz_target;
use patchelf_rs::cli::Operation;
use patchelf_rs::ops::{Modifiers, Report};

/// Drop the section-header table (zeroing e_shoff/e_shnum/e_shstrndx) so the
/// parser must recover .dynstr/.interp from segments, exercising the
/// stripped-binary paths the section-bearing fixtures never reach.
fn strip_sections(mut data: Vec<u8>) -> Vec<u8> {
    if data.len() >= 64 && data[..4] == *b"\x7fELF" && data[4] == 2 {
        data[40..48].fill(0); // e_shoff
        data[60..64].fill(0); // e_shnum, e_shstrndx
    }
    data
}

/// Real binaries to mutate, loaded once. The test fixtures live in the main
/// crate's target dir; PATCHELF_FUZZ_FIXTURES overrides the location.
fn bases() -> &'static [Vec<u8>] {
    static BASES: OnceLock<Vec<Vec<u8>>> = OnceLock::new();
    BASES.get_or_init(|| {
        let dir = std::env::var("PATCHELF_FUZZ_FIXTURES").unwrap_or_else(|_| {
            format!("{}/../target/tmp/elf-fixtures", env!("CARGO_MANIFEST_DIR"))
        });
        let files: Vec<Vec<u8>> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| std::fs::read(e.path()).ok())
            .collect();
        let stripped = files.iter().cloned().map(strip_sections);
        files.iter().cloned().chain(stripped).collect()
    })
}

fn arb_op(u: &mut Unstructured) -> arbitrary::Result<Operation> {
    // BuildResolutionCache and RenameDynamicSymbols read the filesystem, so
    // they are out of scope for in-process fuzzing.
    Ok(match u.int_in_range(0u8..=18)? {
        0 => Operation::SetInterpreter(u.arbitrary()?),
        1 => Operation::SetOsAbi(u.arbitrary()?),
        2 => Operation::SetSoname(u.arbitrary()?),
        3 => Operation::SetRpath(u.arbitrary()?),
        4 => Operation::AddRpath(u.arbitrary()?),
        5 => Operation::RemoveRpath,
        6 => Operation::ShrinkRpath,
        7 => Operation::AllowedRpathPrefixes(u.arbitrary()?),
        8 => Operation::ForceRpath,
        9 => Operation::AddNeeded(u.arbitrary()?),
        10 => Operation::RemoveNeeded(u.arbitrary()?),
        11 => Operation::ReplaceNeeded {
            old: u.arbitrary()?,
            new: u.arbitrary()?,
        },
        12 => Operation::NoDefaultLib,
        13 => Operation::ClearSymbolVersion(u.arbitrary()?),
        14 => Operation::AddDebugTag,
        15 => Operation::ClearExecstack,
        16 => Operation::SetExecstack,
        17 => Operation::PrintRpath,
        _ => Operation::PrintNeeded,
    })
}

fuzz_target!(|data: &[u8]| {
    let bases = bases();
    if bases.is_empty() {
        return; // fixtures not built; nothing to mutate
    }
    let mut u = Unstructured::new(data);
    let Ok(base) = u.choose(bases) else { return };
    let Ok(mut img) = patchelf_rs::parser::parse(base) else {
        return;
    };

    let mods = Modifiers {
        force_rpath: u.arbitrary().unwrap_or(false),
        allowed_rpath_prefixes: u.arbitrary().unwrap_or_default(),
        debug: false,
    };
    let mut report = Report { lines: Vec::new() };
    for _ in 0..8 {
        let Ok(op) = arb_op(&mut u) else { break };
        let _ = patchelf_rs::ops::apply(&mut img, &op, &mods, &mut report);
    }

    // Symbol renaming takes a map directly, bypassing the CLI's file read.
    if let Ok(pairs) = u.arbitrary::<Vec<(String, String)>>() {
        let map: BTreeMap<_, _> = pairs.into_iter().collect();
        let _ = patchelf_rs::symbols::rename_dynamic_symbols(&mut img, &map);
    }

    if let Ok(bytes) = patchelf_rs::layout::finalize(&mut img, base, None, false, false) {
        // Whatever we emit must round-trip and hold our invariants.
        let out = patchelf_rs::parser::parse(&bytes).expect("our output must re-parse");
        let _ = patchelf_rs::verify::run(&out);
    }
});
