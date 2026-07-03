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
  go,
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

  # Native-arch dynamic executables from diverse compilers, libcs and language
  # runtimes, exposing the property tests to layouts zig cc never emits (Go's
  # linker, Rust's heavier .dynamic, C++ exception sections). Surfaced as TC_*
  # env vars folded into tests/prop_ops.rs; the props skip these outside this
  # shell.
  toolchains =
    let
      # C/C++ via a stdenv's $CC/$CXX.
      mkC = s: "${s.mkDerivation {
        name = "fixture";
        dontUnpack = true;
        buildPhase = "$CC ${./tests/sample.c} -o hello";
        installPhase = "install -Dm755 hello $out/bin/hello";
      }}/bin/hello";
      mkCxx = "${stdenv.mkDerivation {
        name = "fixture-cxx";
        dontUnpack = true;
        buildPhase = "$CXX ${./tests/sample.cpp} -o hello";
        installPhase = "install -Dm755 hello $out/bin/hello";
      }}/bin/hello";
      mkRust = "${stdenv.mkDerivation {
        name = "fixture-rust";
        dontUnpack = true;
        nativeBuildInputs = [ rustc ];
        buildPhase = "rustc -O ${./tests/sample.rs} -o hello";
        installPhase = "install -Dm755 hello $out/bin/hello";
      }}/bin/hello";
      # cgo forces external linking, so the output is dynamic.
      mkGo = "${stdenv.mkDerivation {
        name = "fixture-go";
        dontUnpack = true;
        nativeBuildInputs = [ go ];
        buildPhase = ''
          export HOME=$TMPDIR GOCACHE=$TMPDIR/gc GO111MODULE=off CGO_ENABLED=1
          cp ${./tests/sample.go} main.go
          go build -o hello main.go
        '';
        installPhase = "install -Dm755 hello $out/bin/hello";
      }}/bin/hello";
      # Shared objects: no PT_INTERP, but DT_SONAME, exported dynsym and (C++
      # via the version script) GNU symbol versions.
      mkRustSo = "${stdenv.mkDerivation {
        name = "fixture-rust-cdylib";
        dontUnpack = true;
        nativeBuildInputs = [ rustc ];
        buildPhase = "rustc -O --crate-type=cdylib ${./tests/sample_cdylib.rs} -o libfixture.so";
        installPhase = "install -Dm755 libfixture.so $out/lib/libfixture.so";
      }}/lib/libfixture.so";
      mkCxxSo = "${stdenv.mkDerivation {
        name = "fixture-cxx-so";
        dontUnpack = true;
        buildPhase = "$CXX -shared -fPIC ${./tests/sample_lib.cpp} -Wl,--version-script=${./tests/sample.map} -Wl,-soname,libfixturecxx.so.1 -o libfixturecxx.so.1";
        installPhase = "install -Dm755 libfixturecxx.so.1 $out/lib/libfixturecxx.so.1";
      }}/lib/libfixturecxx.so.1";
      mkGoSo = "${stdenv.mkDerivation {
        name = "fixture-go-cshared";
        dontUnpack = true;
        nativeBuildInputs = [ go ];
        buildPhase = ''
          export HOME=$TMPDIR GOCACHE=$TMPDIR/gc GO111MODULE=off CGO_ENABLED=1
          cp ${./tests/sample_cshared.go} main.go
          go build -buildmode=c-shared -o libfixture.so main.go
        '';
        installPhase = "install -Dm755 libfixture.so $out/lib/libfixture.so";
      }}/lib/libfixture.so";
    in
    mkShell {
      packages = [
        cargo
        rustc
      ];
      TC_GCC_GLIBC = mkC stdenv;
      TC_CLANG_GLIBC = mkC clangStdenv;
      TC_GCC_MUSL = mkC pkgsMusl.stdenv;
      TC_CLANG_MUSL = mkC pkgsMusl.clangStdenv;
      TC_CXX = mkCxx;
      TC_RUST = mkRust;
      TC_GO_CGO = mkGo;
      TC_RUST_CDYLIB = mkRustSo;
      TC_CXX_SO = mkCxxSo;
      TC_GO_CSHARED = mkGoSo;
    };
}
