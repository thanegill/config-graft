# NixOS / nix-darwin wrapper: `environment.managed{Json,Plist,Yaml,Toml}`. All the
# logic lives in the shared engine; this selects the system flavour for one
# platform. See ./lib.nix.
#
# `platform` ("nixos" | "darwin") is bound at import time by the flake's
# `nixosModules.default` / `darwinModules.default`. It must be a static argument,
# not derived from `pkgs`: the two platforms differ only in how the activation
# script is wired, which decides config *keys*, and reading that from
# `pkgs.stdenv` would force `pkgs` during `_module.freeformType` -- before `pkgs`
# (itself derived from `config`) is ready -- and recurse.
platform:

(import ./lib.nix).mkModule platform
