{
  description = "Three-way reconcile for app-owned JSON, plist, and YAML files";

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
          pname = "json-apply";
          version = "0.0.3";
          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;
          meta.mainProgram = "json-apply";
        };
      });

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
