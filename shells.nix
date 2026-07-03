{
  lib,
  stdenv,
  mkShell,
  cargo,
  rustc,
  clippy,
  rustfmt,
  zig,
  cargo-mutants,
  qemu,
  pkgsCross,
  clangStdenv,
  pkgsMusl,
}:
{
  default = mkShell {
    packages = [
      cargo
      rustc
      clippy
      rustfmt
      zig
      cargo-mutants
    ];
  };
}
// lib.optionalAttrs stdenv.hostPlatform.isLinux {
  # Cross-arch execution shell: qemu-user plus the foreign `hello` binaries and
  # their runtime glibc, surfaced as env vars the tests/cross_exec.rs harness
  # consumes. Linux only; entering it realises the cross packages, so it is kept
  # separate from `default`.
  cross =
    let
      aarch64 = pkgsCross.aarch64-multiplatform;
      ppc64 = pkgsCross.ppc64;
    in
    mkShell {
      packages = [
        cargo
        rustc
        qemu
      ];
      CROSS_AARCH64_HELLO = "${aarch64.hello}/bin/hello";
      CROSS_AARCH64_GLIBC = "${aarch64.glibc}/lib";
      CROSS_PPC64_HELLO = "${ppc64.hello}/bin/hello";
      CROSS_PPC64_GLIBC = "${ppc64.glibc}/lib";
    };

  # Native-arch dynamic executables from diverse compiler/libc combos, exposing
  # the property tests to layouts zig cc never emits. Surfaced as TC_* env vars
  # folded into tests/prop_ops.rs; the props skip these outside this shell.
  toolchains =
    let
      mkBin =
        s:
        s.mkDerivation {
          name = "fixture";
          dontUnpack = true;
          buildPhase = "$CC ${./tests/sample.c} -o hello";
          installPhase = "install -Dm755 hello $out/bin/hello";
        };
    in
    mkShell {
      packages = [
        cargo
        rustc
      ];
      TC_GCC_GLIBC = "${mkBin stdenv}/bin/hello";
      TC_CLANG_GLIBC = "${mkBin clangStdenv}/bin/hello";
      TC_GCC_MUSL = "${mkBin pkgsMusl.stdenv}/bin/hello";
      TC_CLANG_MUSL = "${mkBin pkgsMusl.clangStdenv}/bin/hello";
    };
}
