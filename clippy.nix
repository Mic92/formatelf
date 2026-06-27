{
  clippy,
  formatelf,
}:
# Lint with the package's vendored deps and build env. installPhase is replaced
# without runHook, so the patchelf-symlink postInstall is skipped.
formatelf.overrideAttrs (old: {
  pname = "formatelf-clippy";
  nativeBuildInputs = (old.nativeBuildInputs or [ ]) ++ [ clippy ];
  buildPhase = ''
    runHook preBuild
    cargo clippy --all-targets --release -- -D warnings
    runHook postBuild
  '';
  installPhase = "touch $out";
})
