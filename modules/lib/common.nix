# Format-agnostic pieces shared by the three managed-file modules. These are plain
# functions with no per-platform dispatch: each module writes its own
# `options`/`config` linearly and calls these for the entry submodule, the DESIRED
# store path, the reconcile script, and the build-time assertions. Every format is
# uniform, serialized by its `pkgs.formats.<name>` generator (`format.name` is the
# format name, e.g. "json"; the entry's `format` option is that generator).
let
  # An entry's attribute name may be a path (e.g. ".config/app/config.json"). Turn it
  # into a flat, filesystem-safe id for snapshot and DESIRED filenames: no "/" (so no
  # nested dirs or a leading "//"), and no doubled extension since the name already
  # ends in one.
  safeName = builtins.replaceStrings [ "/" ] [ "-" ];
in
{
  inherit safeName;

  # DESIRED store path for one entry: a pre-built `source` when given, otherwise
  # generated from `settings` by the entry's `pkgs.formats` generator.
  mkDesired =
    {
      lib,
      pkgs,
      format,
      name,
      entry,
    }:
    if entry.source != null then
      entry.source
    else
      entry.format.generate "managed-${format.name}-${safeName name}" entry.settings;

  # The `attrsOf submodule` type for one format's entries. Every option is the same
  # on every platform except `target` (relative vs absolute) and the `cfprefsdDomain`
  # description (per-user vs system), which the caller supplies.
  entryType =
    {
      lib,
      pkgs,
      defaultPackage,
      format,
      targetOption,
      cfprefsdDescription,
    }:
    let
      inherit (lib) mkOption types literalExpression;
    in
    types.attrsOf (
      types.submodule (
        { name, config, ... }:
        {
          options = {
            target = targetOption;

            package = mkOption {
              type = types.package;
              default = defaultPackage;
              defaultText = literalExpression "config-graft.packages.\${system}.default";
              description = ''
                The config-graft package used to reconcile this entry. Defaults to the
                module-level `managed.package` (this flake's own build), which the
                activation script calls by store path, so no overlay or `PATH` entry is
                needed.
              '';
            };

            settings = mkOption {
              type = config.format.type;
              default = { };
              example = format.settingsExample;
              description = "Freeform ${format.name} data reconciled into {option}`target`. Empty disables the entry.";
            };

            source = mkOption {
              type = types.nullOr types.path;
              default = null;
              example = literalExpression "./managed.${format.fileExtension}";
              description = ''
                A pre-built ${format.name} file to reconcile into {option}`target`,
                as an alternative to {option}`settings`, for a DESIRED built some
                other way (another generator, a rendered template, a checked-in file,
                a derivation). Mutually exclusive with {option}`settings`; setting
                either one makes the entry active.
              '';
            };

            format = mkOption {
              type = types.raw;
              default = pkgs.formats.${format.name} { };
              defaultText = literalExpression "pkgs.formats.${format.name} { }";
              description = ''
                A `pkgs.formats`-style generator (providing `type` and `generate`)
                used to build {option}`settings`. Override to use a validating format.
              '';
            };
          }
          // lib.optionalAttrs (format.name == "plist") {
            # `defaults`/`plutil`/`cfprefsd` are macOS-only; `mkAssertions` guards a
            # Darwin host when this is set.
            cfprefsdDomain = mkOption {
              type = types.nullOr types.str;
              default = null;
              example = "com.example.app";
              description = cfprefsdDescription;
            };
          };

          config.target = lib.mkDefault name;
        }
      )
    );

  # The per-entry reconcile script, shared by every platform. It uses `run` (run a
  # command) and `_i` (info log): home-manager provides these in its activation
  # context, and the system module defines pass-through shims. The caller sets
  # `_prev` (the BASE snapshot path) beforehand and passes the resolved `target`.
  #
  # Shell vars are `_`-prefixed because home-manager runs this inline in its
  # activation shell, shared with every other module's activation code, so bare
  # names like `prev`/`target`/`domain` could clobber or be clobbered by another
  # module's variables. (The system side runs its own script, where it's harmless.)
  mkEntryReconcileScript =
    {
      lib,
      format,
      entry,
      desired,
      target,
    }:
    if format.name == "plist" && entry.cfprefsdDomain != null then
      ''
        _domain=${lib.escapeShellArg entry.cfprefsdDomain}
        _i "Reconciling managed plist domain %s" "$_domain"

        # Read the live domain through cfprefsd (not the on-disk file, which may be
        # staler than cfprefsd's cache). Empty/missing domain -> start from an empty
        # plist.
        _live=$(mktemp)
        /usr/bin/defaults export "$_domain" "$_live" 2>/dev/null || true
        [[ -s "$_live" ]] || /usr/bin/plutil -create xml1 "$_live"

        # Graft our settings into the live state, then push it back through cfprefsd
        # so it adopts the merged result.
        run ${lib.getExe entry.package} plist "$_live" ${desired} "$_prev"
        run /usr/bin/defaults import "$_domain" "$_live"
        rm -f "$_live"
      ''
    else
      ''
        _target=${lib.escapeShellArg target}
        _i "Reconciling managed ${format.name} file %s" "$_target"
        run ${lib.getExe entry.package} ${format.name} "$_target" ${desired} "$_prev"
      '';

  # Directory-format entry type. Unlike the byte formats there is no `settings`
  # (freeform data through a `pkgs.formats` generator): the DESIRED is a prebuilt
  # directory tree given as `source`, so the `directory` subcommand gets its own entry
  # type with the directory-specific reconcile flags. Every declared entry is
  # active (`source` is required), so there is no `settings != {}` liveness test.
  directoryEntryType =
    {
      lib,
      pkgs,
      defaultPackage,
      targetOption,
      sourceDescription,
    }:
    let
      inherit (lib) mkOption types literalExpression;
    in
    types.attrsOf (
      types.submodule (
        { name, ... }:
        {
          options = {
            target = targetOption;

            package = mkOption {
              type = types.package;
              default = defaultPackage;
              defaultText = literalExpression "config-graft.packages.\${system}.default";
              description = ''
                The config-graft package used to reconcile this entry. Defaults to the
                module-level `managed.package` (this flake's own build), called by store
                path, so no overlay or `PATH` entry is needed.
              '';
            };

            source = mkOption {
              type = types.path;
              example = literalExpression "./dotfiles";
              description = sourceDescription;
            };

            manageRoot = mkOption {
              type = types.bool;
              default = false;
              description = ''
                Also reconcile {option}`target`'s own directory attributes
                (mode/owner/xattrs), not just its contents (`--manage-root`).
              '';
            };

            noOwner = mkOption {
              type = types.bool;
              default = false;
              description = ''
                Don't reconcile file/directory ownership, uid/gid (`--no-owner`). A
                store-built {option}`source` is owned by the build user (root), which a
                non-root (home-manager) activation can't chown to, so set this there.
              '';
            };

            xattrs = mkOption {
              type = types.enum [
                "all"
                "safe"
                "none"
              ];
              default = "all";
              description = ''
                Which extended attributes to reconcile (`--xattrs`): `all`, `safe`
                (skip privileged/system namespaces), or `none`.
              '';
            };
          };

          config.target = lib.mkDefault name;
        }
      )
    );

  # The per-entry directory reconcile script, shared by every platform (the sibling
  # of `mkEntryReconcileScript` for the `directory` subcommand). The caller sets `_prev`
  # (the BASE snapshot directory) beforehand and passes the resolved `target`; the
  # DESIRED is the entry's `source` tree.
  mkDirectoryReconcileScript =
    {
      lib,
      entry,
      target,
    }:
    let
      flags = lib.concatStringsSep " " (
        lib.optional entry.manageRoot "--manage-root"
        ++ lib.optional entry.noOwner "--no-owner"
        ++ lib.optional (entry.xattrs != "all") "--xattrs ${entry.xattrs}"
      );
    in
    ''
      _target=${lib.escapeShellArg target}
      _i "Reconciling managed directory tree %s" "$_target"
      run ${lib.getExe entry.package} directory ${flags} "$_target" ${entry.source} "$_prev"
    '';

  # Build-time guards for one format's active entries: `cfprefsdDomain` drives
  # macOS-only tooling, and `settings`/`source` are mutually exclusive.
  mkAssertions =
    {
      lib,
      pkgs,
      parent,
      format,
      active,
    }:
    lib.optionals (format.name == "plist") (
      lib.mapAttrsToList (name: entry: {
        assertion = entry.cfprefsdDomain == null || pkgs.stdenv.hostPlatform.isDarwin;
        message = ''
          ${parent}.${format.optionName}."${name}".cfprefsdDomain is set,
          but cfprefsd, defaults, and plutil exist only on macOS (this
          configuration targets ${pkgs.stdenv.hostPlatform.system}). Unset it to
          edit the plist file in place instead.
        '';
      }) active
    )
    ++ lib.mapAttrsToList (name: entry: {
      assertion = !(entry.settings != { } && entry.source != null);
      message = ''
        ${parent}.${format.optionName}."${name}" sets both `settings` and
        `source`; they are mutually exclusive; use one.
      '';
    }) active;
}
