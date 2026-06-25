//! Differential test: our `--print-*` output must match the reference C
//! patchelf byte-for-byte, including success/failure status, across every
//! fixture.

mod fixtures;

use std::path::Path;
use std::process::Command;

const PRINT_OPS: &[&str] = &[
    "--print-interpreter",
    "--print-os-abi",
    "--print-soname",
    "--print-rpath",
    "--print-needed",
    "--print-execstack",
];

fn run(bin: &Path, op: &str, file: &Path) -> (bool, String) {
    let out = Command::new(bin).arg(op).arg(file).output().unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

#[test]
fn print_ops_match_c_patchelf() {
    if !fixtures::zig_available() {
        eprintln!("skipping: zig not on PATH");
        return;
    }
    let Some(reference) = fixtures::c_patchelf() else {
        eprintln!("skipping: reference patchelf not built");
        return;
    };
    let ours = Path::new(env!("CARGO_BIN_EXE_patchelf"));

    for file in fixtures::samples() {
        for op in PRINT_OPS {
            let (c_ok, c_out) = run(&reference, op, &file);
            let (r_ok, r_out) = run(ours, op, &file);
            assert_eq!(
                (c_ok, c_out.as_str()),
                (r_ok, r_out.as_str()),
                "mismatch for {op} on {file:?}"
            );
        }
    }
}
