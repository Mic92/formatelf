//! Cross-architecture execution: patch a dynamically linked foreign binary and
//! run it under qemu-user to prove the relayout output is acceptable to the
//! real loader, on both endiannesses.
//!
//! The foreign `hello` binaries, their runtime glibc and qemu are provided by
//! the `cross` dev shell (see flake.nix), which exports CROSS_<ARCH>_HELLO and
//! CROSS_<ARCH>_GLIBC. Outside that shell the env vars are unset and the tests
//! skip. Run with: `nix develop .#cross -c cargo test --test cross_exec`.

use std::path::Path;
use std::process::Command;

/// Assemble a qemu sysroot from `glibc_lib`, then check `hello` runs both
/// before and after a growing patch under `qemu`.
fn check(arch: &str, qemu: &str, hello: &str, glibc_lib: &str) {
    let hello = Path::new(hello);

    // Sysroot exposing the loader and libc under both /lib and /lib64.
    let tmp = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("sysroot-{arch}"));
    let libs: Vec<_> = std::fs::read_dir(glibc_lib)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.file_name().unwrap().to_string_lossy().contains(".so"))
        .collect();
    for sub in ["lib", "lib64"] {
        let d = tmp.join(sub);
        std::fs::create_dir_all(&d).unwrap();
        for p in &libs {
            let link = d.join(p.file_name().unwrap());
            let _ = std::fs::remove_file(&link);
            std::os::unix::fs::symlink(p, &link).unwrap();
        }
    }
    let run = |bin: &Path| {
        Command::new(qemu)
            .arg("-L")
            .arg(&tmp)
            .arg(bin)
            .status()
            .unwrap()
            .success()
    };
    assert!(run(hello), "{arch}: baseline failed to run");

    let bin = tmp.join("hello");
    std::fs::copy(hello, &bin).unwrap();
    let mut perms = std::fs::metadata(&bin).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o700);
    std::fs::set_permissions(&bin, perms).unwrap();

    let patched = Command::new(env!("CARGO_BIN_EXE_formatelf"))
        .args([
            "--set-rpath",
            "/opt/cross/relayout/forces/a/long/enough/rpath/xxxx",
        ])
        .arg(&bin)
        .status()
        .unwrap()
        .success();
    assert!(patched, "{arch}: patch failed");
    assert!(run(&bin), "{arch}: patched binary failed to run");
}

fn run_arch(arch: &str, qemu: &str, hello_var: &str, glibc_var: &str) {
    let (Ok(hello), Ok(glibc)) = (std::env::var(hello_var), std::env::var(glibc_var)) else {
        eprintln!("skipping {arch}: {hello_var}/{glibc_var} unset (enter `nix develop .#cross`)");
        return;
    };
    check(arch, qemu, &hello, &glibc);
}

#[test]
fn aarch64_little_endian() {
    run_arch(
        "aarch64",
        "qemu-aarch64",
        "CROSS_AARCH64_HELLO",
        "CROSS_AARCH64_GLIBC",
    );
}

#[test]
fn ppc64_big_endian() {
    run_arch(
        "ppc64",
        "qemu-ppc64",
        "CROSS_PPC64_HELLO",
        "CROSS_PPC64_GLIBC",
    );
}
