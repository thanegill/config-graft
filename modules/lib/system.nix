# Linear system module for the NixOS and nix-darwin wrappers, which differ only in
# `activationWiring` (how the reconcile script is placed into activation) and pass
# their own. Declares `environment.managed{Json,Plist,Yaml,Toml}` and reconciles all
# managed system files from a single `config-graft-activation` script, so the whole
# system module reads top to bottom.
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
  configGraftLib = import ./.;
  inherit (configGraftLib) formats;

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
        System-level ${format.name} configuration files that an application owns and
        writes to, but which should be partially managed declaratively. Each entry
        deep-merges its {option}`settings` into the absolute {option}`target` during
        system activation (via {command}`config-graft`), keeping keys the app wrote
        that aren't managed here and pruning keys dropped from Nix.
      '';
      type = configGraftLib.entryType {
        inherit
          lib
          pkgs
          format
          ;
        defaultPackage = config.environment.managed.package;
        targetOption = lib.mkOption {
          type = lib.types.str;
          example = systemTargetExample.${format.name};
          description = "Absolute path of the managed ${format.name} file. Defaults to the attribute name.";
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

  # The `directory` subcommand option: reconcile a whole `source` tree into an
  # absolute target directory. Distinct from the byte formats (no `settings`).
  directoryOption = lib.mkOption {
    default = { };
    description = ''
      System-level directory *trees* an application owns and writes to, which should
      be partially managed declaratively. Each entry reconciles its {option}`source`
      tree into the absolute {option}`target` during system activation (via
      {command}`config-graft directory`): files the app created are kept,
      files dropped from {option}`source` are pruned, and per-file mode/owner/xattrs
      are reconciled.
    '';
    type = configGraftLib.directoryEntryType {
      inherit lib pkgs;
      defaultPackage = config.environment.managed.package;
      targetOption = lib.mkOption {
        type = lib.types.str;
        example = "/etc/app";
        description = "Absolute path of the managed directory. Defaults to the attribute name.";
      };
      sourceDescription = ''
        The directory tree reconciled into {option}`target` (a path, or a derivation
        that builds one). Its files' modes/owner become the desired attributes; system
        activation runs as root, so a store-owned (root) tree applies as-is.
      '';
    };
  };

  # Directory entries (all active — `source` is required), each with its snapshot
  # path and reconcile script. `entry.target` is absolute.
  directoryEntries = lib.mapAttrsToList (name: entry: {
    inherit entry;
    snapshotRel = "config-graft/managed-directory/${configGraftLib.safeName name}";
    script = configGraftLib.mkDirectoryReconcileScript {
      inherit lib entry;
      target = entry.target;
    };
  }) config.environment.managedDirectory;

  # The active entries of every format, flattened, each carrying its snapshot path
  # and DESIRED. Values only; never used to build config *keys*.
  activeByFormat = map (format: {
    inherit format;
    active = lib.filterAttrs (
      _: entry: entry.settings != { } || entry.source != null
    ) config.environment.${format.optionName};
  }) formats;

  entries = lib.concatMap (
    { format, active }:
    lib.mapAttrsToList (name: entry: {
      inherit format entry;
      snapshotRel = "config-graft/managed-${format.name}/${configGraftLib.safeName name}";
      desired = configGraftLib.mkDesired {
        inherit
          lib
          pkgs
          format
          name
          entry
          ;
      };
    }) active
  ) activeByFormat;

  # A single activation script for all managed system files. The system activation
  # environment has none of home-manager's helpers, so define pass-through `run` and
  # `_i` shims (the reconcile body, shared with home-manager, calls them). BASE is
  # the previous generation's snapshot, reachable at /run/current-system until
  # activation's final symlink swap; empty on the first switch -> no pruning.
  activationScript = pkgs.writeShellScript "config-graft-activation" (
    ''
      # The reconcile body (shared with home-manager) calls `run` and `_i`.
      # home-manager defines them in its activation context; here there are none, so
      # shim them: `run` executes its arguments, `_i` prints an info line.
      run() { "$@"; }
      _i() {
        _fmt="$1"
        shift
        printf "config-graft: $_fmt\n" "$@"
      }
    ''
    + lib.concatMapStringsSep "\n" (
      e:
      ''
        _prev="/run/current-system/${e.snapshotRel}"
        [[ -e "$_prev" ]] || _prev=""
      ''
      + configGraftLib.mkEntryReconcileScript {
        inherit lib;
        inherit (e) format entry desired;
        target = e.entry.target;
      }
    ) entries
    + lib.concatMapStringsSep "\n" (
      e:
      ''
        _prev="/run/current-system/${e.snapshotRel}"
        [[ -e "$_prev" ]] || _prev=""
      ''
      + e.script
    ) directoryEntries
  );
in
{
  options.environment =
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
          Default config-graft package for every `environment.managed*` entry. Defaults
          to this flake's own build; override a single entry with its `package` option.
        '';
      };
    };

  config = lib.mkIf (entries != [ ] || directoryEntries != [ ]) {
    # Embed each DESIRED into the toplevel closure at its snapshot path (a directory
    # DESIRED is a symlink to its store tree, which config-graft follows as BASE).
    system.systemBuilderCommands =
      lib.concatMapStrings (e: ''
        mkdir -p "$(dirname "$out/${e.snapshotRel}")"
        ln -s ${e.desired} $out/${e.snapshotRel}
      '') entries
      + lib.concatMapStrings (e: ''
        mkdir -p "$(dirname "$out/${e.snapshotRel}")"
        ln -s ${e.entry.source} $out/${e.snapshotRel}
      '') directoryEntries;

    # Each wrapper places the activation script its own way (see `activationWiring`).
    system.activationScripts = activationWiring activationScript;

    assertions =
      lib.concatMap (
        { format, active }:
        configGraftLib.mkAssertions {
          inherit
            lib
            pkgs
            format
            active
            ;
          parent = "environment";
        }
      ) activeByFormat
      ++ (
        # config-graft reconciles a mutable file/tree in place; `environment.etc`
        # symlinks an immutable store path into /etc. Reject a target that is also an
        # etc entry (byte formats and directories alike).
        let
          etcTargets = map (e: "/etc/${e.target}") (lib.attrValues config.environment.etc);
          managedTargets = map (e: e.entry.target) (entries ++ directoryEntries);
        in
        map (target: {
          assertion = !(lib.elem target etcTargets);
          message = ''
            `environment.etc` and a config-graft `managed<Format>`/`managedDirectory`
            entry both manage `${target}`. `environment.etc` creates an immutable store
            symlink, while config-graft reconciles a mutable path in place; declare it
            in one.
          '';
        }) managedTargets
      );
  };
}
