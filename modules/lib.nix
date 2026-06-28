# Shared engine behind the home-manager, NixOS, and nix-darwin wrappers.
#
# All three declare the same thing -- a `managed<Format>` option per format
# config-graft speaks (JSON/plist/YAML/TOML), each an attrset of entries that
# reconcile `settings` into a live file on activation, keeping the app's own keys
# and pruning keys dropped from Nix against the previous generation as BASE. Only
# the *engine* differs: where the options live, how the snapshot is stashed in /
# read back from a generation, and how activation is wired. This file owns the
# format/option logic; each engine (below) supplies the rest.
#
# `mkModule <engine-name>` returns a plain module function -- the wrappers are
# just `(import ./lib.nix).mkModule "home"` etc., so the module system applies
# `config`/`lib`/`pkgs` with its normal laziness. The engine is chosen by a static
# name, never from `pkgs`, so the set of config *keys* never depends on `pkgs`;
# the module system therefore does not force `pkgs` while computing
# `_module.freeformType`, and evaluation does not recurse.

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

  homeTargetExample = {
    json = ".config/app/config.json";
    yaml = ".config/app/config.yaml";
    toml = ".config/app/config.toml";
    plist = "Library/Preferences/com.example.app.plist";
  };

  systemTargetExample = {
    json = "/etc/app/config.json";
    yaml = "/etc/app/config.yaml";
    toml = "/etc/app/config.toml";
    plist = "/Library/Preferences/com.example.app.plist";
  };

  # The module function for one engine. Everything that needs `lib` lives here so
  # the wrappers can stay literal module functions (no manual application that
  # would defeat the module system's arg laziness).
  mkModule =
    engineName:
    {
      config,
      lib,
      pkgs,
      ...
    }:
    let
      inherit (lib)
        mkOption
        mkDefault
        mkIf
        mkMerge
        mkAfter
        types
        filterAttrs
        mapAttrsToList
        optionalAttrs
        concatStrings
        concatStringsSep
        escapeShellArg
        getExe
        mkPackageOption
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
        spec: entry: if isFreeform spec then entry.format.type else (pkgs.formats.json { }).type;

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

      # The per-entry submodule, common to every engine bar the engine-specific
      # `target` option, `target` default, and any extra options (plist's
      # `cfprefsdDomain` under home-manager).
      mkSubmodule =
        engine: spec:
        types.submodule (
          { name, config, ... }:
          {
            options = {
              target = engine.targetOption spec;

              package = mkPackageOption pkgs "config-graft" { };

              settings = mkOption {
                type = settingsType spec config;
                default = { };
                example = spec.settingsExample;
                description = "Freeform ${spec.fmt} data reconciled into {option}`target`. Empty disables the entry.";
              };
            }
            // optionalAttrs (isFreeform spec) { format = formatOption spec; }
            // engine.extraEntryOptions spec;

            config = engine.targetConfig name;
          }
        );

      # home-manager engine. Snapshot rationale: config-graft needs the settings we
      # applied *last* time as BASE. We take them from the previous home-manager
      # generation, not mutable state -- each entry's DESIRED is linked as a
      # `home.file` (the snapshot), so the prior generation's copy is reachable at
      # `$oldGenPath/home-files/<snapshot>` on the next switch (GC-safe: the old
      # generation is a GC root). Unset $oldGenPath on the first switch -> no
      # pruning.
      homeEngine = {
        parent = "home";

        targetOption =
          spec:
          mkOption {
            type = types.str;
            example = homeTargetExample.${spec.fmt};
            description = "Path of the managed ${spec.fmt} file, relative to the home directory.";
          };

        targetConfig = name: { target = mkDefault name; };

        extraEntryOptions =
          spec:
          optionalAttrs (spec.kind == "plist") {
            cfprefsdDomain = mkOption {
              type = types.nullOr types.str;
              default = null;
              example = "com.example.app";
              description = ''
                macOS preference domain backing this plist (e.g. `com.example.app`
                for {file}`~/Library/Preferences/com.example.app.plist`). When set,
                {option}`settings` are reconciled through `cfprefsd` instead of by
                editing {option}`target` in place: {command}`defaults export` reads
                the live domain, {command}`config-graft` deep-merges and prunes, and
                {command}`defaults import` writes the result back -- so the change
                isn't lost to cfprefsd's in-memory cache. A running app keeps its
                own copy of the prefs, so quit it before switching and relaunch
                afterwards. {option}`target` is ignored in this mode.
              '';
            };
          };

        optionDescription = spec: ''
          ${spec.fmt} configuration files that an application owns and writes to,
          but which home-manager should partially manage. Each entry deep-merges
          its {option}`settings` into {option}`target` during activation (via
          {command}`config-graft`), keeping keys the app wrote that aren't managed
          here and pruning keys dropped from Nix. All activation is handled by this
          module.
        '';

        snapshotRel = spec: name: ".local/state/home-manager/managed-${spec.fmt}/${name}.${spec.ext}";

        targetPath = entry: "${config.home.homeDirectory}/${entry.target}";

        # Static top-level key (`home.file`); the per-entry snapshot links are its
        # value. Keeping the key static means the module system can read this
        # fragment's config keys without forcing `active` (which would recurse).
        recordSnapshots =
          { entries, ... }:
          {
            home.file = builtins.listToAttrs (
              map (e: {
                name = e.snapshotRel;
                value.source = e.desired;
              }) entries
            );
          };

        wireActivation =
          { spec, text }:
          {
            home.activation.${spec.optionName} = lib.hm.dag.entryAfter [ "writeBoundary" ] text;
          };

        mkScript =
          {
            spec,
            entry,
            desired,
            snapshotRel,
            target,
          }:
          # Previous run's settings = the prior generation's snapshot. Unset on the
          # first switch, so PREVIOUS stays empty.
          ''
            _prev=""
            if [[ -v oldGenPath && -e "$oldGenPath/home-files/${snapshotRel}" ]]; then
              _prev="$oldGenPath/home-files/${snapshotRel}"
              verboseEcho "Pruning against previous snapshot $_prev"
            fi
          ''
          + (
            if spec.kind == "plist" && entry.cfprefsdDomain != null then
              ''
                _domain=${escapeShellArg entry.cfprefsdDomain}
                _i "Reconciling managed plist domain %s" "$_domain"

                # Read the live domain through cfprefsd (not the on-disk file, which
                # may be staler than cfprefsd's cache). Empty/missing domain -> start
                # from an empty plist.
                _live=$(mktemp)
                /usr/bin/defaults export "$_domain" "$_live" 2>/dev/null || true
                [[ -s "$_live" ]] || /usr/bin/plutil -create xml1 "$_live"

                # Graft our settings into the live state in place, then push the
                # merged result back through cfprefsd so it adopts it.
                run ${getExe entry.package} \
                  --format plist \
                  "$_live" \
                  ${desired} \
                  "$_prev"
                run /usr/bin/defaults import "$_domain" "$_live"
                rm -f "$_live"
              ''
            else
              ''
                _target=${escapeShellArg target}
                _i "Reconciling managed ${spec.fmt} file %s" "$_target"

                run ${getExe entry.package} \
                  --format ${spec.fmt} \
                  "$_target" \
                  ${desired} \
                  "$_prev"
              ''
          );
      };

      # NixOS / nix-darwin engine base. Snapshot rationale: each generation embeds
      # its DESIRED into the toplevel closure (via `system.systemBuilderCommands`);
      # during activation `/run/current-system` still points at the previous
      # generation (the symlink swap is activation's last step on both platforms),
      # so the prior snapshot is reachable at `/run/current-system/<snapshot>`.
      # Absent on the first switch (or for a newly added entry) -> no pruning. No
      # `cfprefsd` path: cfprefsd domains are per-user, not a system concern.
      systemEngineBase = {
        parent = "environment";

        targetOption =
          spec:
          mkOption {
            type = types.str;
            example = systemTargetExample.${spec.fmt};
            description = "Absolute path of the managed ${spec.fmt} file.";
          };

        targetConfig = _: { };

        extraEntryOptions = _: { };

        optionDescription = spec: ''
          System-level ${spec.fmt} configuration files that an application owns and
          writes to, but which should be partially managed declaratively. Each
          entry deep-merges its {option}`settings` into the absolute
          {option}`target` during system activation (via {command}`config-graft`),
          keeping keys the app wrote that aren't managed here and pruning keys
          dropped from Nix.
        '';

        snapshotRel = spec: name: "config-graft/managed-${spec.fmt}/${name}.${spec.ext}";

        targetPath = entry: entry.target;

        # Static top-level key (`system.systemBuilderCommands`, a `lines` value);
        # the per-entry links are concatenated into it. Static key -> the module
        # system reads this fragment's keys without forcing `active`.
        recordSnapshots =
          { entries, ... }:
          {
            system.systemBuilderCommands = concatStrings (
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
            _target=${escapeShellArg target}
            echo "config-graft: reconciling managed ${spec.fmt} file $_target"
            _prev="/run/current-system/${snapshotRel}"
            [[ -e "$_prev" ]] || _prev=""
            ${getExe entry.package} --format ${spec.fmt} "$_target" ${desired} "$_prev"
          '';
      };

      # NixOS runs arbitrary-named activation scripts via a topological sort;
      # nix-darwin only runs a fixed set of phases, so we append to one of them.
      engines = {
        home = homeEngine;

        nixos = systemEngineBase // {
          wireActivation =
            { spec, text }:
            {
              system.activationScripts.${spec.optionName} = {
                deps = [ "etc" ];
                inherit text;
              };
            };
        };

        darwin = systemEngineBase // {
          wireActivation =
            { text, ... }:
            {
              system.activationScripts.postActivation.text = mkAfter text;
            };
        };
      };

      engine = engines.${engineName};

      perSpec =
        spec:
        let
          cfg = config.${engine.parent}.${spec.optionName};
          active = filterAttrs (_: entry: entry.settings != { }) cfg;

          # Per-entry data. This list is built from `active`, so it must only feed
          # config *values*, never config *keys* -- otherwise the module system
          # would force `active` (hence `config`) while determining the option
          # structure and recurse through `_module.freeformType`.
          entries = mapAttrsToList (name: entry: rec {
            snapshotRel = engine.snapshotRel spec name;
            desired = mkDesired spec name entry;
            script = engine.mkScript {
              inherit
                spec
                entry
                desired
                snapshotRel
                ;
              target = engine.targetPath entry;
            };
          }) active;

          activationText = concatStringsSep "\n" (map (e: e.script) entries);
        in
        {
          inherit (spec) optionName;

          option = mkOption {
            default = { };
            description = engine.optionDescription spec;
            type = types.attrsOf (mkSubmodule engine spec);
          };

          # Both fragments have *static* top-level keys (`home.file` /
          # `system.systemBuilderCommands`, and a per-format activation key); the
          # `entries`-derived data lives only in their values. So `mkIf`'s body
          # shape is fixed and the module system never forces `active` to learn the
          # config keys.
          config = mkIf (active != { }) (mkMerge [
            (engine.recordSnapshots { inherit spec entries; })
            (engine.wireActivation {
              inherit spec;
              text = activationText;
            })
          ]);
        };

      built = map perSpec specs;
    in
    {
      options.${engine.parent} = builtins.listToAttrs (
        map (m: {
          name = m.optionName;
          value = m.option;
        }) built
      );

      config = mkMerge (map (m: m.config) built);
    };
in
{
  inherit mkModule;
}
