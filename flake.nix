{
  description = "Tuna TUI - a terminal music player";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };

  outputs =
    { self, nixpkgs }:
    let
      supportedSystems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];
      forAllSystems = nixpkgs.lib.genAttrs supportedSystems;
    in
    {
      packages = forAllSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          inherit (pkgs) lib;
        in
        rec {
          # Quoted: the package attr carries the binary's hyphenated name.
          "tuna-tui" = pkgs.rustPlatform.buildRustPackage {
            pname = "tuna-tui";
            version = "0.4.0";

            src = lib.cleanSource ./.;
            cargoLock.lockFile = ./Cargo.lock;

            nativeBuildInputs =
              lib.optionals pkgs.stdenv.hostPlatform.isLinux [
                pkgs.pkg-config
              ]
              # The engine oracle tests spawn the real `ffmpeg` binary during
              # the check phase (which runs at BUILD time); with strictDeps the
              # test env only has nativeBuildInputs on PATH.
              ++ [ pkgs.ffmpeg ];

            buildInputs =
              lib.optionals pkgs.stdenv.hostPlatform.isLinux [
                pkgs.alsa-lib
                pkgs.openssl
              ]
              ++ lib.optionals pkgs.stdenv.hostPlatform.isDarwin [
                pkgs.libiconv
              ]
              # The engine oracle tests spawn the real `ffmpeg` binary; it must
              # be present in the check phase (nix flake check runs cargo test).
              ++ [ pkgs.ffmpeg ];

            strictDeps = true;

            meta = {
              description = "A lean, beautiful terminal music player";
              homepage = "https://github.com/shrijit37/tuna-tui";
              license = lib.licenses.mit;
              mainProgram = "tuna-tui";
              platforms = supportedSystems;
            };
          };

          default = self.packages.${system}."tuna-tui";
        }
      );

      apps = forAllSystems (system: {
        default = {
          type = "app";
          program = "${self.packages.${system}."tuna-tui"}/bin/tuna-tui";
        };
      });

      devShells = forAllSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          default = pkgs.mkShell {
            inputsFrom = [ self.packages.${system}."tuna-tui" ];
            packages = with pkgs; [
              cargo
              clippy
              rust-analyzer
              rustc
              rustfmt
            ];
          };
        }
      );
    };
}