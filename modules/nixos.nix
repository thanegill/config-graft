# NixOS module: declaratively manage system-level config files an application owns
# and writes to itself, via `environment.managed{Json,Plist,Yaml,Toml}`. The system
# module is shared (./lib/system.nix); this file only wires activation the NixOS
# way, an arbitrary-named activation script per format, run via the activation
# topological sort. The flake applies it with `self` so the default package is this
# flake's own build, so no overlay or `PATH` entry is needed.
{ self }:
{
  config,
  lib,
  pkgs,
  ...
}:
import ./lib/system.nix {
  inherit config lib pkgs;
  defaultPackage = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
  activationWiring =
    { format, text }:
    {
      system.activationScripts.${format.optionName} = {
        deps = [ "etc" ];
        inherit text;
      };
    };
}
