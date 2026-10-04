{
  description = "crystal: one terminal for all your coding agents";

  # nix run github:gabalexander/crystal       crystal, built from source
  # nix profile install github:gabalexander/crystal
  # nix develop                                a shell with the toolchain

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "aarch64-darwin"
        "x86_64-darwin"
        "aarch64-linux"
        "x86_64-linux"
      ];
      forEach = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
      cargo = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).package;
    in
    {
      packages = forEach (pkgs: rec {
        crystal = pkgs.rustPlatform.buildRustPackage {
          pname = "crystal";
          inherit (cargo) version;
          src = pkgs.lib.cleanSource ./.;
          cargoLock.lockFile = ./Cargo.lock;
          nativeBuildInputs = [ pkgs.installShellFiles ];
          # The tests start daemons and terminals of their own, which the
          # build's sandbox doesn't allow; CI runs them.
          doCheck = false;
          postInstall = pkgs.lib.optionalString (pkgs.stdenv.buildPlatform.canExecute pkgs.stdenv.hostPlatform) ''
            installShellCompletion --cmd crystal \
              --bash <($out/bin/crystal completions bash) \
              --zsh <($out/bin/crystal completions zsh) \
              --fish <($out/bin/crystal completions fish)
          '';
          meta = {
            inherit (cargo) description;
            homepage = cargo.repository;
            license = pkgs.lib.licenses.mit;
            mainProgram = "crystal";
            platforms = pkgs.lib.platforms.darwin ++ pkgs.lib.platforms.linux;
          };
        };
        default = crystal;
      });

      apps = forEach (pkgs: {
        default = {
          type = "app";
          program = "${self.packages.${pkgs.stdenv.hostPlatform.system}.crystal}/bin/crystal";
        };
      });

      devShells = forEach (pkgs: {
        default = pkgs.mkShell {
          inputsFrom = [ self.packages.${pkgs.stdenv.hostPlatform.system}.crystal ];
          packages = [
            pkgs.clippy
            pkgs.rustfmt
            pkgs.rust-analyzer
          ];
        };
      });
    };
}
