# Shared assembly for the home-manager, NixOS, and nix-darwin modules, split into
# logical files and re-exported here.
#
# Each platform module (`home-manager.nix`, `nixos.nix`, `darwin.nix`) supplies a
# *platform* record and calls `build`. There is no dispatch on module type. The
# home-manager platform has a single consumer, so it lives in `home-manager.nix`
# rather than here; `systemPlatform` is shared by the two system modules (which
# differ only in how activation is wired, supplied by each).
#
# - `formats.nix`         per-format descriptors (JSON / YAML / TOML / plist)
# - `build.nix`           assemble `{ options; config; }` for one platform
# - `system-platform.nix` the platform record shared by nixos.nix / darwin.nix
# - `cfprefsd.nix`        the shared macOS `cfprefsdDomain` option
#
# A platform record provides: `parent` (the option attrset, e.g. "home"),
# `targetOption`/`targetConfig`/`extraEntryOptions`/`optionDescription`,
# `snapshotRel`/`targetPath`, `recordSnapshots`, `wireActivation`, and `mkScript`.
# `build` itself asserts any entry setting `cfprefsdDomain` is on a Darwin host,
# since that option drives macOS-only tooling on both home and system platforms.
#
# Two recursion traps the module system punishes via `_module.freeformType`, both
# avoided by construction: `build` is called from inside a normal
# `{ config, lib, pkgs, ... }:` module (platform chosen by the file, never from
# `pkgs`, so config keys never depend on `pkgs`); and every config fragment a
# platform returns keeps a *static* top-level key whose value aggregates over the
# active entries (e.g. `home.file` built from them), so `mkIf`'s body shape is
# fixed and `active` (hence `config`) is not forced while keys are determined.
{
  inherit (import ./formats.nix) formats;
  build = import ./build.nix;
  systemPlatform = import ./system-platform.nix;
  cfprefsdDomainOption = import ./cfprefsd.nix;
}
