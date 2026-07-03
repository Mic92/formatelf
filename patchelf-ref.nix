# Reference patchelf for the differential test oracle. Its extension flags
# (--build-resolution-cache, --rename-dynamic-symbols) are not yet in nixpkgs,
# so the fork is pinned as a fixed-output fetch instead of a flake input.
{
  stdenv,
  fetchFromGitHub,
  autoreconfHook,
}:
stdenv.mkDerivation {
  pname = "patchelf-reference";
  version = "0.19.0-unstable-2024-bd64dc7";
  src = fetchFromGitHub {
    owner = "NixOS";
    repo = "patchelf";
    rev = "bd64dc797410234cd70eb19ed7cd914e7f2b0afb";
    hash = "sha256-e+wuH9u4accYUS7Ll99LGK0EbuFffmDvLnWtly0koqA=";
  };
  nativeBuildInputs = [ autoreconfHook ];
  doCheck = false;
}
