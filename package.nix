{
  lib,
  rustPlatform,
  # Install a `patchelf` symlink so formatelf can be used as a drop-in
  # replacement; disable to keep only the `formatelf` binary.
  patchelfSymlink ? true,
}:
rustPlatform.buildRustPackage {
  pname = "formatelf";
  version = (lib.importTOML ./Cargo.toml).package.version;
  src = ./.;
  cargoLock.lockFile = ./Cargo.lock;
  # The test suite needs zig-built fixtures and a reference patchelf, neither
  # of which exists in the build sandbox.
  doCheck = false;
  # auto-formatelf is the multi-call personality selected by argv[0]; the
  # patchelf-compatible names ship under the same flag.
  postInstall = ''
    ln -s formatelf $out/bin/auto-formatelf
  '' + lib.optionalString patchelfSymlink ''
    ln -s formatelf $out/bin/patchelf
    ln -s formatelf $out/bin/auto-patchelf
  '';
  meta = {
    description = "Modify the dynamic linker and RPATH of ELF executables";
    license = lib.licenses.mit;
    mainProgram = "formatelf";
    platforms = lib.platforms.unix;
  };
}
