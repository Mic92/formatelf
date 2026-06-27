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
      packages = forAll (pkgs: {
        default = pkgs.callPackage ./package.nix { };
      });

      formatter = forAll (pkgs: treefmtEval.${pkgs.system}.config.build.wrapper);

      checks = forAll (pkgs: {
        formatting = treefmtEval.${pkgs.system}.config.build.check self;

        clippy = pkgs.callPackage ./clippy.nix {
          formatelf = self.packages.${pkgs.system}.default;
        };
      });

      devShells = forAll (
        pkgs:
        builtins.removeAttrs (pkgs.callPackage ./shells.nix { }) [
          "override"
          "overrideDerivation"
        ]
      );
    };
}
