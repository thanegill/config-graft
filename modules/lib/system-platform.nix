# Platform shared by the NixOS and nix-darwin modules. Snapshot rationale: each
# generation embeds its DESIRED into the toplevel closure (via
# `system.systemBuilderCommands`); during activation `/run/current-system` still
# points at the previous generation (the symlink swap is activation's last step
# on both platforms), so the prior snapshot is reachable at
# `/run/current-system/<snapshot>`. Absent on the first switch (or for a newly
# added entry) -> no pruning. Plist entries may set `cfprefsdDomain` to reconcile
# through `cfprefsd` (as root, so system/global domains) instead of editing the
# file; `build` asserts a Darwin host. Each system module supplies `wireActivation`.
let
  cfprefsdDomainOption = import ./cfprefsd.nix;

  systemTargetExample = {
    json = "/etc/app/config.json";
    yaml = "/etc/app/config.yaml";
    toml = "/etc/app/config.toml";
    plist = "/Library/Preferences/com.example.app.plist";
  };
in
lib: {
  parent = "environment";

  targetOption =
    spec:
    lib.mkOption {
      type = lib.types.str;
      example = systemTargetExample.${spec.fmt};
      description = "Absolute path of the managed ${spec.fmt} file. Defaults to the attribute name.";
    };

  targetConfig = name: { target = lib.mkDefault name; };

  extraEntryOptions =
    spec:
    lib.optionalAttrs (spec.kind == "plist") {
      cfprefsdDomain = cfprefsdDomainOption lib ''
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
    # Previous run's settings = the prior generation's snapshot, reachable at
    # /run/current-system until activation's final symlink swap. Empty on the
    # first switch -> no pruning.
    ''
      _prev="/run/current-system/${snapshotRel}"
      [[ -e "$_prev" ]] || _prev=""
    ''
    + (
      if spec.kind == "plist" && entry.cfprefsdDomain != null then
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
          _target=${lib.escapeShellArg target}
          echo "config-graft: reconciling managed ${spec.fmt} file $_target"
          ${lib.getExe entry.package} --format ${spec.fmt} "$_target" ${desired} "$_prev"
        ''
    );
}
