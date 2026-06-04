{
  description = "sec-grep";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-parts.url = "github:hercules-ci/flake-parts";
    treefmt-nix = {
      url = "github:numtide/treefmt-nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    inputs@{
      self,
      nixpkgs,
      flake-parts,
      treefmt-nix,
      ...
    }:
    flake-parts.lib.mkFlake { inherit inputs; } {
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
      ];

      imports = [
        treefmt-nix.flakeModule
      ];

      perSystem =
        {
          config,
          self',
          inputs',
          pkgs,
          system,
          ...
        }:
        let
          src = ./.;
          manifest = (pkgs.lib.importTOML "${src}/Cargo.toml").workspace.package;
          secGrep =
            with pkgs;
            rustPlatform.buildRustPackage {
              inherit src;
              name = "sec-grep";
              version = manifest.version;
              cargoLock = {
                lockFile = ./Cargo.lock;
              };
              doCheck = false;
            };

        in
        {
          # auto formatting
          treefmt.config = {
            projectRootFile = "flake.nix";
            programs = {
              yamlfmt.enable = true;
              rustfmt.enable =  true;
              nixfmt.enable = true;
            };
          };

          packages = {
            inherit secGrep;
            default = secGrep;
          };

          devShells.default = pkgs.mkShell {
            buildInputs =
              secGrep.buildInputs
              ++ (with pkgs; [
                cargo
              ]);
          };
        };
    };
}
