{
  description = "kubie: kubectx/kubens alternative with remote provider discovery";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = nixpkgs.legacyPackages.${system};
        cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);
        kubie = pkgs.rustPlatform.buildRustPackage {
          pname = "kubie";
          version = cargoToml.package.version;
          src = pkgs.lib.fileset.toSource {
            root = ./.;
            fileset = pkgs.lib.fileset.unions [
              ./Cargo.toml
              ./Cargo.lock
              ./src
              ./completion
            ];
          };
          cargoLock.lockFile = ./Cargo.lock;

          # Nix manages upgrades, so leave out the self-update feature.
          buildNoDefaultFeatures = true;
          buildFeatures = [ "remote" ];

          nativeBuildInputs = [ pkgs.installShellFiles ];
          postInstall = ''
            installShellCompletion --bash --name kubie completion/kubie.bash
            installShellCompletion --zsh --name _kubie completion/kubie.zsh
            installShellCompletion --fish --name kubie.fish completion/kubie.fish
          '';

          meta = {
            description = cargoToml.package.description;
            homepage = cargoToml.package.homepage;
            license = pkgs.lib.licenses.zlib;
            mainProgram = "kubie";
          };
        };
      in
      {
        packages.default = kubie;
        packages.kubie = kubie;
        apps.default = flake-utils.lib.mkApp { drv = kubie; };
        devShells.default = pkgs.mkShell {
          inputsFrom = [ kubie ];
          packages = [ pkgs.rust-analyzer pkgs.clippy pkgs.rustfmt ];
        };
      });
}
