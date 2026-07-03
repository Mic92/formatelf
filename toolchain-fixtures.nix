# TC_* fixtures: dynamic binaries from diverse compilers, libcs, language
# runtimes and transformation states, covering ELF layouts zig cc never emits.
# Shared by the toolchains shell and the tests check. Values are binary paths.
{
  lib,
  stdenv,
  clangStdenv,
  pkgsMusl,
  rustc,
  go,
  elfkickers,
  gfortran,
  gnat,
  ldc,
  fpc,
  ghc,
  ocaml,
}:
let
  # C/C++ via a stdenv's $CC/$CXX.
  mkC =
    s:
    "${
      s.mkDerivation {
        name = "fixture";
        dontUnpack = true;
        buildPhase = "$CC ${./tests/samples/sample.c} -o hello";
        installPhase = "install -Dm755 hello $out/bin/hello";
      }
    }/bin/hello";
  mkCxx = "${
    stdenv.mkDerivation {
      name = "fixture-cxx";
      dontUnpack = true;
      buildPhase = "$CXX ${./tests/samples/sample.cpp} -o hello";
      installPhase = "install -Dm755 hello $out/bin/hello";
    }
  }/bin/hello";
  mkRust = "${
    stdenv.mkDerivation {
      name = "fixture-rust";
      dontUnpack = true;
      nativeBuildInputs = [ rustc ];
      buildPhase = "rustc -O ${./tests/samples/sample.rs} -o hello";
      installPhase = "install -Dm755 hello $out/bin/hello";
    }
  }/bin/hello";
  # cgo forces external linking, so the output is dynamic.
  mkGo = "${
    stdenv.mkDerivation {
      name = "fixture-go";
      dontUnpack = true;
      nativeBuildInputs = [ go ];
      buildPhase = ''
        export HOME=$TMPDIR GOCACHE=$TMPDIR/gc GO111MODULE=off CGO_ENABLED=1
        cp ${./tests/samples/sample.go} main.go
        go build -o hello main.go
      '';
      installPhase = "install -Dm755 hello $out/bin/hello";
    }
  }/bin/hello";
  # Shared objects: no PT_INTERP, but DT_SONAME, exported dynsym and (C++ via
  # the version script) GNU symbol versions.
  mkRustSo = "${
    stdenv.mkDerivation {
      name = "fixture-rust-cdylib";
      dontUnpack = true;
      nativeBuildInputs = [ rustc ];
      buildPhase = "rustc -O --crate-type=cdylib ${./tests/samples/sample_cdylib.rs} -o libfixture.so";
      installPhase = "install -Dm755 libfixture.so $out/lib/libfixture.so";
    }
  }/lib/libfixture.so";
  mkCxxSo = "${
    stdenv.mkDerivation {
      name = "fixture-cxx-so";
      dontUnpack = true;
      buildPhase = "$CXX -shared -fPIC ${./tests/samples/sample_lib.cpp} -Wl,--version-script=${./tests/samples/sample.map} -Wl,-soname,libfixturecxx.so.1 -o libfixturecxx.so.1";
      installPhase = "install -Dm755 libfixturecxx.so.1 $out/lib/libfixturecxx.so.1";
    }
  }/lib/libfixturecxx.so.1";
  mkGoSo = "${
    stdenv.mkDerivation {
      name = "fixture-go-cshared";
      dontUnpack = true;
      nativeBuildInputs = [ go ];
      buildPhase = ''
        export HOME=$TMPDIR GOCACHE=$TMPDIR/gc GO111MODULE=off CGO_ENABLED=1
        cp ${./tests/samples/sample_cshared.go} main.go
        go build -buildmode=c-shared -o libfixture.so main.go
      '';
      installPhase = "install -Dm755 libfixture.so $out/lib/libfixture.so";
    }
  }/lib/libfixture.so";
  # Transformation states no compiler flag alone produces: sstrip drops the
  # section header table (patchelf must work from program headers alone);
  # pack-relative-relocs emits DT_RELR; -fcf-protection adds .note.gnu.property.
  mkStripped = "${
    stdenv.mkDerivation {
      name = "fixture-stripped";
      dontUnpack = true;
      nativeBuildInputs = [ elfkickers ];
      buildPhase = ''
        $CC ${./tests/samples/sample.c} -o hello
        sstrip hello
      '';
      installPhase = "install -Dm755 hello $out/bin/hello";
    }
  }/bin/hello";
  mkRelr = "${
    stdenv.mkDerivation {
      name = "fixture-relr";
      dontUnpack = true;
      buildPhase = "$CC -O2 -fPIE -pie -Wl,-z,pack-relative-relocs ${./tests/samples/sample_relr.c} -o hello";
      installPhase = "install -Dm755 hello $out/bin/hello";
    }
  }/bin/hello";
  mkGnuProperty = "${
    stdenv.mkDerivation {
      name = "fixture-gnu-property";
      dontUnpack = true;
      buildPhase = "$CC -fcf-protection=full ${./tests/samples/sample.c} -o hello";
      installPhase = "install -Dm755 hello $out/bin/hello";
    }
  }/bin/hello";
  # Feature-driven ELF shapes a trivial hello never produces: TLS (PT_TLS), a
  # static-PIE, and a many-DT_NEEDED binary that stresses PHT relocation.
  mkCFlags =
    s: name: build:
    "${
      s.mkDerivation {
        name = "fixture-${name}";
        dontUnpack = true;
        buildPhase = build;
        installPhase = "install -Dm755 hello $out/bin/hello";
      }
    }/bin/hello";
  # Other compiled languages, for their distinct runtime DT_NEEDED and (Go,
  # FreePascal, GHC) bespoke linker/section layouts. Languages that merely lower
  # to the GNU/LLVM toolchain (Nim, Vala, Crystal) are omitted: they emit the
  # same ELF as gcc/clang, already covered.
  mkLang =
    name: tool: build:
    "${
      stdenv.mkDerivation {
        name = "fixture-${name}";
        dontUnpack = true;
        nativeBuildInputs = [ tool ];
        buildPhase = ''
          export HOME=$TMPDIR
          ${build}
        '';
        installPhase = "install -Dm755 hello $out/bin/hello";
      }
    }/bin/hello";
in
{
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
  TC_STRIPPED = mkStripped;
  TC_RELR = mkRelr;
  TC_TLS = mkCFlags stdenv "tls" "$CC -O2 ${./tests/samples/sample_tls.c} -o hello";
  TC_STATIC_PIE =
    mkCFlags pkgsMusl.stdenv "static-pie"
      "$CC -static-pie -fPIE ${./tests/samples/sample.c} -o hello";
  TC_MANY_NEEDED =
    mkCFlags stdenv "many-needed"
      "$CC -Wl,--no-as-needed ${./tests/samples/sample.c} -lm -ldl -lpthread -lrt -o hello";
  TC_FORTRAN = mkLang "fortran" gfortran "gfortran ${./tests/samples/sample.f90} -o hello";
  TC_D = mkLang "d" ldc ''
    cp ${./tests/samples/sample.d} sample.d
    ldc2 sample.d -of=hello
  '';
  TC_OCAML = mkLang "ocaml" ocaml "ocamlopt ${./tests/samples/sample.ml} -o hello";
  TC_HASKELL = mkLang "haskell" ghc "ghc ${./tests/samples/sample.hs} -o hello";
  TC_PASCAL = mkLang "pascal" fpc ''
    cp ${./tests/samples/sample.pas} sample.pas
    fpc sample.pas
    cp sample hello
  '';
}
// lib.optionalAttrs (stdenv.hostPlatform.system == "x86_64-linux") {
  # -fcf-protection is x86 CET; gnat throws "Unsupported system" on aarch64.
  TC_GNU_PROPERTY = mkGnuProperty;
  TC_ADA = mkLang "ada" gnat ''
    cp ${./tests/samples/sample.adb} sample.adb
    gnatmake sample.adb
    cp sample hello
  '';
}
