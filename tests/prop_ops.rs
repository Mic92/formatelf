//! Idempotence/inverse property tests for the mutating ops. Generated args
//! over real fixtures stress the layout engine's stateful paths (in-place
//! overwrite vs grow vs region reuse) far better than single examples. The
//! fixtures span 32/64-bit and both endiannesses (MIPS, PowerPC) plus diverse
//! compiler/libc output, so byte-swapping and 32-bit offset paths run too.

mod fixtures;

use fixtures::{
    copy_from, guard, ours, out, patch, sample, toolchain_exes, toolchain_sos, zig_available,
};
use proptest::prelude::*;
use proptest::test_runner::{Config, TestCaseError, TestRunner};
use std::path::PathBuf;

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

/// Every dynamic fixture, executables and shared objects. rpath, needed and
/// flag laws apply to all.
fn dyn_all() -> Vec<PathBuf> {
    let mut v = dyn_exes();
    v.extend(dyn_sos());
    v
}

/// Applying `args` to `bin` a second time must leave it byte-identical.
fn assert_idempotent(bin: &std::path::Path, args: &[&str]) -> Result<(), TestCaseError> {
    patch(bin, args);
    let once = std::fs::read(bin).unwrap();
    patch(bin, args);
    prop_assert_eq!(std::fs::read(bin).unwrap(), once);
    Ok(())
}

/// Setting the same value twice must produce a byte-identical file: the second
/// pass fits the slot the first created, so no relayout and no growth.
#[test]
fn set_rpath_is_idempotent() {
    let exes = dyn_all();
    if exes.is_empty() {
        return;
    }
    check((path(), prop::sample::select(exes)), |(rpath, src)| {
        let bin = copy_from(&src, "prop-idem-rpath");
        assert_idempotent(&bin, &["--set-rpath", &rpath])
    });
}

/// Setting the same interpreter twice reuses the slot the first pass sized, so
/// the file stays byte-identical.
#[test]
fn set_interpreter_is_idempotent() {
    let exes = dyn_exes();
    if exes.is_empty() {
        return;
    }
    check((path(), prop::sample::select(exes)), |(interp, src)| {
        let bin = copy_from(&src, "prop-idem-interp");
        assert_idempotent(&bin, &["--set-interpreter", &interp])
    });
}

/// Flag-setting ops carry no argument; applying them again must not perturb the
/// file (e.g. re-setting `DF_1_NODEFLIB` or re-adding a `DT_DEBUG` slot).
#[test]
fn flag_ops_are_idempotent() {
    for src in dyn_all() {
        for op in ["--no-default-lib", "--add-debug-tag"] {
            let bin = copy_from(&src, "prop-idem-flag");
            assert_idempotent(&bin, &[op]).unwrap();
        }
    }
}

#[test]
fn set_soname_is_idempotent() {
    let sos = dyn_sos();
    if sos.is_empty() {
        return;
    }
    check((libname(), prop::sample::select(sos)), |(soname, src)| {
        let lib = copy_from(&src, "prop-idem-soname");
        assert_idempotent(&lib, &["--set-soname", &soname])
    });
}

/// Clearing a symbol's version binding twice is a no-op the second time, so the
/// file stays byte-identical. Uses the versioned C++ shared library, whose
/// `probe` export carries a VERDEF entry.
#[test]
fn clear_symbol_version_is_idempotent() {
    let Ok(lib) = std::env::var("TC_CXX_SO") else {
        return;
    };
    let bin = copy_from(std::path::Path::new(&lib), "prop-idem-clearver");
    assert_idempotent(&bin, &["--clear-symbol-version", "probe"]).unwrap();
}

/// Removing an rpath we just removed is a no-op, so a second remove leaves the
/// file unchanged.
#[test]
fn remove_rpath_is_idempotent() {
    let exes = dyn_all();
    if exes.is_empty() {
        return;
    }
    check((path(), prop::sample::select(exes)), |(rpath, src)| {
        let bin = copy_from(&src, "prop-idem-rmrpath");
        patch(&bin, &["--set-rpath", &rpath]);
        assert_idempotent(&bin, &["--remove-rpath"])
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
        Ok(())
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
            Ok(())
        },
    );
}
