{
  lib,
  makeSetupHook,
  formatelf,
  stdenv,
}:
# Drop-in equivalent of nixpkgs' autoPatchelfHook, backed by auto-formatelf.
# The bintools dependency supplies $NIX_BINTOOLS, from which auto-formatelf
# reads the dynamic linker and libc.
makeSetupHook {
  name = "auto-formatelf-hook";
  propagatedBuildInputs = [
    formatelf
    stdenv.cc.bintools
  ];
  meta.platforms = lib.platforms.linux;
} ./auto-formatelf-hook.sh
