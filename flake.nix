{
  description = "Three-way reconcile for app-owned JSON, plist, YAML, and TOML files";

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
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      packages = forAllSystems (pkgs: {
        default = pkgs.rustPlatform.buildRustPackage {
          pname = "config-graft";
          version = "0.0.3";
          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;
          meta.mainProgram = "config-graft";
        };
      });

      # Adds `config-graft` to a consumer's package set, which is where the
      # home-manager module looks for the binary by default.
      overlays.default = final: _prev: {
        config-graft = self.packages.${final.stdenv.hostPlatform.system}.default;
      };

      # Declarative wrappers exposing `managed{Json,Plist,Yaml,Toml}` -- one per
      # format config-graft reconciles. `home.*` for home-manager,
      # `environment.*` for NixOS / nix-darwin. The three modules share one engine
      # (modules/lib.nix). All need `config-graft` on PATH: apply
      # `overlays.default`, or set each entry's `package`. Each is exposed under
      # both `default` and the named `config-graft` attribute.
      homeManagerModules = {
        config-graft = ./modules/managed.nix;
        default = ./modules/managed.nix;
      };
      nixosModules = {
        config-graft = ./modules/managed-nixos.nix;
        default = ./modules/managed-nixos.nix;
      };
      darwinModules = {
        config-graft = ./modules/managed-darwin.nix;
        default = ./modules/managed-darwin.nix;
      };

      # All the tools needed to build, test, lint, and format the crate.
      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          packages = [
            pkgs.cargo
            pkgs.rustc
            pkgs.clippy
            pkgs.rustfmt
            pkgs.rust-analyzer
            pkgs.cargo-llvm-cov
          ];
          RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
          # cargo-llvm-cov needs the LLVM tools matching rustc (both LLVM 21 here).
          LLVM_COV = "${pkgs.llvmPackages.llvm}/bin/llvm-cov";
          LLVM_PROFDATA = "${pkgs.llvmPackages.llvm}/bin/llvm-profdata";
        };
      });

      formatter = forAllSystems (pkgs: pkgs.nixfmt-rfc-style);
    };
}
