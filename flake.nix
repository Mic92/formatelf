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
      treefmtEval = forAll (pkgs: treefmt-nix.lib.evalModule pkgs ./treefmt.nix);

    in
    {
      packages = forAll (
        pkgs:
        let
          formatelf = pkgs.callPackage ./package.nix { };
        in
        {
          default = formatelf;
          inherit formatelf;
        }
        // pkgs.lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
          autoFormatelfHook = pkgs.callPackage ./hook.nix { inherit formatelf; };
        }
      );

      # Complex real-world consumers of the auto-formatelf setup hook. Kept out
      # of `checks` so a plain `nix flake check` does not pull large binaries.
      examples = nixpkgs.lib.genAttrs [ "x86_64-linux" ] (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          discord = pkgs.callPackage ./examples/discord.nix {
            autoFormatelfHook = self.packages.${system}.autoFormatelfHook;
          };
        }
      );

      formatter = forAll (pkgs: treefmtEval.${pkgs.stdenv.hostPlatform.system}.config.build.wrapper);

      checks = forAll (
        pkgs:
        let
          inherit (pkgs) lib;
          prefix = p: lib.mapAttrs' (n: lib.nameValuePair "${p}-${n}");
        in
        {
          formatting = treefmtEval.${pkgs.stdenv.hostPlatform.system}.config.build.check self;

          clippy = pkgs.callPackage ./clippy.nix {
            formatelf = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
          };
        }
        // lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
          # Suite over zig fixtures, a reference patchelf and the TC_* fixtures.
          # Clang is dropped: uncached on aarch64 (builds LLVM from source) and
          # its ELF output is covered by gcc. Linux only: the fixtures are ELF.
          tests = pkgs.callPackage ./test.nix {
            formatelf = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
            toolchainFixtures =
              builtins.removeAttrs self.legacyPackages.${pkgs.stdenv.hostPlatform.system}.toolchainFixtures
                [
                  "TC_CLANG_GLIBC"
                  "TC_CLANG_MUSL"
                ];
          };
        }
        // prefix "package" self.packages.${pkgs.stdenv.hostPlatform.system}
        // prefix "devShell" (
          # The toolchains shell realises every compiler; keep it out of CI.
          builtins.removeAttrs self.devShells.${pkgs.stdenv.hostPlatform.system} [ "toolchains" ]
        )
      );

      # Shared TC_* fixture set, exposed once per system so the toolchains shell
      # and the tests check consume identical fixtures. filterAttrs drops the
      # override/overrideDerivation callPackage adds; values stay lazy.
      legacyPackages = forAll (pkgs: {
        toolchainFixtures = nixpkgs.lib.filterAttrs (n: _: nixpkgs.lib.hasPrefix "TC_" n) (
          pkgs.callPackage ./toolchain-fixtures.nix { }
        );
      });

      devShells = forAll (
        pkgs:
        builtins.removeAttrs
          (pkgs.callPackage ./shells.nix {
            toolchainFixtures = self.legacyPackages.${pkgs.stdenv.hostPlatform.system}.toolchainFixtures;
          })
          [
            "override"
            "overrideDerivation"
          ]
      );
    };
}
