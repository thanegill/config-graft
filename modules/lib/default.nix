# Shared pieces for the home-manager, NixOS, and nix-darwin modules. There is no
# generic engine and no per-platform dispatch record: each module writes its own
# `options`/`config` linearly and pulls the format-agnostic pieces from here.
#
# - `formats.nix`   per-format descriptors, keyed by name (JSON / YAML / TOML / plist)
# - `common.nix`    the entry submodule type, DESIRED store path, reconcile script,
#                   and assertions (re-exported here; the modules import `./lib`)
# - `system.nix`    the linear system module (imports `./lib`; nixos.nix /
#                   darwin.nix are thin wrappers passing their activation wiring)
#
# Recursion trap the module system punishes via `_module.freeformType`, avoided by
# construction: a module's config keys never depend on `pkgs` (the module is chosen
# by the file, not by `pkgs.stdenv`), and every config fragment keeps a *static*
# top-level key whose value aggregates over the active entries (e.g. `home.file`
# built from them), so `mkIf`'s body shape is fixed and `active` (hence `config`) is
# not forced while keys are determined.
(import ./common.nix)
// {
  # `formats.nix` is keyed by format name; re-export as a list of descriptors that
  # carry the name (as `name`), so consumers iterate without re-adding it.
  formats = builtins.attrValues (
    builtins.mapAttrs (name: spec: spec // { inherit name; }) (import ./formats.nix)
  );
}
