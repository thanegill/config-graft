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
#
# That covers keys dropped from an entry that stays declared. An entry that is
# removed -- its `settings` emptied, or the entry deleted -- has no reconcile left,
# so each generation also links a manifest of what it manages, and an
# unconditional activation step, ahead of the entries, prunes what the previous
# manifest lists and this generation no longer manages (`mkPruneScript`).
{ self }:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  configGraftLib = import ./lib;
  inherit (configGraftLib) formats;

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
        and pruning keys dropped from Nix. Removing an entry, or emptying its
        {option}`settings`, prunes every key it last applied on the next switch. All
        activation is handled by this module.
      '';
      type = configGraftLib.entryType {
        inherit
          lib
          pkgs
          format
          ;
        parent = "home";
        defaultPackage = config.home.managed.package;
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
          the prefs, so quit it before switching and relaunch afterwards: name
          its bundle id in {option}`whileRunning.restart` to have that done, or
          its process in {option}`whileRunning.skip` to leave the domain alone
          while it runs. {option}`target` is ignored in this mode.
        '';
      };
    };

  # The `directory` subcommand option: reconcile a whole `source` tree into a target
  # directory. Distinct from the byte formats (no `settings`), so it lives beside
  # the format loop rather than in it.
  directoryOption = lib.mkOption {
    default = { };
    description = ''
      Directory *trees* an application owns and writes to, which home-manager should
      partially manage. Each entry reconciles its {option}`source` tree into
      {option}`target` during activation (via {command}`config-graft
      directory`): files the app created are kept, files dropped from {option}`source`
      are pruned, and per-file mode/xattrs (and, unless {option}`noOwner`, owner) are
      reconciled. Removing an entry prunes every file it last applied on the next
      switch. All activation is handled by this module.
    '';
    type = configGraftLib.directoryEntryType {
      inherit lib pkgs;
      parent = "home";
      defaultPackage = config.home.managed.package;
      targetOption = lib.mkOption {
        type = lib.types.str;
        example = ".config/app";
        description = "Path of the managed directory, relative to the home directory.";
      };
      sourceDescription = ''
        The directory tree reconciled into {option}`target` (a path, or a derivation
        that builds one). Its files' modes become the desired modes, so build it with
        the modes you want; a plain store copy is root-owned and read-only, so on this
        (non-root) activation set {option}`noOwner`.
      '';
    };
  };

  # Where the previous generation's `home.file` links live during activation, as
  # a shell expression; meaningful only when `oldGenPath` is set.
  prevGenRoot = "$oldGenPath/home-files";

  # Sets `_prev` to the previous generation's snapshot of an entry, the BASE. Absent
  # on the first switch (or for a newly added entry), so `_prev` stays empty.
  prevSnapshot = snapshotRel: ''
    _prev=""
    if [[ -v oldGenPath && -e "${prevGenRoot}/${snapshotRel}" ]]; then
      _prev="${prevGenRoot}/${snapshotRel}"
      verboseEcho "Pruning against previous snapshot $_prev"
    fi
  '';

  # Active directory entries (all of them — `source` is required), each with its
  # snapshot path and reconcile script.
  directoryEntries = lib.mapAttrs (
    name: entry:
    let
      snapshotRel = ".local/state/home-manager/managed-directory/${configGraftLib.safeName name}";
      target = "${config.home.homeDirectory}/${entry.target}";
    in
    {
      inherit snapshotRel;
      source = entry.source;
      unit = configGraftLib.mkDirectoryUnit {
        inherit entry;
        identity = configGraftLib.normalizePath target;
        snapshot = snapshotRel;
      };
      script =
        prevSnapshot snapshotRel
        + configGraftLib.mkDirectoryReconcileScript {
          inherit
            lib
            pkgs
            entry
            target
            ;
        };
    }
  ) config.home.managedDirectory;

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
    prevSnapshot snapshotRel
    + configGraftLib.mkEntryReconcileScript {
      inherit
        lib
        pkgs
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
      active = lib.filterAttrs (_: configGraftLib.isActive) config.home.${format.optionName};

      entries = lib.mapAttrs (
        name: entry:
        let
          snapshotRel = ".local/state/home-manager/managed-${format.name}/${configGraftLib.safeName name}";
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
          unit =
            if format.name == "plist" && entry.cfprefsdDomain != null then
              configGraftLib.mkUnit {
                inherit format entry;
                identity = configGraftLib.domainPlist "${config.home.homeDirectory}/Library/Preferences" entry.cfprefsdDomain;
                snapshot = snapshotRel;
                domain = entry.cfprefsdDomain;
              }
            else
              configGraftLib.mkUnit {
                inherit format entry;
                identity = configGraftLib.normalizePath "${config.home.homeDirectory}/${entry.target}";
                snapshot = snapshotRel;
              };
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

  # After `linkGeneration`, which removes the previous generation's stale
  # `home.file` links: an entry moved from `home.file` would otherwise meet its old
  # read-only store link, which config-graft cannot write.
  activationAfter = [
    "writeBoundary"
    "linkGeneration"
  ];

  # What this generation manages, as the manifest the next switch reads to find
  # what it no longer does. Linked as a `home.file` like the snapshots it points
  # at, so it is in `$oldGenPath/home-files` with them.
  units =
    lib.concatMap (x: lib.mapAttrsToList (_: e: e.unit) x.entries) byFormat
    ++ lib.mapAttrsToList (_: e: e.unit) directoryEntries;

  manifestRel = ".local/state/home-manager/managed-manifest.json";

  # Managed targets (relative to $HOME) across all formats + directories, for the
  # overlap guard.
  managedTargets =
    lib.concatMap (x: lib.mapAttrsToList (_: entry: entry.target) x.active) byFormat
    ++ lib.mapAttrsToList (_: entry: entry.target) config.home.managedDirectory;
in
{
  options.home =
    builtins.listToAttrs (
      map (format: {
        name = format.optionName;
        value = managedOption format;
      }) formats
    )
    // {
      managedDirectory = directoryOption;

      managed.package = lib.mkOption {
        type = lib.types.package;
        default = defaultPackage;
        defaultText = lib.literalExpression "config-graft.packages.\${system}.default";
        description = ''
          Default config-graft package for every `home.managed*` entry. Defaults to this
          flake's own build; override a single entry with its `package` option.
        '';
      };
    };

  config = {
    # Link each DESIRED as a `home.file` snapshot, readable as BASE next switch (a
    # directory DESIRED becomes a symlink to the store tree, which config-graft
    # follows as the BASE root).
    home.file = builtins.listToAttrs (
      lib.concatMap (
        x: lib.mapAttrsToList (_: e: lib.nameValuePair e.snapshotRel { source = e.desired; }) x.entries
      ) byFormat
      ++ lib.mapAttrsToList (
        _: e: lib.nameValuePair e.snapshotRel { source = e.source; }
      ) directoryEntries
      ++ [
        (lib.nameValuePair manifestRel {
          source = pkgs.writeText "config-graft-manifest.json" (configGraftLib.mkManifest units);
        })
      ]
    );

    # One activation entry per format (a static key), defined only when it has entries.
    home.activation = builtins.listToAttrs (
      map (x: {
        name = x.format.optionName;
        value = lib.mkIf (x.active != { }) (
          lib.hm.dag.entryAfter activationAfter (
            lib.concatStringsSep "\n" (lib.mapAttrsToList (_: e: e.script) x.entries)
          )
        );
      }) byFormat
      ++ [
        # Unconditional, unlike the entries: the generation that removes the last
        # entry has nothing else to key it off. Ahead of every entry, so an entry
        # sharing a removed one's target re-applies after the prune.
        {
          name = "managedPrune";
          value =
            lib.hm.dag.entryBetween
              (
                map (format: format.optionName) formats
                ++ [
                  "managedDirectory"
                ]
              )
              activationAfter
              (
                configGraftLib.mkPruneScript {
                  inherit
                    lib
                    pkgs
                    formats
                    manifestRel
                    units
                    ;
                  package = config.home.managed.package;
                  prevRoot = prevGenRoot;
                  hasPrev = "[[ -v oldGenPath ]]";
                  claimed = map (file: configGraftLib.normalizePath "${config.home.homeDirectory}/${file.target}") (
                    lib.filter (file: file.enable) (lib.attrValues config.home.file)
                  );
                  claimedBy = "home.file";
                }
              );
        }
        {
          name = "managedDirectory";
          value = lib.mkIf (directoryEntries != { }) (
            lib.hm.dag.entryAfter activationAfter (
              lib.concatStringsSep "\n" (lib.mapAttrsToList (_: e: e.script) directoryEntries)
            )
          );
        }
      ]
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
      ++ lib.concatMap (
        x:
        configGraftLib.mkRestartAssertions {
          inherit lib pkgs;
          parent = "home";
          inherit (x.format) optionName;
          entries = x.active;
        }
      ) byFormat
      ++ configGraftLib.mkRestartAssertions {
        inherit lib pkgs;
        parent = "home";
        optionName = "managedDirectory";
        entries = config.home.managedDirectory;
      }
      ++ configGraftLib.mkSnapshotAssertions {
        inherit lib;
        entries =
          lib.concatMap (
            x:
            lib.mapAttrsToList (name: e: {
              option = "home.${x.format.optionName}";
              inherit name;
              snapshot = e.snapshotRel;
            }) x.entries
          ) byFormat
          ++ lib.mapAttrsToList (name: e: {
            option = "home.managedDirectory";
            inherit name;
            snapshot = e.snapshotRel;
          }) directoryEntries;
      }
      # config-graft reconciles a mutable file/tree in place; `home.file` symlinks an
      # immutable store path. The same path can't be both, so reject the overlap
      # (byte formats and directories alike).
      ++ map (path: {
        assertion = !(config.home.file ? ${path});
        message = ''
          `home.file."${path}"` and a config-graft `managed<Format>`/`managedDirectory`
          entry both manage `${path}`. `home.file` creates an immutable store symlink,
          while config-graft reconciles a mutable path in place; declare it in one.
        '';
      }) managedTargets;
  };
}
