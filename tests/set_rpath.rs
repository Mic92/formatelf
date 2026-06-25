//! Growing edit exercising the layout engine: set a longer RPATH on a PIE,
//! then confirm the reference patchelf reads it back, the needed list and
//! interpreter are untouched, and the patched binary still executes.

mod fixtures;

use std::path::Path;
use std::process::Command;

fn out(bin: &Path, op: &str, file: &Path) -> String {
    let o = Command::new(bin).arg(op).arg(file).output().unwrap();
    assert!(o.status.success(), "{op} on {file:?} failed");
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn set_rpath_grows_and_stays_loadable() {
    if !fixtures::zig_available() {
        eprintln!("skipping: zig not on PATH");
        return;
    }
    let Some(reference) = fixtures::c_patchelf() else {
        eprintln!("skipping: reference patchelf not built");
        return;
    };
    let ours = Path::new(env!("CARGO_BIN_EXE_patchelf"));

    let src = fixtures::samples()
        .into_iter()
        .find(|p| p.file_name().unwrap() == "exe-dyn-le")
        .unwrap();
    // Sanity: the unpatched binary runs on this host.
    assert!(Command::new(&src).status().unwrap().success());

    let needed = out(&reference, "--print-needed", &src);
    let interp = out(&reference, "--print-interpreter", &src);

    let copy = Path::new(env!("CARGO_TARGET_TMPDIR")).join("exe-dyn-le.rpath");
    std::fs::copy(&src, &copy).unwrap();

    let long = "/a/very/long/custom/runpath/that/will/not/fit/in/place/lib";
    assert!(Command::new(ours)
        .args(["--set-rpath", long])
        .arg(&copy)
        .status()
        .unwrap()
        .success());

    assert_eq!(out(&reference, "--print-rpath", &copy).trim(), long);
    assert_eq!(out(&reference, "--print-needed", &copy), needed);
    assert_eq!(out(&reference, "--print-interpreter", &copy), interp);

    // The loader resolves DT_RUNPATH via DT_STRTAB's address, not the section
    // header, so DT_STRTAB must point at the relocated .dynstr.
    let patched = std::fs::read(&copy).unwrap();
    let img = patchelf_rs::parser::parse(&patched).unwrap();
    let strtab_addr = img
        .dynamic
        .iter()
        .find(|d| d.tag == patchelf_rs::ir::dt::STRTAB)
        .unwrap()
        .val;
    let dynstr_addr = img.shdrs[img.find_section(".dynstr").unwrap()].addr;
    assert_eq!(
        strtab_addr, dynstr_addr,
        "DT_STRTAB not synced to moved .dynstr"
    );

    // The relocated program headers and PT_LOAD must still be loadable.
    let status = Command::new(&copy).status().unwrap();
    assert!(status.success(), "patched binary failed to run");
}
