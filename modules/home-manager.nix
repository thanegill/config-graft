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
# Orphan pruning: per-entry pruning only fires while the entry is still declared --
# a *removed* entry (its `settings` emptied, or the whole entry deleted) drives no
# reconcile, so its last-grafted keys would freeze in the committed file. Each
# generation therefore also links a manifest listing its file entries (target +
# snapshot path); on the next switch an unconditional activation step reads the
# previous generation's manifest and, for every entry no longer declared, reconciles
# its target back to empty against the old snapshot -- pruning exactly what we
# grafted while keeping app/user keys. cfprefsd-backed plist domains and directory
# trees are out of scope (no plain-file target).
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
        and pruning keys dropped from Nix. All activation is handled by this module.
      '';
      type = configGraftLib.entryType {
        inherit
          lib
          pkgs
          format
          ;
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
          the prefs, so quit it before switching and relaunch afterwards.
          {option}`target` is ignored in this mode.
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
      reconciled. All activation is handled by this module.
    '';
    type = configGraftLib.directoryEntryType {
      inherit lib pkgs;
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
      script = ''
        _prev=""
        if [[ -v oldGenPath && -e "$oldGenPath/home-files/${snapshotRel}" ]]; then
          _prev="$oldGenPath/home-files/${snapshotRel}"
          verboseEcho "Pruning against previous snapshot $_prev"
        fi
      ''
      + configGraftLib.mkDirectoryReconcileScript { inherit lib entry target; };
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
    # Previous run's settings = the prior generation's snapshot. Unset on the first
    # switch, so PREVIOUS stays empty.
    ''
      _prev=""
      if [[ -v oldGenPath && -e "$oldGenPath/home-files/${snapshotRel}" ]]; then
        _prev="$oldGenPath/home-files/${snapshotRel}"
        verboseEcho "Pruning against previous snapshot $_prev"
      fi
    ''
    + configGraftLib.mkEntryReconcileScript {
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

  # Managed targets (relative to $HOME) across all formats + directories, for the
  # overlap guard.
  managedTargets =
    lib.concatMap (x: lib.mapAttrsToList (_: entry: entry.target) x.active) byFormat
    ++ lib.mapAttrsToList (_: entry: entry.target) config.home.managedDirectory;

  homeDir = config.home.homeDirectory;

  # Every active byte-format entry that reconciles a plain *file*, flattened to the
  # data the orphan-prune (below) needs: its format, target, snapshot path, and
  # binary flag. cfprefsd-backed plist entries are excluded -- they reconcile a live
  # preference domain, not a file, so there's no target to reconcile back to empty.
  fileEntries = lib.concatMap (
    x:
    lib.mapAttrsToList
      (name: entry: {
        format = x.format.name;
        inherit (entry) target;
        snapshotRel = ".local/state/home-manager/managed-${x.format.name}/${configGraftLib.safeName name}";
        binary = entry.binary or false;
      })
      (lib.filterAttrs (_: entry: !(x.format.name == "plist" && entry.cfprefsdDomain != null)) x.active)
  ) byFormat;

  # An empty DESIRED document per format (empty object / dictionary), reused by the
  # orphan-prune to reconcile a removed entry's target back to empty.
  emptyDesired = builtins.listToAttrs (
    map (
      format:
      lib.nameValuePair format.name (
        (pkgs.formats.${format.name} { }).generate "config-graft-empty.${format.fileExtension}" { }
      )
    ) formats
  );

  # Manifest of this generation's file entries, linked as a `home.file` snapshot so
  # the *next* switch can read it (at `$oldGenPath/home-files/<manifestRel>`) to find
  # entries that were removed. TSV rows: `<format>\t<target>\t<snapshotRel>\t<binary>`.
  manifestRel = ".local/state/home-manager/config-graft-manifest";
  manifestFile = pkgs.writeText "config-graft-manifest" (
    lib.concatMapStrings (
      e: "${e.format}\t${e.target}\t${e.snapshotRel}\t${if e.binary then "1" else "0"}\n"
    ) fileEntries
  );

  # Format-qualified keys of the entries still managed this generation, as a
  # newline-delimited string with sentinel newlines, for a pure-bash membership test.
  currentKeys = "\n" + lib.concatMapStrings (e: "${e.format}\t${e.target}\n") fileEntries;

  # Reconcile the target of every entry present last generation but gone this one back
  # to empty: its old snapshot (still reachable in `$oldGenPath`) is BASE, so exactly
  # the keys we last grafted are pruned while keys the app or user wrote are kept.
  # Without this a removed entry (its `settings` emptied, or the whole entry deleted)
  # would leave its last-applied keys frozen in the committed file -- no active entry
  # drives their prune, and config-graft's snapshot is gone too. Runs unconditionally
  # (even when no entry remains) since cleaning up after the *last* entry is removed is
  # the whole point. No external tools: pure bash plus the config-graft binary.
  orphanPruneScript = ''
    if [[ -v oldGenPath && -e "$oldGenPath/home-files/${manifestRel}" ]]; then
      while IFS=$'\t' read -r _cgFormat _cgTarget _cgSnap _cgBinary; do
        [[ -n "$_cgFormat" ]] || continue
        # Still managed this generation? Then its own entry prunes it; skip.
        if [[ ${lib.escapeShellArg currentKeys} == *$'\n'"$_cgFormat"$'\t'"$_cgTarget"$'\n'* ]]; then
          continue
        fi
        _cgBase="$oldGenPath/home-files/$_cgSnap"
        [[ -e "$_cgBase" ]] || continue
        case "$_cgFormat" in
          json) _cgEmpty=${emptyDesired.json} ;;
          yaml) _cgEmpty=${emptyDesired.yaml} ;;
          toml) _cgEmpty=${emptyDesired.toml} ;;
          plist) _cgEmpty=${emptyDesired.plist} ;;
          *) continue ;;
        esac
        _cgBin=""
        [[ "$_cgBinary" = 1 ]] && _cgBin="--plist-binary"
        _i "Pruning removed managed %s file %s" "$_cgFormat" "${homeDir}/$_cgTarget"
        run ${lib.getExe config.home.managed.package} \
          "$_cgFormat" "${homeDir}/$_cgTarget" "$_cgEmpty" "$_cgBase" $_cgBin
      done < "$oldGenPath/home-files/${manifestRel}"
    fi
  '';
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
    # follows as the BASE root). The file-entry manifest rides along the same way, so
    # the next switch can read it to prune entries removed since (see orphanPrune).
    home.file =
      builtins.listToAttrs (
        lib.concatMap (
          x: lib.mapAttrsToList (_: e: lib.nameValuePair e.snapshotRel { source = e.desired; }) x.entries
        ) byFormat
        ++ lib.mapAttrsToList (
          _: e: lib.nameValuePair e.snapshotRel { source = e.source; }
        ) directoryEntries
      )
      // lib.optionalAttrs (fileEntries != [ ]) {
        ${manifestRel}.source = manifestFile;
      };

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
      ++ [
        {
          name = "managedDirectory";
          value = lib.mkIf (directoryEntries != { }) (
            lib.hm.dag.entryAfter [ "writeBoundary" ] (
              lib.concatStringsSep "\n" (lib.mapAttrsToList (_: e: e.script) directoryEntries)
            )
          );
        }
        # Unconditional: it must still run in the generation that removed the *last*
        # entry (when there are no active entries to key it off). It self-guards on a
        # previous-generation manifest and no-ops when there's nothing to prune.
        {
          name = "configGraftOrphanPrune";
          value = lib.hm.dag.entryAfter [ "writeBoundary" ] orphanPruneScript;
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
