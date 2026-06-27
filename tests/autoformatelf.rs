//! End-to-end check of the multi-call `auto-formatelf` personality: it must
//! resolve `DT_NEEDED` entries against the provided library dirs and set both
//! the interpreter and RUNPATH.

mod fixtures;

use fixtures::{ours, out, sample, zig_available};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;

#[test]
fn auto_formatelf_sets_interpreter_and_rpath() {
    if !zig_available() {
        return;
    }
    let exe = sample("exe-dyn-le");
    let lib = sample("so-soname-le"); // a matching x86_64 shared object

    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join("autofe");
    let bin = root.join("bin");
    let libdir = root.join("lib");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::create_dir_all(&libdir).unwrap();

    let app = bin.join("app");
    std::fs::copy(&exe, &app).unwrap();
    // Stand in for every library the executable needs, under the right soname.
    for soname in ["libc.so.6", "ld-linux-x86-64.so.2", "libpthread.so.0"] {
        std::fs::copy(&lib, libdir.join(soname)).unwrap();
    }

    let status = Command::new(ours())
        .arg0("auto-formatelf")
        .arg("--interpreter")
        .arg(&exe)
        // Point libc at an empty dir so the staged sonames are not treated as
        // libc and dropped (the ambient $NIX_BINTOOLS would otherwise apply).
        .arg("--libc")
        .arg(&root)
        .arg("--paths")
        .arg(&bin)
        .arg("--libs")
        .arg(&libdir)
        .status()
        .unwrap();
    assert!(status.success(), "auto-formatelf failed");

    let rpath = out(ours(), "--print-rpath", &app);
    assert!(
        rpath.trim() == libdir.to_str().unwrap(),
        "rpath {rpath:?} should be the lib dir"
    );
    let interp = out(ours(), "--print-interpreter", &app);
    assert_eq!(interp.trim(), exe.to_str().unwrap());
}

#[test]
fn auto_formatelf_reports_unresolved_dependencies() {
    if !zig_available() {
        return;
    }
    let exe = sample("exe-dyn-le");
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join("autofe-missing");
    let bin = root.join("bin");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::copy(&exe, bin.join("app")).unwrap();

    // No --libs and an empty libc dir, so the deps cannot be found: must fail.
    let status = Command::new(ours())
        .arg0("auto-formatelf")
        .arg("--interpreter")
        .arg(&exe)
        .arg("--libc")
        .arg(&root)
        .arg("--paths")
        .arg(&bin)
        .status()
        .unwrap();
    assert!(!status.success(), "missing deps must fail the run");

    // ...unless they are explicitly ignored.
    let ignored = Command::new(ours())
        .arg0("auto-formatelf")
        .arg("--interpreter")
        .arg(&exe)
        .arg("--libc")
        .arg(&root)
        .arg("--ignore-missing")
        .arg("*")
        .arg("--paths")
        .arg(&bin)
        .status()
        .unwrap();
    assert!(ignored.success(), "ignored deps must succeed");
}
