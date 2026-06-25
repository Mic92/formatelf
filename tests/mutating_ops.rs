//! Growing mutating ops validated against the reference patchelf, plus
//! execution checks for the cases that stay loadable.

mod fixtures;

use std::path::{Path, PathBuf};
use std::process::Command;

fn ours() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_patchelf"))
}

fn out(bin: &Path, op: &str, file: &Path) -> String {
    let o = Command::new(bin).arg(op).arg(file).output().unwrap();
    assert!(o.status.success(), "{op} on {file:?} failed");
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn sample(name: &str) -> PathBuf {
    fixtures::samples()
        .into_iter()
        .find(|p| p.file_name().unwrap() == name)
        .unwrap()
}

fn copy(name: &str, suffix: &str) -> PathBuf {
    let dst = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("{name}.{suffix}"));
    std::fs::copy(sample(name), &dst).unwrap();
    dst
}

fn patch(file: &Path, args: &[&str]) {
    let st = Command::new(ours()).args(args).arg(file).status().unwrap();
    assert!(st.success(), "patch {args:?} failed");
}

fn guard() -> Option<PathBuf> {
    if !fixtures::zig_available() {
        eprintln!("skipping: zig not on PATH");
        return None;
    }
    let r = fixtures::c_patchelf();
    if r.is_none() {
        eprintln!("skipping: reference patchelf not built");
    }
    r
}

#[test]
fn set_interpreter_grows_and_runs() {
    let Some(reference) = guard() else { return };
    let src = sample("exe-dyn-le");
    assert!(Command::new(&src).status().unwrap().success());

    // Symlink the working loader under a longer path so the grown interpreter
    // still resolves and the binary remains runnable.
    let interp = out(&reference, "--print-interpreter", &src);
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("loaderdir-with-a-long-name");
    std::fs::create_dir_all(&dir).unwrap();
    let link = dir.join("ld-linux-renamed-long.so.2");
    let _ = std::fs::remove_file(&link);
    std::os::unix::fs::symlink(interp.trim(), &link).unwrap();

    let bin = copy("exe-dyn-le", "interp");
    let want = link.to_str().unwrap();
    patch(&bin, &["--set-interpreter", want]);

    assert_eq!(out(&reference, "--print-interpreter", &bin).trim(), want);
    assert!(
        Command::new(&bin).status().unwrap().success(),
        "patched binary failed to run"
    );
}
