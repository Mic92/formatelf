//! `--set-entry-stub`: the stub becomes `e_entry` in its own R+X segment and
//! chains to the program through the entry address saved in its header.

mod fixtures;

use fixtures::{assert_dynstr_synced, assert_runs, copy, ours, out, zig_available};
use formatelf::ir::{pf, pt};
use formatelf::{parser, stub};
use std::path::{Path, PathBuf};
use std::process::Command;

/// `x86_64` stub honouring the `--set-entry-stub` contract: 16-byte header whose
/// second word receives the original `e_entry`, code from offset 16. It finds
/// auxv, computes the load bias as `AT_PHDR` - `PT_PHDR.p_vaddr`, rewrites
/// `AT_ENTRY` to bias + saved entry (glibc's static-pie start reads it) and jumps
/// there with the initial stack untouched. Source: `tests/samples/entry_stub.S`.
const STUB_CODE: &[u8] = &[
    0x48, 0x89, 0xe6, 0x48, 0x8b, 0x0e, 0x48, 0x8d, 0x74, 0xce, 0x10, 0x48, 0x8b, 0x0e, 0x48, 0x83,
    0xc6, 0x08, 0x48, 0x85, 0xc9, 0x75, 0xf4, 0x45, 0x31, 0xc0, 0x45, 0x31, 0xc9, 0x48, 0x89, 0xf7,
    0x48, 0x8b, 0x0f, 0x48, 0x85, 0xc9, 0x74, 0x1a, 0x48, 0x83, 0xf9, 0x03, 0x75, 0x04, 0x4c, 0x8b,
    0x47, 0x08, 0x48, 0x83, 0xf9, 0x09, 0x75, 0x04, 0x4c, 0x8d, 0x4f, 0x08, 0x48, 0x83, 0xc7, 0x10,
    0xeb, 0xde, 0x4c, 0x89, 0xc7, 0x83, 0x3f, 0x06, 0x74, 0x06, 0x48, 0x83, 0xc7, 0x38, 0xeb, 0xf5,
    0x4c, 0x89, 0xc0, 0x48, 0x2b, 0x47, 0x10, 0x48, 0x03, 0x05, 0x9a, 0xff, 0xff, 0xff, 0x4d, 0x85,
    0xc9, 0x74, 0x03, 0x49, 0x89, 0x01, 0x31, 0xd2, 0xff, 0xe0,
];

fn stub_file() -> PathBuf {
    let mut blob = b"TESTSTUB".to_vec();
    blob.extend_from_slice(&[0; 8]);
    blob.extend_from_slice(STUB_CODE);
    let p = Path::new(env!("CARGO_TARGET_TMPDIR")).join("entry_stub.bin");
    std::fs::write(&p, blob).unwrap();
    p
}

fn patch(file: &Path, args: &[&str]) {
    let st = Command::new(ours()).args(args).arg(file).status().unwrap();
    assert!(st.success(), "patch {args:?} failed");
}

/// entry lies 16 bytes into the one executable `PT_LOAD` that maps the stub
/// section, no segment is W+X, and the header names the original entry.
fn assert_stub_layout(file: &Path, original_entry: u64) {
    let data = std::fs::read(file).unwrap();
    let img = parser::parse(&data).unwrap();
    let idx = img.find_section(stub::SECTION).expect("stub section");
    let s = &img.shdrs[idx];
    assert_eq!(img.ehdr.entry, s.addr + stub::CODE_OFFSET);
    let covering: Vec<_> = img
        .phdrs
        .iter()
        .filter(|p| p.p_type == pt::LOAD && p.vaddr <= s.addr && s.addr < p.vaddr + p.memsz)
        .collect();
    assert_eq!(covering.len(), 1);
    assert_eq!(covering[0].flags, pf::R | pf::X);
    assert!(
        img.phdrs
            .iter()
            .all(|p| p.flags & (pf::W | pf::X) != (pf::W | pf::X))
    );
    let saved = u64::from_le_bytes(img.section_data[idx][8..16].try_into().unwrap());
    assert_eq!(saved, original_entry);
}

fn original_entry(file: &Path) -> u64 {
    parser::parse(&std::fs::read(file).unwrap())
        .unwrap()
        .ehdr
        .entry
}

fn check(fixture: &str) {
    if !zig_available() {
        return;
    }
    let stub = stub_file();
    let stub = stub.to_str().unwrap();
    let bin = copy(fixture, "stub");
    let entry = original_entry(&bin);
    let interp = out(ours(), "--print-interpreter", &bin);

    patch(&bin, &["--set-entry-stub", stub]);
    assert_stub_layout(&bin, entry);
    assert_eq!(out(ours(), "--print-interpreter", &bin), interp);
    assert_runs(&bin);

    // again, together with a growing op: one stub, still chaining to the program
    let rpath = "/a/runpath/long/enough/to/force/the/string/table/to/move/lib";
    patch(&bin, &["--set-rpath", rpath, "--set-entry-stub", stub]);
    assert_stub_layout(&bin, entry);
    assert_eq!(out(ours(), "--print-rpath", &bin).trim(), rpath);
    assert_dynstr_synced(&bin);
    assert_runs(&bin);
}

#[test]
fn pie_executable() {
    check("exe-dyn-le");
}

#[test]
fn non_pie_executable() {
    check("exe-nopie-le");
}

#[test]
fn bun_template_prepend_layout() {
    check("exe-bun-le");
}

#[test]
fn keeps_overlay_at_the_file_end() {
    if !zig_available() {
        return;
    }
    let overlay = b"SFX-ARCHIVE-PAYLOAD";
    let bin = copy("exe-nopie-le", "stub-overlay");
    let mut data = std::fs::read(&bin).unwrap();
    data.extend_from_slice(overlay);
    std::fs::write(&bin, &data).unwrap();
    let entry = original_entry(&bin);

    patch(&bin, &["--set-entry-stub", stub_file().to_str().unwrap()]);
    assert!(std::fs::read(&bin).unwrap().ends_with(overlay));
    assert_stub_layout(&bin, entry);
    assert_runs(&bin);
}

#[test]
fn rejects_short_stub() {
    if !zig_available() {
        return;
    }
    let bin = copy("exe-dyn-le", "stub-short");
    let short = Path::new(env!("CARGO_TARGET_TMPDIR")).join("short_stub.bin");
    std::fs::write(&short, [0u8; 16]).unwrap();
    let st = Command::new(ours())
        .args(["--set-entry-stub", short.to_str().unwrap()])
        .arg(&bin)
        .status()
        .unwrap();
    assert!(!st.success());
}
