{
  formatelf,
  callPackage,
  # TC_* toolchain/language fixtures; empty runs only the zig + reference suite.
  toolchainFixtures ? { },
}:
# Runs the suite in a derivation. package.nix keeps doCheck off because the
# tests need zig fixtures and a reference patchelf; supply both plus the shared
# TC_* fixtures here. installPhase drops the patchelf-symlink postInstall.
formatelf.overrideAttrs (
  old:
  {
    pname = "formatelf-tests";
    doCheck = true;
    FORMATELF_ZIG_FIXTURES = callPackage ./zig-fixtures.nix { };
    PATCHELF_REFERENCE = "${callPackage ./patchelf-ref.nix { }}/bin/patchelf";
    installPhase = "touch $out";
  }
  // toolchainFixtures
)
