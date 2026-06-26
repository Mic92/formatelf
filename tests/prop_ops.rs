//! Idempotence/inverse property tests for the mutating ops. Generated args
//! over a real fixture stress the layout engine's stateful paths (in-place
//! overwrite vs grow vs region reuse) far better than a single example.

mod fixtures;

use fixtures::{copy, guard, out, patch, sample, zig_available};
use proptest::prelude::*;
use proptest::test_runner::{Config, TestCaseError, TestRunner};

/// Run `test` over values from `strategy`. Each case forks our binary a few
/// times, so keep the count modest.
fn check<S: Strategy>(strategy: S, test: impl Fn(S::Value) -> Result<(), TestCaseError>) {
    TestRunner::new(Config::with_cases(16))
        .run(&strategy, test)
        .unwrap();
}

fn path() -> impl Strategy<Value = String> {
    "(/[a-z0-9_]{1,10}){1,12}"
}

fn libname() -> impl Strategy<Value = String> {
    "lib[a-z0-9_]{1,18}\\.so\\.[0-9]"
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
    if !zig_available() {
        return;
    }
    let fixtures = prop::sample::select(vec!["exe-dyn-le", "exe-nopie-le"]);
    check((path(), fixtures), |(rpath, fixture)| {
        let bin = copy(fixture, "prop-idem-rpath");
        assert_idempotent(&bin, &["--set-rpath", &rpath])
    });
}

#[test]
fn set_soname_is_idempotent() {
    if !zig_available() {
        return;
    }
    check(libname(), |soname| {
        let lib = copy("so-soname-le", "prop-idem-soname");
        assert_idempotent(&lib, &["--set-soname", &soname])
    });
}

/// Removing an rpath we just removed is a no-op, so a second remove leaves the
/// file unchanged.
#[test]
fn remove_rpath_is_idempotent() {
    if !zig_available() {
        return;
    }
    check(path(), |rpath| {
        let bin = copy("exe-dyn-le", "prop-idem-rmrpath");
        patch(&bin, &["--set-rpath", &rpath]);
        assert_idempotent(&bin, &["--remove-rpath"])
    });
}

/// add-needed then remove-needed is an inverse on the DT_NEEDED list (the
/// reference reads back the original entries; orphaned strings may linger, so
/// the file need not be byte-identical).
#[test]
fn add_then_remove_needed_restores_list() {
    let Some(reference) = guard() else { return };
    let before = out(&reference, "--print-needed", &sample("exe-dyn-le"));
    check(libname(), |lib| {
        let bin = copy("exe-dyn-le", "prop-inv-needed");
        patch(&bin, &["--add-needed", &lib]);
        patch(&bin, &["--remove-needed", &lib]);
        prop_assert_eq!(out(&reference, "--print-needed", &bin), before.clone());
        Ok(())
    });
}
