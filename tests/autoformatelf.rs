//! End-to-end check of the multi-call `auto-formatelf` personality: it must
//! resolve `DT_NEEDED` entries against the provided library dirs and set both
//! the interpreter and RUNPATH.

mod fixtures;

use fixtures::{ours, out, sample, zig_available};
use std::fs;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;

fn stage_dependencies(lib: &Path, dir: &Path) {
    fs::create_dir_all(dir).unwrap();
    for soname in ["libc.so.6", "ld-linux-x86-64.so.2", "libpthread.so.0"] {
        fs::copy(lib, dir.join(soname)).unwrap();
    }
}

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
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&bin).unwrap();

    let app = bin.join("app");
    fs::copy(&exe, &app).unwrap();
    stage_dependencies(&lib, &libdir);

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
fn auto_formatelf_resolves_duplicate_libraries_in_search_order() {
    if !zig_available() {
        return;
    }
    let exe = sample("exe-dyn-le");
    let lib = sample("so-soname-le");
    let root = tempfile::tempdir().unwrap();
    let bin = root.path().join("bin");
    let bundled = root.path().join("bundled");
    let first = bundled.join("z-first");
    let second = bundled.join("a-second");
    fs::create_dir_all(&bin).unwrap();
    // Search order is the reverse of lexical order.
    for dir in [&first, &second] {
        stage_dependencies(&lib, dir);
    }
    let app = bin.join("app");
    for (paths, libs, expected) in [
        (vec![&bin], vec![&first, &second], &first),
        (vec![&bin, &bundled], vec![], &second),
    ] {
        let expected = fs::canonicalize(expected).unwrap();
        for jobs in [1, 2, 4] {
            // The fixture is read-only and fs::copy keeps its mode.
            let _ = fs::remove_file(&app);
            fs::copy(&exe, &app).unwrap();
            let status = Command::new(ours())
                .arg0("auto-formatelf")
                .arg("--interpreter")
                .arg(&exe)
                .arg("--libc")
                .arg(root.path())
                .arg("--jobs")
                .arg(jobs.to_string())
                .arg("--paths")
                .args(&paths)
                .arg("--libs")
                .args(&libs)
                .status()
                .unwrap();
            assert!(
                status.success(),
                "auto-formatelf failed with {jobs} workers"
            );
            assert_eq!(
                out(ours(), "--print-rpath", &app).trim(),
                expected.to_str().unwrap(),
                "search paths {libs:?} with {jobs} workers"
            );
        }
    }
}

#[test]
fn auto_formatelf_reports_unresolved_dependencies() {
    if !zig_available() {
        return;
    }
    let exe = sample("exe-dyn-le");
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join("autofe-missing");
    let bin = root.join("bin");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&bin).unwrap();
    fs::copy(&exe, bin.join("app")).unwrap();

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
