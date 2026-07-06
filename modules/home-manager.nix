# home-manager module: declaratively manage config files an application owns and
# writes to itself, via `home.managed{Json,Plist,Yaml,Toml}`. Declares the options
# and wires each format's activation directly (the format descriptors and the
# shared entry/DESIRED/assertion helpers come from ./lib), so this module reads top
# to bottom. The flake applies it with `self` so the default package is this flake's
# own build, so no overlay or `PATH` entry is needed.
#
# Snapshot rationale: config-graft needs the settings we applied *last* time as
# BASE. We take them from the previous home-manager generation, not mutable state.
# Each entry's DESIRED is linked as a `home.file` (the snapshot), so the prior
# generation's copy is reachable at `$oldGenPath/home-files/<snapshot>` on the next
# switch (GC-safe: the old generation is a GC root). Unset $oldGenPath on the first
# switch -> no pruning.
{ self }:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  configGraftLib = import ./lib;

  # `configGraftLib.formats` is keyed by name; add the key back as `name`.
  formats = lib.mapAttrsToList (name: spec: spec // { inherit name; }) configGraftLib.formats;

  defaultPackage = self.packages.${pkgs.stdenv.hostPlatform.system}.default;

  homeTargetExample = {
    json = ".config/app/config.json";
    yaml = ".config/app/config.yaml";
    toml = ".config/app/config.toml";
    plist = "Library/Preferences/com.example.app.plist";
  };

  managedOption =
    format:
    lib.mkOption {
      default = { };
      description = ''
        ${format.name} configuration files that an application owns and writes to, but
        which home-manager should partially manage. Each entry deep-merges its
        {option}`settings` into {option}`target` during activation (via
        {command}`config-graft`), keeping keys the app wrote that aren't managed here
        and pruning keys dropped from Nix. All activation is handled by this module.
      '';
      type = configGraftLib.entryType {
        inherit
          lib
          pkgs
          defaultPackage
          format
          ;
        targetOption = lib.mkOption {
          type = lib.types.str;
          example = homeTargetExample.${format.name};
          description = "Path of the managed ${format.name} file, relative to the home directory.";
        };
        cfprefsdDescription = ''
          macOS preference domain backing this plist (e.g. `com.example.app` for
          {file}`~/Library/Preferences/com.example.app.plist`). When set,
          {option}`settings` are reconciled through `cfprefsd` instead of by
          editing {option}`target` in place: {command}`defaults export` reads the
          live domain, {command}`config-graft` deep-merges and prunes, and
          {command}`defaults import` writes the result back, so the change isn't
          lost to cfprefsd's in-memory cache. A running app keeps its own copy of
          the prefs, so quit it before switching and relaunch afterwards.
          {option}`target` is ignored in this mode.
        '';
      };
    };

  # All activation is handled here (unlike the system modules, which hand a text
  # blob to their platform's activation phase).
  mkScript =
    {
      format,
      snapshotRel,
      entry,
      desired,
    }:
    let
      target = "${config.home.homeDirectory}/${entry.target}";
    in
    # Previous run's settings = the prior generation's snapshot. Unset on the first
    # switch, so PREVIOUS stays empty.
    ''
      _prev=""
      if [[ -v oldGenPath && -e "$oldGenPath/home-files/${snapshotRel}" ]]; then
        _prev="$oldGenPath/home-files/${snapshotRel}"
        verboseEcho "Pruning against previous snapshot $_prev"
      fi
    ''
    + configGraftLib.mkConfigGraftActivationScript {
      inherit
        lib
        format
        entry
        desired
        target
        ;
    };

  # Per-format active entries, each with its snapshot path, DESIRED, and script.
  byFormat = map (
    format:
    let
      active = lib.filterAttrs (
        _: entry: entry.settings != { } || entry.source != null
      ) config.home.${format.optionName};

      entries = lib.mapAttrs (
        name: entry:
        let
          snapshotRel = ".local/state/home-manager/managed-${format.name}/${name}.${format.fileExtension}";
          desired = configGraftLib.mkDesired {
            inherit
              lib
              pkgs
              format
              name
              entry
              ;
          };
        in
        {
          inherit snapshotRel desired;
          script = mkScript {
            inherit
              format
              snapshotRel
              entry
              desired
              ;
          };
        }
      ) active;
    in
    {
      inherit format active entries;
    }
  ) formats;

  # Managed targets (relative to $HOME) across all formats, for the overlap guard.
  managedTargets = lib.concatMap (x: lib.mapAttrsToList (_: entry: entry.target) x.active) byFormat;
in
{
  options.home = builtins.listToAttrs (
    map (format: {
      name = format.optionName;
      value = managedOption format;
    }) formats
  );

  config = {
    # Link each DESIRED as a `home.file` snapshot, readable as BASE next switch.
    home.file = builtins.listToAttrs (
      lib.concatMap (
        x: lib.mapAttrsToList (_: e: lib.nameValuePair e.snapshotRel { source = e.desired; }) x.entries
      ) byFormat
    );

    # One activation entry per format (a static key), defined only when it has entries.
    home.activation = builtins.listToAttrs (
      map (x: {
        name = x.format.optionName;
        value = lib.mkIf (x.active != { }) (
          lib.hm.dag.entryAfter [ "writeBoundary" ] (
            lib.concatStringsSep "\n" (lib.mapAttrsToList (_: e: e.script) x.entries)
          )
        );
      }) byFormat
    );

    assertions =
      lib.concatMap (
        x:
        configGraftLib.mkAssertions {
          inherit lib pkgs;
          inherit (x) format active;
          parent = "home";
        }
      ) byFormat
      # config-graft reconciles a mutable file in place; `home.file` symlinks an
      # immutable store path. The same path can't be both, so reject the overlap.
      ++ map (path: {
        assertion = !(config.home.file ? ${path});
        message = ''
          `home.file."${path}"` and a config-graft `managed<Format>` entry both
          manage `${path}`. `home.file` creates an immutable store symlink, while
          config-graft reconciles a mutable file in place; declare it in one.
        '';
      }) managedTargets;
  };
}
