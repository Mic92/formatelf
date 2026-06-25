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

#[test]
fn set_soname_on_shared_object() {
    let Some(reference) = guard() else { return };
    let lib = copy("so-soname-le", "soname");
    patch(
        &lib,
        &["--set-soname", "librenamed-with-a-much-longer-name.so.7"],
    );
    assert_eq!(
        out(&reference, "--print-soname", &lib).trim(),
        "librenamed-with-a-much-longer-name.so.7"
    );
}

#[test]
fn add_then_remove_needed_round_trips() {
    let Some(reference) = guard() else { return };
    let src = sample("exe-dyn-le");
    let before = out(&reference, "--print-needed", &src);

    let bin = copy("exe-dyn-le", "needed");
    patch(&bin, &["--add-needed", "libextra-placeholder.so.9"]);
    let after_add = out(&reference, "--print-needed", &bin);
    assert!(
        after_add.lines().any(|l| l == "libextra-placeholder.so.9"),
        "added lib missing: {after_add:?}"
    );

    patch(&bin, &["--remove-needed", "libextra-placeholder.so.9"]);
    assert_eq!(out(&reference, "--print-needed", &bin), before);
    // The bogus entry is gone, so the binary loads again.
    assert!(Command::new(&bin).status().unwrap().success());
}

#[test]
fn replace_needed_changes_entry() {
    let Some(reference) = guard() else { return };
    let src = sample("exe-dyn-le");
    let needed = out(&reference, "--print-needed", &src);
    let first = needed.lines().next().unwrap();

    let bin = copy("exe-dyn-le", "replace");
    patch(
        &bin,
        &["--replace-needed", first, "libreplacement-longer-name.so.3"],
    );
    let after = out(&reference, "--print-needed", &bin);
    assert!(after
        .lines()
        .any(|l| l == "libreplacement-longer-name.so.3"));
    assert!(!after.lines().any(|l| l == first));
}

#[test]
fn remove_rpath_clears_runpath_keeps_needed() {
    let Some(reference) = guard() else { return };
    let src = sample("exe-dyn-le");
    assert_eq!(
        out(&reference, "--print-rpath", &src).trim(),
        "/opt/custom/lib"
    );
    let needed = out(&reference, "--print-needed", &src);

    let bin = copy("exe-dyn-le", "norpath");
    patch(&bin, &["--remove-rpath"]);
    assert_eq!(out(&reference, "--print-rpath", &bin).trim(), "");
    assert_eq!(out(&reference, "--print-needed", &bin), needed);
}

#[test]
fn set_rpath_grows_and_stays_loadable() {
    let Some(reference) = guard() else { return };
    let src = sample("exe-dyn-le");
    assert!(Command::new(&src).status().unwrap().success());
    let needed = out(&reference, "--print-needed", &src);
    let interp = out(&reference, "--print-interpreter", &src);

    let bin = copy("exe-dyn-le", "rpath");
    let long = "/a/very/long/custom/runpath/that/will/not/fit/in/place/lib";
    patch(&bin, &["--set-rpath", long]);

    assert_eq!(out(&reference, "--print-rpath", &bin).trim(), long);
    assert_eq!(out(&reference, "--print-needed", &bin), needed);
    assert_eq!(out(&reference, "--print-interpreter", &bin), interp);

    // The loader resolves DT_RUNPATH via DT_STRTAB's address, not the section
    // header, so DT_STRTAB must point at the relocated .dynstr.
    let img = patchelf_rs::parser::parse(&std::fs::read(&bin).unwrap()).unwrap();
    let strtab = img
        .dynamic
        .iter()
        .find(|d| d.tag == patchelf_rs::ir::dt::STRTAB)
        .unwrap()
        .val;
    let dynstr = img.shdrs[img.find_section(".dynstr").unwrap()].addr;
    assert_eq!(strtab, dynstr, "DT_STRTAB not synced to moved .dynstr");

    assert!(
        Command::new(&bin).status().unwrap().success(),
        "patched binary failed to run"
    );
}

#[test]
fn add_rpath_appends_to_existing() {
    let Some(reference) = guard() else { return };
    let src = sample("exe-dyn-le");
    let before = out(&reference, "--print-rpath", &src);
    let before = before.trim();

    let bin = copy("exe-dyn-le", "addrpath");
    patch(&bin, &["--add-rpath", "/new/dir/here"]);
    assert_eq!(
        out(&reference, "--print-rpath", &bin).trim(),
        format!("{before}:/new/dir/here")
    );
    assert!(Command::new(&bin).status().unwrap().success());
}

#[test]
fn force_rpath_uses_dt_rpath_tag() {
    let Some(reference) = guard() else { return };
    let bin = copy("exe-dyn-le", "forcerpath");
    patch(
        &bin,
        &["--force-rpath", "--set-rpath", "/forced/path/longlonglong"],
    );

    assert_eq!(
        out(&reference, "--print-rpath", &bin).trim(),
        "/forced/path/longlonglong"
    );
    let img = patchelf_rs::parser::parse(&std::fs::read(&bin).unwrap()).unwrap();
    assert!(
        img.dynamic
            .iter()
            .any(|d| d.tag == patchelf_rs::ir::dt::RPATH),
        "expected DT_RPATH after --force-rpath"
    );
    assert!(
        !img.dynamic
            .iter()
            .any(|d| d.tag == patchelf_rs::ir::dt::RUNPATH),
        "DT_RUNPATH should have been converted"
    );
    assert!(Command::new(&bin).status().unwrap().success());
}

#[test]
fn set_os_abi_changes_ident() {
    let Some(reference) = guard() else { return };
    let bin = copy("exe-dyn-le", "osabi");
    patch(&bin, &["--set-os-abi", "freebsd"]);
    assert_eq!(out(&reference, "--print-os-abi", &bin).trim(), "FreeBSD");
}

#[test]
fn execstack_set_then_clear() {
    let Some(reference) = guard() else { return };
    let bin = copy("exe-dyn-le", "execstack");

    patch(&bin, &["--set-execstack"]);
    assert_eq!(
        out(&reference, "--print-execstack", &bin).trim(),
        "execstack: X"
    );
    assert!(Command::new(&bin).status().unwrap().success());

    patch(&bin, &["--clear-execstack"]);
    assert_eq!(
        out(&reference, "--print-execstack", &bin).trim(),
        "execstack: -"
    );
    assert!(Command::new(&bin).status().unwrap().success());
}

#[test]
fn no_default_lib_sets_flag() {
    if !fixtures::zig_available() {
        eprintln!("skipping: zig not on PATH");
        return;
    }
    let bin = copy("exe-dyn-le", "nodeflib");
    patch(&bin, &["--no-default-lib"]);
    let img = patchelf_rs::parser::parse(&std::fs::read(&bin).unwrap()).unwrap();
    let flags1 = img
        .dynamic
        .iter()
        .find(|d| d.tag == patchelf_rs::ir::dt::FLAGS_1)
        .expect("DT_FLAGS_1 present");
    assert_ne!(flags1.val & patchelf_rs::ir::df1::NODEFLIB, 0);
}

#[test]
fn add_debug_tag_inserts_entry() {
    if !fixtures::zig_available() {
        eprintln!("skipping: zig not on PATH");
        return;
    }
    let bin = copy("exe-dyn-le", "debugtag");
    patch(&bin, &["--add-debug-tag"]);
    let img = patchelf_rs::parser::parse(&std::fs::read(&bin).unwrap()).unwrap();
    assert!(img
        .dynamic
        .iter()
        .any(|d| d.tag == patchelf_rs::ir::dt::DEBUG));
    // Runs because DT_DEBUG is benign.
    assert!(Command::new(&bin).status().unwrap().success());
}
