# Parameterized by the host platform ("nixos" or "darwin"), bound at import time
# by the flake's `nixosModules.default` / `darwinModules.default`. It must be a
# static argument, NOT derived from `pkgs`: the only place the two platforms
# differ is how the activation script is wired (a named script on NixOS, a fixed
# phase on nix-darwin), and that choice decides which config *keys* exist. If it
# were read from `pkgs.stdenv.hostPlatform`, the module system would force `pkgs`
# while computing `_module.freeformType` -- before `pkgs` (itself derived from
# `config`) is ready -- and evaluation would recurse.
platform:

{
  config,
  lib,
  pkgs,
  ...
}:

# Shared NixOS / nix-darwin module: declaratively manage system-level config
# files that an application owns and writes to itself. One option per format
# config-graft speaks -- `environment.managedJson`, `environment.managedPlist`,
# `environment.managedYaml`, `environment.managedToml` -- each an attrset of
# entries. Every entry reconciles its `settings` into the live (absolute)
# `target` during system activation (via config-graft): the app's own keys are
# kept, and keys dropped from Nix are pruned. The system analogue of the
# home-manager `home.managed*` options.
#
# Pruning needs the previously-applied settings as BASE. We get them GC-safely
# from the *previous system generation* rather than mutable runtime state: each
# generation embeds its DESIRED snapshots into the toplevel closure (via
# `system.systemBuilderCommands`), and during activation `/run/current-system`
# still points to the previous generation (the symlink swap is activation's last
# step, on both NixOS and nix-darwin). So the previous generation's snapshot is
# reachable at `/run/current-system/<snapshot>`. Absent on the first switch, or
# for a newly added entry -> no pruning; present thereafter -> prune. (On a fresh
# boot /run is a tmpfs without the symlink yet, so a boot-time activation skips
# pruning and re-applies on the next switch -- harmless, since DESIRED is still
# deep-merged.)
#
# config-graft needs `config-graft` on PATH; each entry's `package` option
# supplies it (default `pkgs.config-graft`, e.g. via this flake's
# `overlays.default`).

let
  inherit (lib)
    mkIf
    mkMerge
    mkOption
    mkAfter
    types
    filterAttrs
    mapAttrsToList
    concatStrings
    escapeShellArg
    getExe
    mkPackageOption
    literalExpression
    ;

  # Static, pkgs-free per-format descriptors. pkgs-dependent bits are resolved in
  # `mkManaged`'s module body -- never in `imports` (below), or evaluation
  # recurses.
  specs = [
    {
      fmt = "json";
      ext = "json";
      optionName = "managedJson";
      kind = "freeform";
      targetExample = "/etc/app/config.json";
      settingsExample = {
        theme = "dark";
        editor.fontSize = 14;
      };
    }
    {
      fmt = "yaml";
      ext = "yaml";
      optionName = "managedYaml";
      kind = "freeform";
      targetExample = "/etc/app/config.yaml";
      settingsExample = {
        theme = "dark";
        plugins = [ "git" ];
      };
    }
    {
      fmt = "toml";
      ext = "toml";
      optionName = "managedToml";
      kind = "freeform";
      targetExample = "/etc/app/config.toml";
      settingsExample = {
        theme = "dark";
        editor.font_size = 14;
      };
    }
    {
      fmt = "plist";
      ext = "plist";
      optionName = "managedPlist";
      kind = "plist";
      targetExample = "/Library/Preferences/com.example.app.plist";
      settingsExample = {
        NSGlobalDomain.AppleShowAllExtensions = true;
      };
    }
  ];

  # The generic engine. Returns the option declaration and config fragment for one
  # format; the four are merged below. `pkgs`/`config` come from this module's own
  # arguments -- never from an `imports` entry, which would make the module
  # system's freeform-type check force `pkgs` before it is available and recurse.
  mkManaged =
    spec:
    let
      cfg = config.environment.${spec.optionName};

      isFreeform = spec.kind == "freeform";

      # Path of an entry's snapshot, relative to a system generation's root.
      snapshotRel = name: "config-graft/managed-${spec.fmt}/${name}.${spec.ext}";

      active = filterAttrs (_: entry: entry.settings != { }) cfg;

      mkDesired =
        name: entry:
        if isFreeform then
          entry.format.generate "managed-${spec.fmt}-${name}.${spec.ext}" entry.settings
        else
          pkgs.writeText "managed-plist-${name}.plist" (
            lib.generators.toPlist { escape = true; } entry.settings
          );

      extraOptions =
        _:
        if isFreeform then
          {
            format = mkOption {
              type = types.raw;
              default = pkgs.formats.${spec.fmt} { };
              defaultText = literalExpression "pkgs.formats.${spec.fmt} { }";
              description = ''
                A `pkgs.formats`-style generator (providing `type` and `generate`)
                used to build {option}`settings`. Override to use a validating
                format.
              '';
            };
          }
        else
          { };

      settingsType = entry: if isFreeform then entry.format.type else (pkgs.formats.json { }).type;

      submodule = types.submodule (
        { config, ... }:
        {
          options = {
            target = mkOption {
              type = types.str;
              example = spec.targetExample;
              description = "Absolute path of the managed ${spec.fmt} file.";
            };

            package = mkPackageOption pkgs "config-graft" { };

            settings = mkOption {
              type = settingsType config;
              default = { };
              example = spec.settingsExample;
              description = "Freeform ${spec.fmt} data reconciled into {option}`target`. Empty disables the entry.";
            };
          }
          // extraOptions config;
        }
      );

      # One reconcile invocation. No home-manager `run`/`_i` helpers exist at the
      # system level, so this is plain bash. BASE is the previous generation's
      # snapshot if present (see header), else empty (no pruning).
      entryScript =
        name: entry:
        let
          desired = mkDesired name entry;
        in
        ''
          _target=${escapeShellArg entry.target}
          echo "config-graft: reconciling managed ${spec.fmt} file $_target"
          _prev="/run/current-system/${snapshotRel name}"
          [[ -e "$_prev" ]] || _prev=""
          ${getExe entry.package} --format ${spec.fmt} "$_target" ${desired} "$_prev"
        '';

      activationText = concatStrings (mapAttrsToList entryScript active);
    in
    {
      inherit (spec) optionName;

      option = mkOption {
        default = { };
        description = ''
          System-level ${spec.fmt} configuration files that an application owns
          and writes to, but which should be partially managed declaratively.
          Each entry deep-merges its {option}`settings` into the absolute
          {option}`target` during system activation (via
          {command}`config-graft`), keeping keys the app wrote that aren't managed
          here and pruning keys dropped from Nix.
        '';
        type = types.attrsOf submodule;
      };

      config = mkIf (active != { }) (mkMerge [
        {
          # Embed each entry's DESIRED into this generation's closure, so the
          # *next* activation can read it back from /run/current-system as BASE.
          system.systemBuilderCommands = ''
            mkdir -p $out/config-graft/managed-${spec.fmt}
          ''
          + concatStrings (
            mapAttrsToList (name: entry: ''
              ln -s ${mkDesired name entry} $out/${snapshotRel name}
            '') active
          );
        }

        # NixOS runs arbitrary-named activation scripts via a topological sort;
        # nix-darwin only runs a fixed set of phases, so we append to one of them.
        (
          if platform == "darwin" then
            { system.activationScripts.postActivation.text = mkAfter activationText; }
          else
            {
              system.activationScripts.${spec.optionName} = {
                deps = [ "etc" ];
                text = activationText;
              };
            }
        )
      ]);
    };

  managed = map mkManaged specs;
in
{
  options.environment = builtins.listToAttrs (
    map (m: {
      name = m.optionName;
      value = m.option;
    }) managed
  );

  config = lib.mkMerge (map (m: m.config) managed);
}
