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
        // prefix "package" self.packages.${pkgs.stdenv.hostPlatform.system}
        // prefix "devShell" self.devShells.${pkgs.stdenv.hostPlatform.system}
      );

      devShells = forAll (
        pkgs:
        builtins.removeAttrs (pkgs.callPackage ./shells.nix { }) [
          "override"
          "overrideDerivation"
        ]
      );
    };
}
