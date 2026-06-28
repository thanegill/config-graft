{
  config,
  lib,
  pkgs,
  ...
}:

# home-manager module: declaratively manage config files that an application owns
# and writes to itself. One option per format config-graft speaks --
# `home.managedJson`, `home.managedPlist`, `home.managedYaml`, `home.managedToml`
# -- each an attrset of entries. Every entry reconciles its `settings` into the
# live `target` during activation (via config-graft): the app's own keys are
# kept, and keys dropped from Nix are pruned. All the activation/snapshot
# plumbing lives here, written once over a generic engine, so consumers only
# declare intent.
#
# config-graft needs `config-graft` on PATH; each entry's `package` option
# supplies it (default `pkgs.config-graft`, e.g. via this flake's
# `overlays.default`).

let
  inherit (lib)
    mkIf
    mkOption
    mkDefault
    types
    mapAttrs'
    nameValuePair
    filterAttrs
    escapeShellArg
    getExe
    mkPackageOption
    literalExpression
    ;

  # Static, pkgs-free per-format descriptors. The pkgs-dependent bits (generators,
  # settings type, plist vs freeform behaviour) are resolved inside `mkManaged`'s
  # module body, keyed on `kind`/`fmt` -- never here, because `imports` (below)
  # must not touch `pkgs`/`config` or evaluation recurses.
  specs = [
    {
      fmt = "json";
      ext = "json";
      optionName = "managedJson";
      kind = "freeform";
      targetExample = ".config/app/config.json";
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
      targetExample = ".config/app/config.yaml";
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
      targetExample = ".config/app/config.toml";
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
      targetExample = "Library/Preferences/com.example.app.plist";
      settingsExample = {
        NSGlobalDomain.AppleShowAllExtensions = true;
        recentItems = [
          "a"
          "b"
        ];
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
      cfg = config.home.${spec.optionName};

      isFreeform = spec.kind == "freeform";

      # To prune keys we've stopped managing, config-graft needs to know what we
      # managed *last* time. We get that from the previous home-manager
      # generation rather than any mutable runtime state: each generation is a
      # store path ($oldGenPath / $newGenPath at activation time), and a
      # generation only exposes a file at a predictable
      # `$genPath/home-files/<path>` if that file is a home.file. So we link the
      # generated settings as a home.file at the path below -- the "snapshot".
      # Its source is the very same store path passed to config-graft as DESIRED,
      # so the snapshot is just a stable, addressable handle to "the settings this
      # generation applied".
      #
      # On the next switch the activation reads `$oldGenPath/home-files/<snapshot>`,
      # which resolves to the *previous* generation's snapshot -- exactly the
      # settings we applied last time. This is GC-safe: the old generation is a GC
      # root and transitively keeps that store path alive. On the first ever switch
      # $oldGenPath is unset, so there is no previous and nothing is pruned.
      snapshotPath = name: ".local/state/home-manager/managed-${spec.fmt}/${name}.${spec.ext}";

      targetPath = entry: "${config.home.homeDirectory}/${entry.target}";

      active = filterAttrs (_: entry: entry.settings != { }) cfg;

      # Build DESIRED. Freeform formats use a `pkgs.formats` generator (overridable
      # per entry via `format`); plist uses `lib.generators.toPlist`.
      mkDesired =
        name: entry:
        if isFreeform then
          entry.format.generate "managed-${spec.fmt}-${name}.${spec.ext}" entry.settings
        else
          pkgs.writeText "managed-plist-${name}.plist" (
            lib.generators.toPlist { escape = true; } entry.settings
          );

      # Per-entry options beyond the common target/package/settings.
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
          {
            cfprefsdDomain = mkOption {
              type = types.nullOr types.str;
              default = null;
              example = "com.example.app";
              description = ''
                macOS preference domain backing this plist (e.g. `com.example.app`
                for {file}`~/Library/Preferences/com.example.app.plist`). When set,
                {option}`settings` are reconciled through `cfprefsd` instead of by
                editing {option}`target` in place: {command}`defaults export` reads
                the live domain, {command}`config-graft` deep-merges and prunes,
                and {command}`defaults import` writes the result back -- so the
                change isn't lost to cfprefsd's in-memory cache. A running app
                keeps its own copy of the prefs, so quit it before switching and
                relaunch afterwards. {option}`target` is ignored in this mode.
              '';
            };
          };

      # plist shares JSON's value model (minus null, which it cannot emit).
      settingsType = entry: if isFreeform then entry.format.type else (pkgs.formats.json { }).type;

      submodule = types.submodule (
        { name, config, ... }:
        {
          options = {
            target = mkOption {
              type = types.str;
              example = spec.targetExample;
              description = "Path of the managed ${spec.fmt} file, relative to the home directory.";
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

          config.target = mkDefault name;
        }
      );

      # Common prelude: resolve the prior generation's snapshot as BASE so
      # config-graft can prune. Unset on the first switch, so PREVIOUS stays empty.
      prunePrelude = name: ''
        _prev=""
        if [[ -v oldGenPath && -e "$oldGenPath/home-files/${snapshotPath name}" ]]; then
          _prev="$oldGenPath/home-files/${snapshotPath name}"
          verboseEcho "Pruning against previous snapshot $_prev"
        fi
      '';

      mkScript =
        name: entry:
        let
          desired = mkDesired name entry;
        in
        prunePrelude name
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
              _target=${escapeShellArg (targetPath entry)}
              _i "Reconciling managed ${spec.fmt} file %s" "$_target"

              run ${getExe entry.package} \
                --format ${spec.fmt} \
                "$_target" \
                ${desired} \
                "$_prev"
            ''
        );
    in
    {
      inherit (spec) optionName;

      option = mkOption {
        default = { };
        description = ''
          ${spec.fmt} configuration files that an application owns and writes to,
          but which home-manager should partially manage. Each entry deep-merges
          its {option}`settings` into {option}`target` during activation (via
          {command}`config-graft`), keeping keys the app wrote that aren't managed
          here and pruning keys dropped from Nix. All activation is handled by
          this module.
        '';
        type = types.attrsOf submodule;
      };

      config = mkIf (active != { }) {
        # Stash each entry's applied settings in the generation, so the next
        # switch can diff against it (see snapshotPath above).
        home.file = mapAttrs' (
          name: entry: nameValuePair (snapshotPath name) { source = mkDesired name entry; }
        ) active;

        home.activation = mapAttrs' (
          name: entry:
          nameValuePair "${spec.optionName}-${name}" (
            lib.hm.dag.entryAfter [ "writeBoundary" ] (mkScript name entry)
          )
        ) active;
      };
    };

  managed = map mkManaged specs;
in
{
  options.home = builtins.listToAttrs (
    map (m: {
      name = m.optionName;
      value = m.option;
    }) managed
  );

  config = lib.mkMerge (map (m: m.config) managed);
}
