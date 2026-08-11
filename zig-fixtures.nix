# Pre-built zig-cc fixtures for the test suite. Compiling them on demand costs
# ~90s cold (zig builds musl/compiler-rt per target); building here caches the
# result in the store so CI reuses it. The target list mirrors SPECS in
# tests/fixtures/mod.rs; a missing entry fails the test that needs it.
{
  stdenv,
  zig,
}:
stdenv.mkDerivation {
  name = "formatelf-zig-fixtures";
  dontUnpack = true;
  # fixupPhase would shrink rpaths and strip, corrupting the fixtures.
  dontFixup = true;
  nativeBuildInputs = [ zig ];
  buildPhase = ''
    export ZIG_GLOBAL_CACHE_DIR=$TMPDIR/zig
    src=${./tests/samples/sample.c}
    bunsrc=${./tests/samples/sample_bun.c}
    mkdir -p $out
    build() { zig cc -target "$2" "''${@:3}" "$src" -o "$out/$1"; }
    buildbun() { zig cc -target "$2" "''${@:3}" "$bunsrc" -o "$out/$1"; }
    build exe-x86_64-le x86_64-linux-musl
    build exe-x86-le x86-linux-musl
    build so-aarch64-le aarch64-linux-musl -shared
    build so-ppc64-be powerpc64-linux-musl -shared
    build so-ppc-be powerpc-linux-musleabi -shared
    build obj-x86_64 x86_64-linux-musl -c
    build exe-dyn-le x86_64-linux-gnu -fPIE -pie -Wl,-rpath,/opt/custom/lib
    build exe-nopie-le x86_64-linux-gnu -no-pie -fno-pie -Wl,-rpath,/opt/custom/lib
    build so-soname-le x86_64-linux-gnu -shared -Wl,-soname,libsample.so.1
    build exe-mips-be mips-linux-gnueabi -fPIE -pie
    buildbun exe-bun-le x86_64-linux-gnu -no-pie -fno-pie
  '';
  dontInstall = true;
}
