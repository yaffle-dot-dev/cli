{
  description = "Yaffle CLI";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, rust-overlay, ... }:
    let
      systems = [
        "x86_64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];
      version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).workspace.package.version;
      forAllSystems = function:
        nixpkgs.lib.genAttrs systems (system:
          function (import nixpkgs {
            inherit system;
            overlays = [ rust-overlay.overlays.default ];
          }));
    in
    {
      packages = forAllSystems (pkgs:
        let
          rust = pkgs.rust-bin.stable."1.95.0".minimal.override {
            extensions = [ "clippy" "rustfmt" ];
            targets = pkgs.lib.optionals pkgs.stdenv.isLinux [ "x86_64-unknown-linux-musl" ];
          };
          rustPlatform = pkgs.makeRustPlatform {
            cargo = rust;
            rustc = rust;
          };
          muslCc = pkgs.pkgsCross.musl64.stdenv.cc;
          muslLinker = "${muslCc}/bin/${muslCc.targetPrefix}cc";
          yaffle = rustPlatform.buildRustPackage ({
            pname = "yaffle";
            inherit version;
            src = pkgs.lib.fileset.toSource {
              root = ./.;
              fileset = pkgs.lib.fileset.unions [
                ./Cargo.lock
                ./Cargo.toml
                ./crates
                ./testdata
              ];
            };
            cargoLock.lockFile = ./Cargo.lock;
            cargoBuildFlags = [ "-p" "yaffle-cli" ];
            doCheck = false;
            strictDeps = true;

            postInstall = ''
              $out/bin/yaffle --version
              $out/bin/yaffle --help >/dev/null
            '';

            postFixup = pkgs.lib.optionalString pkgs.stdenv.isDarwin ''
              install_name_tool -change \
                ${pkgs.libiconv}/lib/libiconv.2.dylib \
                /usr/lib/libiconv.2.dylib \
                $out/bin/yaffle
            '';

            meta = {
              description = "Environment orchestration for Terraform and OpenTofu";
              homepage = "https://yaffle.dev";
              license = pkgs.lib.licenses.mit;
              mainProgram = "yaffle";
              platforms = systems;
            };
          } // pkgs.lib.optionalAttrs pkgs.stdenv.isLinux {
            nativeBuildInputs = [ muslCc ];
            CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER = muslLinker;
            CC_x86_64_unknown_linux_musl = muslLinker;

            buildPhase = ''
              runHook preBuild
              cargo build --offline --release --target x86_64-unknown-linux-musl -p yaffle-cli
              runHook postBuild
            '';

            installPhase = ''
              runHook preInstall
              install -Dm755 \
                target/x86_64-unknown-linux-musl/release/yaffle \
                $out/bin/yaffle
              runHook postInstall
            '';
          });
        in
        {
          inherit yaffle;
          default = yaffle;
        });

      apps = forAllSystems (pkgs: {
        default = {
          type = "app";
          program = "${self.packages.${pkgs.system}.yaffle}/bin/yaffle";
        };
        yaffle = self.apps.${pkgs.system}.default;
      });

      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          packages = [
            pkgs.opentofu
            pkgs.rust-bin.stable."1.95.0".default
          ];
        };
      });
    };
}
