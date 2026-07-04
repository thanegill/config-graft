# home-manager module: declaratively manage config files an application owns and
# writes to itself, via `home.managed{Json,Plist,Yaml,Toml}`. The per-format specs
# and assembly are shared (./lib.nix); the home-manager platform has a single
# consumer, so it is defined here (mirroring how the system modules supply their
# own activation wiring).
{
  config,
  lib,
  pkgs,
  ...
}:

let
  cg = import ./lib.nix;

  homeTargetExample = {
    json = ".config/app/config.json";
    yaml = ".config/app/config.yaml";
    toml = ".config/app/config.toml";
    plist = "Library/Preferences/com.example.app.plist";
  };

  # Snapshot rationale: config-graft needs the settings we applied *last* time as
  # BASE. We take them from the previous home-manager generation, not mutable state
  # -- each entry's DESIRED is linked as a `home.file` (the snapshot), so the prior
  # generation's copy is reachable at `$oldGenPath/home-files/<snapshot>` on the
  # next switch (GC-safe: the old generation is a GC root). Unset $oldGenPath on the
  # first switch -> no pruning.
  platform = {
    parent = "home";

    targetOption =
      spec:
      lib.mkOption {
        type = lib.types.str;
        example = homeTargetExample.${spec.fmt};
        description = "Path of the managed ${spec.fmt} file, relative to the home directory.";
      };

    targetConfig = name: { target = lib.mkDefault name; };

    extraEntryOptions =
      spec:
      lib.optionalAttrs (spec.kind == "plist") {
        cfprefsdDomain = lib.mkOption {
          type = lib.types.nullOr lib.types.str;
          default = null;
          example = "com.example.app";
          description = ''
            macOS preference domain backing this plist (e.g. `com.example.app` for
            {file}`~/Library/Preferences/com.example.app.plist`). When set,
            {option}`settings` are reconciled through `cfprefsd` instead of by
            editing {option}`target` in place: {command}`defaults export` reads the
            live domain, {command}`config-graft` deep-merges and prunes, and
            {command}`defaults import` writes the result back -- so the change isn't
            lost to cfprefsd's in-memory cache. A running app keeps its own copy of
            the prefs, so quit it before switching and relaunch afterwards.
            {option}`target` is ignored in this mode.
          '';
        };
      };

    optionDescription = spec: ''
      ${spec.fmt} configuration files that an application owns and writes to, but
      which home-manager should partially manage. Each entry deep-merges its
      {option}`settings` into {option}`target` during activation (via
      {command}`config-graft`), keeping keys the app wrote that aren't managed here
      and pruning keys dropped from Nix. All activation is handled by this module.
    '';

    snapshotRel = spec: name: ".local/state/home-manager/managed-${spec.fmt}/${name}.${spec.ext}";

    targetPath = config: entry: "${config.home.homeDirectory}/${entry.target}";

    # Static top-level key (`home.file`); the per-entry snapshot links are its
    # value.
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
            _domain=${lib.escapeShellArg entry.cfprefsdDomain}
            _i "Reconciling managed plist domain %s" "$_domain"

            # Read the live domain through cfprefsd (not the on-disk file, which
            # may be staler than cfprefsd's cache). Empty/missing domain -> start
            # from an empty plist.
            _live=$(mktemp)
            /usr/bin/defaults export "$_domain" "$_live" 2>/dev/null || true
            [[ -s "$_live" ]] || /usr/bin/plutil -create xml1 "$_live"

            # Graft our settings into the live state in place, then push the merged
            # result back through cfprefsd so it adopts it.
            run ${lib.getExe entry.package} \
              --format plist \
              "$_live" \
              ${desired} \
              "$_prev"
            run /usr/bin/defaults import "$_domain" "$_live"
            rm -f "$_live"
          ''
        else
          ''
            _target=${lib.escapeShellArg target}
            _i "Reconciling managed ${spec.fmt} file %s" "$_target"

            run ${lib.getExe entry.package} \
              --format ${spec.fmt} \
              "$_target" \
              ${desired} \
              "$_prev"
          ''
      );
  };
in
cg.build {
  inherit
    config
    lib
    pkgs
    platform
    ;
}
