# Shared attributes for the home-manager, NixOS, and nix-darwin modules.
#
# Each of those is its own module file (`home-manager.nix`, `nixos.nix`,
# `darwin.nix`) that supplies a *platform* record and calls `build` here.
# There is no dispatch on module type in this file -- it holds the per-format
# `specs`, the format-aware option/DESIRED helpers and assembly (`build`), and
# `systemPlatform` (shared by the two system modules, which differ only in how
# activation is wired -- supplied by each). The home-manager platform has a single
# consumer, so it lives in `home-manager.nix` rather than here.
#
# A platform record provides: `parent` (the option attrset, e.g. "home"),
# `targetOption`/`targetConfig`/`extraEntryOptions`/`optionDescription`,
# `snapshotRel`/`targetPath`, `recordSnapshots`, `wireActivation`, and `mkScript`.
#
# Two recursion traps the module system punishes via `_module.freeformType`, both
# avoided by construction: `build` is called from inside a normal
# `{ config, lib, pkgs, ... }:` module (platform chosen by the file, never from
# `pkgs`, so config keys never depend on `pkgs`); and every config fragment a
# platform returns keeps a *static* top-level key whose value aggregates over the
# active entries (e.g. `home.file` built from them), so `mkIf`'s body shape is
# fixed and `active` (hence `config`) is not forced while keys are determined.

let
  # Static, pkgs-free per-format descriptors. `kind` picks freeform (a
  # `pkgs.formats` generator) vs. plist (`lib.generators.toPlist`).
  specs = [
    {
      fmt = "json";
      ext = "json";
      optionName = "managedJson";
      kind = "freeform";
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
      settingsExample = {
        NSGlobalDomain.AppleShowAllExtensions = true;
        recentItems = [
          "a"
          "b"
        ];
      };
    }
  ];

  isFreeform = spec: spec.kind == "freeform";

  systemTargetExample = {
    json = "/etc/app/config.json";
    yaml = "/etc/app/config.yaml";
    toml = "/etc/app/config.toml";
    plist = "/Library/Preferences/com.example.app.plist";
  };

  # Assemble `{ options; config; }` for one platform. Called from inside a module,
  # so `config`/`lib`/`pkgs` come from that module's own arguments.
  build =
    {
      config,
      lib,
      pkgs,
      platform,
      defaultPackage,
    }:
    let
      inherit (lib)
        mkOption
        mkIf
        mkMerge
        types
        filterAttrs
        mapAttrsToList
        concatStringsSep
        optionalAttrs
        literalExpression
        ;

      # DESIRED store path for one entry: a `pkgs.formats` generator for freeform
      # formats (overridable per entry via `format`), `lib.generators.toPlist` for
      # plist.
      mkDesired =
        spec: name: entry:
        if isFreeform spec then
          entry.format.generate "managed-${spec.fmt}-${name}.${spec.ext}" entry.settings
        else
          pkgs.writeText "managed-plist-${name}.plist" (
            lib.generators.toPlist { escape = true; } entry.settings
          );

      # plist shares JSON's value model (minus null, which it cannot emit).
      settingsType =
        spec: entryConfig:
        if isFreeform spec then entryConfig.format.type else (pkgs.formats.json { }).type;

      formatOption =
        spec:
        mkOption {
          type = types.raw;
          default = pkgs.formats.${spec.fmt} { };
          defaultText = literalExpression "pkgs.formats.${spec.fmt} { }";
          description = ''
            A `pkgs.formats`-style generator (providing `type` and `generate`) used
            to build {option}`settings`. Override to use a validating format.
          '';
        };

      mkSubmodule =
        spec:
        types.submodule (
          { name, config, ... }:
          {
            options = {
              target = platform.targetOption spec;

              package = mkOption {
                type = types.package;
                default = defaultPackage;
                defaultText = literalExpression "config-graft.packages.\${system}.default";
                description = ''
                  The config-graft package used to reconcile this entry. Defaults
                  to this flake's own build, so no overlay or `PATH` entry is
                  needed -- the activation script calls it by store path.
                '';
              };

              settings = mkOption {
                type = settingsType spec config;
                default = { };
                example = spec.settingsExample;
                description = "Freeform ${spec.fmt} data reconciled into {option}`target`. Empty disables the entry.";
              };
            }
            // optionalAttrs (isFreeform spec) { format = formatOption spec; }
            // platform.extraEntryOptions spec;

            config = platform.targetConfig name;
          }
        );

      perSpec =
        spec:
        let
          cfg = config.${platform.parent}.${spec.optionName};
          active = filterAttrs (_: entry: entry.settings != { }) cfg;

          # Per-entry data, built from `active`. It must only feed config *values*,
          # never config *keys* (see the header note on `_module.freeformType`).
          entries = mapAttrsToList (name: entry: rec {
            snapshotRel = platform.snapshotRel spec name;
            desired = mkDesired spec name entry;
            script = platform.mkScript {
              inherit
                spec
                entry
                desired
                snapshotRel
                ;
              target = platform.targetPath config entry;
            };
          }) active;

          activationText = concatStringsSep "\n" (map (e: e.script) entries);
        in
        {
          inherit (spec) optionName;

          option = mkOption {
            default = { };
            description = platform.optionDescription spec;
            type = types.attrsOf (mkSubmodule spec);
          };

          config = mkIf (active != { }) (mkMerge [
            (platform.recordSnapshots { inherit spec entries; })
            (platform.wireActivation {
              inherit spec;
              text = activationText;
            })
          ]);
        };

      built = map perSpec specs;
    in
    {
      options.${platform.parent} = builtins.listToAttrs (
        map (m: {
          name = m.optionName;
          value = m.option;
        }) built
      );

      config = mkMerge (map (m: m.config) built);
    };

  # Platform shared by the NixOS and nix-darwin modules. Snapshot rationale: each
  # generation embeds its DESIRED into the toplevel closure (via
  # `system.systemBuilderCommands`); during activation `/run/current-system` still
  # points at the previous generation (the symlink swap is activation's last step
  # on both platforms), so the prior snapshot is reachable at
  # `/run/current-system/<snapshot>`. Absent on the first switch (or for a newly
  # added entry) -> no pruning. No `cfprefsd` path: cfprefsd domains are per-user,
  # not a system concern. Each system module supplies `wireActivation`.
  systemPlatform = lib: {
    parent = "environment";

    targetOption =
      spec:
      lib.mkOption {
        type = lib.types.str;
        example = systemTargetExample.${spec.fmt};
        description = "Absolute path of the managed ${spec.fmt} file.";
      };

    targetConfig = _: { };

    extraEntryOptions = _: { };

    optionDescription = spec: ''
      System-level ${spec.fmt} configuration files that an application owns and
      writes to, but which should be partially managed declaratively. Each entry
      deep-merges its {option}`settings` into the absolute {option}`target` during
      system activation (via {command}`config-graft`), keeping keys the app wrote
      that aren't managed here and pruning keys dropped from Nix.
    '';

    snapshotRel = spec: name: "config-graft/managed-${spec.fmt}/${name}.${spec.ext}";

    targetPath = _config: entry: entry.target;

    # Static top-level key (`system.systemBuilderCommands`, a `lines` value); the
    # per-entry links are concatenated into it.
    recordSnapshots =
      { entries, ... }:
      {
        system.systemBuilderCommands = lib.concatStrings (
          map (e: ''
            mkdir -p "$(dirname "$out/${e.snapshotRel}")"
            ln -s ${e.desired} $out/${e.snapshotRel}
          '') entries
        );
      };

    # No home-manager `run`/`_i` helpers at the system level, so plain bash.
    mkScript =
      {
        spec,
        entry,
        desired,
        snapshotRel,
        target,
      }:
      ''
        _target=${lib.escapeShellArg target}
        echo "config-graft: reconciling managed ${spec.fmt} file $_target"
        _prev="/run/current-system/${snapshotRel}"
        [[ -e "$_prev" ]] || _prev=""
        ${lib.getExe entry.package} --format ${spec.fmt} "$_target" ${desired} "$_prev"
      '';
  };

in
{
  inherit
    specs
    build
    systemPlatform
    ;
}
