{
  description = "Yaffle CLI";

  inputs = {
    nixpkgs.url = "https://flakehub.com/f/NixOS/nixpkgs/0.1";
    systems.url = "github:nix-systems/default";
  };

  outputs = { self, nixpkgs, systems, ... }:
    let
      forEachSystem = nixpkgs.lib.genAttrs (import systems);
    in {
      packages = forEachSystem (system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          yaffle-cli = pkgs.callPackage ./nix/yaffle-cli.nix {
            inherit pkgs;
            lib = pkgs.lib;
            src = ./.;
          };
        in {
          default = yaffle-cli;
          inherit yaffle-cli;
        }
      );

      apps = forEachSystem (system:
        let
          yaffle-cli = self.packages.${system}.yaffle-cli;
        in {
          default = {
            type = "app";
            program = "${yaffle-cli}/bin/yaffle";
          };
          yaffle = {
            type = "app";
            program = "${yaffle-cli}/bin/yaffle";
          };
          yaffle-outputs = {
            type = "app";
            program = "${yaffle-cli}/bin/yaffle-outputs";
          };
        }
      );
    };
}
