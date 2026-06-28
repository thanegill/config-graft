# NixOS module: declaratively manage system-level config files an application owns
# and writes to itself, via `environment.managed{Json,Plist,Yaml,Toml}`. The
# per-format specs, assembly, and system engine are shared (./lib.nix); this file
# only wires activation the NixOS way -- an arbitrary-named activation script per
# format, run via the activation topological sort.
{
  config,
  lib,
  pkgs,
  ...
}:

let
  cg = import ./lib.nix;

  engine = cg.systemEngine lib // {
    wireActivation =
      { spec, text }:
      {
        system.activationScripts.${spec.optionName} = {
          deps = [ "etc" ];
          inherit text;
        };
      };
  };
in
cg.build {
  inherit
    config
    lib
    pkgs
    engine
    ;
}
