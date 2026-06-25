//! `--remove-rpath` is validated semantically rather than byte-wise: the C
//! patchelf normalizes the whole file on write, so we instead patch a copy and
//! confirm the reference tool reads back no rpath with the needed list intact.

mod fixtures;

use std::path::Path;
use std::process::Command;

fn read(bin: &Path, op: &str, file: &Path) -> String {
    let out = Command::new(bin).arg(op).arg(file).output().unwrap();
    assert!(out.status.success(), "{op} on {file:?} failed");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn remove_rpath_clears_runpath_keeps_needed() {
    if !fixtures::zig_available() {
        eprintln!("skipping: zig not on PATH");
        return;
    }
    let Some(reference) = fixtures::c_patchelf() else {
        eprintln!("skipping: reference patchelf not built");
        return;
    };
    let ours = Path::new(env!("CARGO_BIN_EXE_patchelf"));

    // exe-dyn-le carries a DT_RUNPATH from -Wl,-rpath.
    let src = fixtures::samples()
        .into_iter()
        .find(|p| p.file_name().unwrap() == "exe-dyn-le")
        .unwrap();
    assert_eq!(
        read(&reference, "--print-rpath", &src).trim(),
        "/opt/custom/lib"
    );
    let needed_before = read(&reference, "--print-needed", &src);

    let copy = Path::new(env!("CARGO_TARGET_TMPDIR")).join("exe-dyn-le.norpath");
    std::fs::copy(&src, &copy).unwrap();
    let status = Command::new(ours)
        .arg("--remove-rpath")
        .arg(&copy)
        .status()
        .unwrap();
    assert!(status.success());

    assert_eq!(read(&reference, "--print-rpath", &copy).trim(), "");
    assert_eq!(read(&reference, "--print-needed", &copy), needed_before);
}
