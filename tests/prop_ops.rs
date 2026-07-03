//! Property tests for the mutating ops. Generated args over real fixtures
//! stress the layout engine's stateful paths (in-place overwrite vs grow vs
//! region reuse) far better than single examples. The fixtures span 32/64-bit
//! and both endiannesses (MIPS, PowerPC) plus diverse compiler/libc output, so
//! byte-swapping and 32-bit offset paths run too.
//!
//! A silent no-op cannot pass: each mutation must read back (from us and, when
//! built, reference patchelf), stay sound (re-parses, verifies, `DT_STRTAB` on
//! the relocated `.dynstr`), and be a no-op when re-applied.

mod fixtures;

use fixtures::{
    assert_dynstr_synced, c_patchelf, copy_from, guard, ours, out, patch, sample, toolchain_exes,
    toolchain_no_interp, toolchain_sos, zig_available,
};
use proptest::prelude::*;
use proptest::test_runner::{Config, TestCaseError, TestRunner};
use std::path::{Path, PathBuf};

/// Dynamically linked executables (carry a `PT_INTERP`): interpreter ops apply.
const DYN_EXES: &[&str] = &["exe-dyn-le", "exe-nopie-le", "exe-mips-be"];
/// Dynamically linked shared objects: soname ops apply.
const DYN_SOS: &[&str] = &["so-aarch64-le", "so-ppc64-be", "so-ppc-be", "so-soname-le"];

/// Run `test` over values from `strategy`. Each case forks our binary a few
/// times, so keep the count bounded.
fn check<S: Strategy>(strategy: S, test: impl Fn(S::Value) -> Result<(), TestCaseError>) {
    TestRunner::new(Config::with_cases(64))
        .run(&strategy, test)
        .unwrap();
}

fn path() -> impl Strategy<Value = String> {
    "(/[a-z0-9_]{1,10}){1,12}"
}

fn libname() -> impl Strategy<Value = String> {
    "lib[a-z0-9_]{1,18}\\.so\\.[0-9]"
}

/// Dynamic executables, across arches/endiannesses, plus the `toolchains`
/// shell's binaries. Interpreter and rpath laws apply.
fn dyn_exes() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if zig_available() {
        v.extend(DYN_EXES.iter().map(|n| sample(n)));
    }
    v.extend(toolchain_exes());
    v
}

/// Dynamic shared objects, across arches/endiannesses, plus the `toolchains`
/// shell's Rust/C++/Go libraries. soname laws apply.
fn dyn_sos() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if zig_available() {
        v.extend(DYN_SOS.iter().map(|n| sample(n)));
    }
    v.extend(toolchain_sos());
    v
}

/// Every dynamic fixture, executables and shared objects, plus the static-PIE
/// that has no interpreter. rpath, needed and flag laws apply to all.
fn dyn_all() -> Vec<PathBuf> {
    let mut v = dyn_exes();
    v.extend(dyn_sos());
    v.extend(toolchain_no_interp());
    v
}

/// Applying `args` to `bin` a second time must leave it byte-identical.
fn assert_idempotent(bin: &Path, args: &[&str]) -> Result<(), TestCaseError> {
    patch(bin, args);
    let once = std::fs::read(bin).unwrap();
    patch(bin, args);
    prop_assert_eq!(std::fs::read(bin).unwrap(), once);
    Ok(())
}

/// The emitted file must re-parse, pass verify, and keep `DT_STRTAB` pointing
/// at the relocated `.dynstr` (the classic patchelf relayout bug).
fn assert_sound(bin: &Path) -> Result<(), TestCaseError> {
    let bytes = std::fs::read(bin).unwrap();
    let img = formatelf::parser::parse(&bytes).expect("mutated output must re-parse");
    formatelf::verify::run(&img, bytes.len() as u64)
        .map_err(|e| TestCaseError::fail(format!("verify rejected our output: {e}")))?;
    assert_dynstr_synced(bin);
    Ok(())
}

/// `print_op` on `bin` must report `value`, from us and (when built) reference
/// patchelf.
fn reads_back(
    bin: &Path,
    print_op: &str,
    value: &str,
    reference: Option<&Path>,
) -> Result<(), TestCaseError> {
    let got = out(ours(), print_op, bin);
    prop_assert_eq!(got.trim(), value);
    if let Some(r) = reference {
        let ref_got = out(r, print_op, bin);
        prop_assert_eq!(ref_got.trim(), value);
    }
    Ok(())
}

/// Apply a set/clear op, then assert it took (`reads_back`), the output is
/// sound, and re-applying is a no-op.
fn assert_set(
    bin: &Path,
    args: &[&str],
    print_op: &str,
    value: &str,
    reference: Option<&Path>,
) -> Result<(), TestCaseError> {
    patch(bin, args);
    let once = std::fs::read(bin).unwrap();
    reads_back(bin, print_op, value, reference)?;
    assert_sound(bin)?;
    patch(bin, args);
    prop_assert_eq!(std::fs::read(bin).unwrap(), once);
    Ok(())
}

/// Setting an rpath makes it read back, keeps the file sound, and re-setting
/// the same value is a byte-identical no-op.
#[test]
fn set_rpath_applies_and_is_idempotent() {
    let exes = dyn_all();
    if exes.is_empty() {
        return;
    }
    let reference = c_patchelf();
    check((path(), prop::sample::select(exes)), |(rpath, src)| {
        let bin = copy_from(&src, "prop-idem-rpath");
        let args = ["--set-rpath", &rpath];
        assert_set(&bin, &args, "--print-rpath", &rpath, reference.as_deref())
    });
}

/// Likewise for the interpreter path.
#[test]
fn set_interpreter_applies_and_is_idempotent() {
    let exes = dyn_exes();
    if exes.is_empty() {
        return;
    }
    let reference = c_patchelf();
    check((path(), prop::sample::select(exes)), |(interp, src)| {
        let bin = copy_from(&src, "prop-idem-interp");
        let args = ["--set-interpreter", &interp];
        assert_set(
            &bin,
            &args,
            "--print-interpreter",
            &interp,
            reference.as_deref(),
        )
    });
}

/// Flag-setting ops carry no argument; applying them again must not perturb the
/// file (e.g. re-setting `DF_1_NODEFLIB` or re-adding a `DT_DEBUG` slot).
/// Flag-setting ops carry no argument; applying them again must not perturb the
/// file (e.g. re-setting `DF_1_NODEFLIB` or re-adding a `DT_DEBUG` slot), and
/// the result stays sound.
#[test]
fn flag_ops_are_idempotent() {
    for src in dyn_all() {
        for op in ["--no-default-lib", "--add-debug-tag"] {
            let bin = copy_from(&src, "prop-idem-flag");
            assert_idempotent(&bin, &[op]).unwrap();
            assert_sound(&bin).unwrap();
        }
    }
}

/// Setting a soname makes it read back, keeps the library sound, and re-setting
/// is a no-op.
#[test]
fn set_soname_applies_and_is_idempotent() {
    let sos = dyn_sos();
    if sos.is_empty() {
        return;
    }
    let reference = c_patchelf();
    check((libname(), prop::sample::select(sos)), |(soname, src)| {
        let lib = copy_from(&src, "prop-idem-soname");
        let args = ["--set-soname", &soname];
        assert_set(&lib, &args, "--print-soname", &soname, reference.as_deref())
    });
}

/// Clearing a symbol's version binding twice is a no-op the second time and
/// leaves the file sound. Uses the versioned C++ shared library, whose `probe`
/// export carries a VERDEF entry.
#[test]
fn clear_symbol_version_is_idempotent() {
    let Ok(lib) = std::env::var("TC_CXX_SO") else {
        return;
    };
    let bin = copy_from(Path::new(&lib), "prop-idem-clearver");
    assert_idempotent(&bin, &["--clear-symbol-version", "probe"]).unwrap();
    assert_sound(&bin).unwrap();
}

/// Removing an rpath clears it (empty read-back), keeps the file sound, and a
/// second remove is a no-op.
#[test]
fn remove_rpath_clears_and_is_idempotent() {
    let exes = dyn_all();
    if exes.is_empty() {
        return;
    }
    let reference = c_patchelf();
    check((path(), prop::sample::select(exes)), |(rpath, src)| {
        let bin = copy_from(&src, "prop-idem-rmrpath");
        patch(&bin, &["--set-rpath", &rpath]);
        assert_set(
            &bin,
            &["--remove-rpath"],
            "--print-rpath",
            "",
            reference.as_deref(),
        )
    });
}

/// Renaming a `DT_NEEDED` entry and back is an inverse: the list is restored.
#[test]
fn replace_needed_roundtrip() {
    let exes = dyn_all();
    if exes.is_empty() {
        return;
    }
    check(prop::sample::select(exes), |src| {
        let bin = copy_from(&src, "prop-inv-replace");
        let needed = out(ours(), "--print-needed", &bin);
        let Some(first) = needed.lines().next().map(str::to_owned) else {
            return Ok(());
        };
        let probe = "libfmtelf_probe.so.9";
        patch(&bin, &["--replace-needed", &first, probe]);
        patch(&bin, &["--replace-needed", probe, &first]);
        prop_assert_eq!(out(ours(), "--print-needed", &bin), needed);
        assert_sound(&bin)
    });
}

/// add-needed then remove-needed is an inverse on the `DT_NEEDED` list (the
/// reference reads back the original entries; orphaned strings may linger, so
/// the file need not be byte-identical).
#[test]
fn add_then_remove_needed_restores_list() {
    let Some(reference) = guard() else { return };
    check(
        (libname(), prop::sample::select(dyn_all())),
        |(lib, src)| {
            let before = out(&reference, "--print-needed", &src);
            let bin = copy_from(&src, "prop-inv-needed");
            patch(&bin, &["--add-needed", &lib]);
            patch(&bin, &["--remove-needed", &lib]);
            prop_assert_eq!(out(&reference, "--print-needed", &bin), before);
            assert_sound(&bin)
        },
    );
}
