{
  description = "Modify the dynamic linker and RPATH of ELF executables";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  inputs.treefmt-nix.url = "github:numtide/treefmt-nix";
  inputs.treefmt-nix.inputs.nixpkgs.follows = "nixpkgs";

  outputs =
    {
      self,
      nixpkgs,
      treefmt-nix,
    }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];
      forAll = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
      isLinux = system: nixpkgs.lib.hasSuffix "-linux" system;
      treefmtEval = forAll (pkgs: treefmt-nix.lib.evalModule pkgs ./treefmt.nix);
    in
    {
      packages = forAll (pkgs: {
        default = pkgs.callPackage ./package.nix { };
      });

      formatter = forAll (pkgs: treefmtEval.${pkgs.system}.config.build.wrapper);

      checks = forAll (pkgs: {
        formatting = treefmtEval.${pkgs.system}.config.build.check self;
      });

      devShells = forAll (
        pkgs:
        {
          default = pkgs.mkShell {
            packages = [
              pkgs.cargo
              pkgs.rustc
              pkgs.clippy
              pkgs.rustfmt
              pkgs.zig
              pkgs.cargo-mutants
            ];
          };
        }
        // nixpkgs.lib.optionalAttrs (isLinux pkgs.stdenv.hostPlatform.system) {
          # Cross-arch execution shell: qemu-user plus the foreign `hello`
          # binaries and their runtime glibc, surfaced as env vars the
          # tests/cross_exec.rs harness consumes. Linux only; entering it
          # realises the cross packages, so it is kept separate from `default`.
          cross =
            let
              aarch64 = pkgs.pkgsCross.aarch64-multiplatform;
              ppc64 = pkgs.pkgsCross.ppc64;
            in
            pkgs.mkShell {
              packages = [
                pkgs.cargo
                pkgs.rustc
                pkgs.qemu
              ];
              CROSS_AARCH64_HELLO = "${aarch64.hello}/bin/hello";
              CROSS_AARCH64_GLIBC = "${aarch64.glibc}/lib";
              CROSS_PPC64_HELLO = "${ppc64.hello}/bin/hello";
              CROSS_PPC64_GLIBC = "${ppc64.glibc}/lib";
            };
        }
      );
    };
}
