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
          version = "0.1.1";
          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;
          meta.mainProgram = "config-graft";
        };
      });

      # Optional: adds `config-graft` to a consumer's package set, e.g. to get the
      # CLI interactively. The modules do NOT need it -- they run this flake's own
      # build directly (see below).
      overlays.default = final: _prev: {
        config-graft = self.packages.${final.stdenv.hostPlatform.system}.default;
      };

      # Declarative wrappers exposing `managed{Json,Plist,Yaml,Toml}` -- one per
      # byte format config-graft reconciles -- plus `managedDirectory` for the
      # `directory` subcommand (reconcile a whole source tree). `home.*` for home-manager,
      # `environment.*` for NixOS / nix-darwin. The three modules share one
      # assembly (modules/lib). Each is applied with `self` so entries
      # default to this flake's build (the activation script calls it by store
      # path) -- no overlay or `PATH` entry needed; override per entry via
      # `package`. Each is exposed under both `default` and the named
      # `config-graft` attribute.
      homeManagerModules = {
        config-graft = import ./modules/home-manager.nix { inherit self; };
        default = import ./modules/home-manager.nix { inherit self; };
      };
      nixosModules = {
        config-graft = import ./modules/nixos.nix { inherit self; };
        default = import ./modules/nixos.nix { inherit self; };
      };
      darwinModules = {
        config-graft = import ./modules/darwin.nix { inherit self; };
        default = import ./modules/darwin.nix { inherit self; };
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
