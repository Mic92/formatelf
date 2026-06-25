//! On-demand ELF fixtures. Rather than commit binaries, we compile a tiny C
//! source with `zig cc` for several arch/endian/class combinations the first
//! time the fixtures are requested. Requires `zig` on PATH (provided by the
//! dev shell); tests skip themselves when it is absent.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// (file name, zig target triple, extra zig cc flags) covering the codec matrix:
/// 64/32-bit and little/big-endian, plus a relocatable object.
const SPECS: &[(&str, &str, &[&str])] = &[
    ("exe-x86_64-le", "x86_64-linux-musl", &[]),
    ("exe-x86-le", "x86-linux-musl", &[]),
    ("so-aarch64-le", "aarch64-linux-musl", &["-shared"]),
    ("so-ppc64-be", "powerpc64-linux-musl", &["-shared"]),
    ("so-ppc-be", "powerpc-linux-musleabi", &["-shared"]),
    ("obj-x86_64", "x86_64-linux-musl", &["-c"]),
];

pub fn zig_available() -> bool {
    Command::new("zig")
        .arg("version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn dir() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("elf-fixtures");
        let src = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("sample.c");
        std::fs::create_dir_all(&d).unwrap();
        let cache = d.join(".zig-cache");
        for (name, target, extra) in SPECS {
            let out = d.join(name);
            if out.exists() {
                continue;
            }
            let status = Command::new("zig")
                .args(["cc", "-target", target])
                .args(*extra)
                .arg(&src)
                .arg("-o")
                .arg(&out)
                .env("ZIG_GLOBAL_CACHE_DIR", &cache)
                .status()
                .expect("run zig cc");
            assert!(status.success(), "zig cc failed for {name} ({target})");
        }
        d
    })
}

/// Paths to all generated fixtures.
pub fn samples() -> Vec<PathBuf> {
    SPECS.iter().map(|(name, ..)| dir().join(name)).collect()
}
