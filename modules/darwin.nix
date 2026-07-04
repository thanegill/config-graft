# nix-darwin module: declaratively manage system-level config files an application
# owns and writes to itself, via `environment.managed{Json,Plist,Yaml,Toml}`. The
# per-format specs, assembly, and system platform are shared (./lib.nix); this
# file only wires activation the nix-darwin way -- appended to the fixed
# `postActivation` phase, since nix-darwin runs only its predefined phases.
{
  config,
  lib,
  pkgs,
  ...
}:

let
  cg = import ./lib.nix;

  platform = cg.systemPlatform lib // {
    wireActivation =
      { text, ... }:
      {
        system.activationScripts.postActivation.text = lib.mkAfter text;
      };
  };
in
cg.build {
  inherit
    config
    lib
    pkgs
    platform
    ;
}
