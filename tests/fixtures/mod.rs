//! On-demand ELF fixtures. Rather than commit binaries, we compile a tiny C
//! source with `zig cc` for several arch/endian/class combinations the first
//! time the fixtures are requested. Requires `zig` on PATH (provided by the
//! dev shell); tests skip themselves when it is absent.
#![allow(dead_code)] // each test binary uses only part of this shared module

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// (file name, zig target triple, extra zig cc flags) covering the codec matrix:
/// 64/32-bit and little/big-endian, plus a relocatable object.
const SPECS: &[(&str, &str, &[&str])] = &[
    ("exe-x86_64-le", "x86_64-linux-musl", &[]),
    ("exe-x86-le", "x86-linux-musl", &[]),
    ("so-aarch64-le", "aarch64-linux-musl", &["-shared"]),
    ("so-ppc64-be", "powerpc64-linux-musl", &["-shared"]),
    ("so-ppc-be", "powerpc-linux-musleabi", &["-shared"]),
    ("obj-x86_64", "x86_64-linux-musl", &["-c"]),
    // Dynamically linked: interpreter, DT_NEEDED, DT_RUNPATH.
    (
        "exe-dyn-le",
        "x86_64-linux-gnu",
        &["-fPIE", "-pie", "-Wl,-rpath,/opt/custom/lib"],
    ),
    // Non-PIE (ET_EXEC) executable, for the exec growth path.
    (
        "exe-nopie-le",
        "x86_64-linux-gnu",
        &["-no-pie", "-fno-pie", "-Wl,-rpath,/opt/custom/lib"],
    ),
    // Shared object carrying a DT_SONAME.
    (
        "so-soname-le",
        "x86_64-linux-gnu",
        &["-shared", "-Wl,-soname,libsample.so.1"],
    ),
    // MIPS PIE: exercises DT_MIPS_RLD_MAP_REL and the PT_MIPS_ABIFLAGS segment.
    ("exe-mips-be", "mips-linux-gnueabi", &["-fPIE", "-pie"]),
];

/// The formatelf binary under test.
pub fn ours() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_formatelf"))
}

/// Run a read-only op on `file` and return its stdout, asserting success.
pub fn out(bin: &Path, op: &str, file: &Path) -> String {
    let o = Command::new(bin).arg(op).arg(file).output().unwrap();
    assert!(o.status.success(), "{op} on {file:?} failed");
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// Path to the named fixture.
pub fn sample(name: &str) -> PathBuf {
    samples()
        .into_iter()
        .find(|p| p.file_name().unwrap() == name)
        .unwrap()
}

/// A writable copy of a fixture, named with `suffix` to keep cases isolated.
pub fn copy(name: &str, suffix: &str) -> PathBuf {
    let dst = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("{name}.{suffix}"));
    std::fs::copy(sample(name), &dst).unwrap();
    dst
}

/// Apply our formatelf to `file`, asserting success.
pub fn patch(file: &Path, args: &[&str]) {
    let st = Command::new(ours()).args(args).arg(file).status().unwrap();
    assert!(st.success(), "patch {args:?} failed");
}

/// The reference patchelf, but only if both it and zig are present; otherwise
/// the caller skips. Differential tests gate on this.
pub fn guard() -> Option<PathBuf> {
    if !zig_available() {
        eprintln!("skipping: zig not on PATH");
        return None;
    }
    let r = c_patchelf();
    if r.is_none() {
        eprintln!("skipping: reference patchelf not built");
    }
    r
}

/// The loader resolves dynamic strings via DT_STRTAB's address, not the section
/// header, so after a relayout DT_STRTAB must point at the relocated .dynstr.
pub fn assert_dynstr_synced(file: &Path) {
    use formatelf::ir::dt;
    let buf = std::fs::read(file).unwrap();
    let img = formatelf::parser::parse(&buf).unwrap();
    let strtab = img
        .dynamic
        .iter()
        .find(|d| d.tag == dt::STRTAB)
        .unwrap()
        .val;
    let dynstr = img.shdrs[img.find_section(".dynstr").unwrap()].addr;
    assert_eq!(strtab, dynstr, "DT_STRTAB not synced to moved .dynstr");
}

/// Path to the reference C patchelf, if it has been built. PATCHELF_REFERENCE
/// overrides the default relative path so tools that build in a copy of the
/// tree (e.g. cargo-mutants) can still locate it.
pub fn c_patchelf() -> Option<PathBuf> {
    let p = match std::env::var_os("PATCHELF_REFERENCE") {
        Some(p) => PathBuf::from(p),
        None => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../patchelf/src/patchelf"),
    };
    p.exists().then_some(p)
}

pub fn zig_available() -> bool {
    tool_available("zig", &["version"])
}

/// True if `cmd args...` runs and exits successfully.
pub fn tool_available(cmd: &str, args: &[&str]) -> bool {
    Command::new(cmd)
        .args(args)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn dir() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("elf-fixtures");
        let src = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("sample.c");
        std::fs::create_dir_all(&d).unwrap();
        let cache = d.join(".zig-cache");
        for (name, target, extra) in SPECS {
            let out = d.join(name);
            if out.exists() {
                continue;
            }
            let status = Command::new("zig")
                .args(["cc", "-target", target])
                .args(*extra)
                .arg(&src)
                .arg("-o")
                .arg(&out)
                .env("ZIG_GLOBAL_CACHE_DIR", &cache)
                .status()
                .expect("run zig cc");
            assert!(status.success(), "zig cc failed for {name} ({target})");
        }
        d
    })
}

/// Paths to all generated fixtures.
pub fn samples() -> Vec<PathBuf> {
    SPECS.iter().map(|(name, ..)| dir().join(name)).collect()
}
