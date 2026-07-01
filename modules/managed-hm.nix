# home-manager module: declaratively manage config files an application owns and
# writes to itself, via `home.managed{Json,Plist,Yaml,Toml}`. The per-format
# specs, assembly, and the home-manager engine are shared (./lib.nix); this file
# just selects that engine.
{
  config,
  lib,
  pkgs,
  ...
}:

let
  cg = import ./lib.nix;
in
cg.build {
  inherit config lib pkgs;
  engine = cg.homeEngine lib;
}
