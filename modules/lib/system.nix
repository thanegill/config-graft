# Linear system module for the NixOS and nix-darwin wrappers, which differ only in
# `activationWiring` (how the reconcile script is placed into activation) and pass
# their own. Declares `environment.managed{Json,Plist,Yaml,Toml}` and wires each
# format's activation directly, so the whole system module reads top to bottom.
#
# Snapshot rationale: each generation embeds its DESIRED into the toplevel closure
# (via `system.systemBuilderCommands`); during activation `/run/current-system`
# still points at the previous generation (the symlink swap is activation's last
# step on both platforms), so the prior snapshot is reachable at
# `/run/current-system/<snapshot>`. Absent on the first switch (or for a newly added
# entry) -> no pruning. Plist entries may set `cfprefsdDomain` to reconcile through
# `cfprefsd` (as root, so system/global domains) instead of editing the file;
# a build-time assertion requires a Darwin host.
let
  common = import ./common.nix;
  inherit (import ./formats.nix) formats;

  systemTargetExample = {
    json = "/etc/app/config.json";
    yaml = "/etc/app/config.yaml";
    toml = "/etc/app/config.toml";
    plist = "/Library/Preferences/com.example.app.plist";
  };
in
{
  config,
  lib,
  pkgs,
  defaultPackage,
  activationWiring,
}:
let
  managedOption =
    format:
    lib.mkOption {
      default = { };
      description = ''
        System-level ${format.format} configuration files that an application owns and
        writes to, but which should be partially managed declaratively. Each entry
        deep-merges its {option}`settings` into the absolute {option}`target` during
        system activation (via {command}`config-graft`), keeping keys the app wrote
        that aren't managed here and pruning keys dropped from Nix.
      '';
      type = common.entryType {
        inherit
          lib
          pkgs
          defaultPackage
          format
          ;
        targetOption = lib.mkOption {
          type = lib.types.str;
          example = systemTargetExample.${format.format};
          description = "Absolute path of the managed ${format.format} file. Defaults to the attribute name.";
        };
        cfprefsdDescription = ''
          macOS preference domain backing this system plist (e.g. `com.example.app`
          for {file}`/Library/Preferences/com.example.app.plist`). When set,
          {option}`settings` are reconciled through `cfprefsd` during system
          activation instead of by editing {option}`target` in place:
          {command}`defaults export` reads the live domain, {command}`config-graft`
          deep-merges and prunes, and {command}`defaults import` writes it back, so
          the change isn't lost to cfprefsd's in-memory cache. Runs as root, so it
          targets system/global domains under {file}`/Library/Preferences`.
          {option}`target` is ignored in this mode.
        '';
      };
    };

  # No home-manager `run`/`_i` helpers at the system level, so plain bash. Previous
  # run's settings = the prior generation's snapshot, reachable at /run/current-system
  # until activation's final symlink swap. Empty on the first switch -> no pruning.
  mkScript =
    format: snapshotRel: entry: desired:
    ''
      _prev="/run/current-system/${snapshotRel}"
      [[ -e "$_prev" ]] || _prev=""
    ''
    + (
      if format.kind == "plist" && entry.cfprefsdDomain != null then
        ''
          _domain=${lib.escapeShellArg entry.cfprefsdDomain}
          echo "config-graft: reconciling managed plist domain $_domain"

          # Read the live domain through cfprefsd (not the on-disk file, which may
          # be staler than cfprefsd's cache). Empty/missing domain -> start from an
          # empty plist.
          _live=$(mktemp)
          /usr/bin/defaults export "$_domain" "$_live" 2>/dev/null || true
          [[ -s "$_live" ]] || /usr/bin/plutil -create xml1 "$_live"

          # Graft our settings into the live state, then push it back through
          # cfprefsd so it adopts the merged result.
          ${lib.getExe entry.package} --format plist "$_live" ${desired} "$_prev"
          /usr/bin/defaults import "$_domain" "$_live"
          rm -f "$_live"
        ''
      else
        ''
          _target=${lib.escapeShellArg entry.target}
          echo "config-graft: reconciling managed ${format.format} file $_target"
          ${lib.getExe entry.package} --format ${format.format} "$_target" ${desired} "$_prev"
        ''
    );

  managedConfig =
    format:
    let
      active = lib.filterAttrs (
        _: entry: entry.settings != { } || entry.source != null
      ) config.environment.${format.optionName};

      entries = lib.mapAttrs (
        name: entry:
        let
          snapshotRel = "config-graft/managed-${format.format}/${name}.${format.fileExtension}";
          desired = common.mkDesired lib pkgs format name entry;
        in
        {
          inherit snapshotRel desired;
          script = mkScript format snapshotRel entry desired;
        }
      ) active;
    in
    lib.mkIf (active != { }) (
      lib.mkMerge [
        # Embed each DESIRED into the toplevel closure at its snapshot path.
        {
          system.systemBuilderCommands = lib.concatStrings (
            lib.mapAttrsToList (_: e: ''
              mkdir -p "$(dirname "$out/${e.snapshotRel}")"
              ln -s ${e.desired} $out/${e.snapshotRel}
            '') entries
          );
        }
        (activationWiring {
          inherit format;
          text = lib.concatStringsSep "\n" (lib.mapAttrsToList (_: e: e.script) entries);
        })
        {
          assertions = common.mkAssertions {
            inherit
              lib
              pkgs
              format
              active
              ;
            parent = "environment";
          };
        }
      ]
    );
in
{
  options.environment = builtins.listToAttrs (
    map (format: {
      name = format.optionName;
      value = managedOption format;
    }) formats
  );

  config = lib.mkMerge (map managedConfig formats);
}
