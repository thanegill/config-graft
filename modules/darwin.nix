# nix-darwin module: declaratively manage system-level config files an application
# owns and writes to itself, via `environment.managed{Json,Plist,Yaml,Toml}`. The
# per-format specs, assembly, and system platform are shared (./lib); this
# file only wires activation the nix-darwin way, appended to the fixed
# `postActivation` phase, since nix-darwin runs only its predefined phases. The
# flake applies this with `self` so the default package comes from this flake's
# own build, so no overlay or `PATH` entry is needed.
{ self }:
{
  config,
  lib,
  pkgs,
  ...
}:

let
  shared = import ./lib;

  platform = shared.systemPlatform lib // {
    wireActivation =
      { text, ... }:
      {
        system.activationScripts.postActivation.text = lib.mkAfter text;
      };
  };
in
shared.build {
  inherit
    config
    lib
    pkgs
    platform
    ;
  defaultPackage = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
}
