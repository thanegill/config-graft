# Format-agnostic pieces shared by the three managed-file modules. Plain helpers
# with no per-platform dispatch: each module writes its own `options`/`config`
# linearly and calls these for the entry submodule, the DESIRED store path, the
# reconcile script, and the build-time assertions.
let
  inherit (import ./formats.nix) isFreeform;
in
{
  # DESIRED store path for one entry: a pre-built `source` when given, otherwise
  # generated from `settings` (a `pkgs.formats` generator for freeform formats,
  # `lib.generators.toPlist` for plist).
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
    else if isFreeform format then
      entry.format.generate "managed-${format.format}-${name}.${format.fileExtension}" entry.settings
    else
      pkgs.writeText "managed-plist-${name}.plist" (
        lib.generators.toPlist { escape = true; } entry.settings
      );

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
                The config-graft package used to reconcile this entry. Defaults to
                this flake's own build, so no overlay or `PATH` entry is needed,
                since the activation script calls it by store path.
              '';
            };

            settings = mkOption {
              type = if isFreeform format then config.format.type else (pkgs.formats.json { }).type;
              default = { };
              example = format.settingsExample;
              description = "Freeform ${format.format} data reconciled into {option}`target`. Empty disables the entry.";
            };

            source = mkOption {
              type = types.nullOr types.path;
              default = null;
              example = literalExpression "./managed.${format.fileExtension}";
              description = ''
                A pre-built ${format.format} file to reconcile into {option}`target`,
                as an alternative to {option}`settings`, for a DESIRED built some
                other way (another generator, a rendered template, a checked-in file,
                a derivation). Mutually exclusive with {option}`settings`; setting
                either one makes the entry active.
              '';
            };
          }
          // lib.optionalAttrs (isFreeform format) {
            format = mkOption {
              type = types.raw;
              default = pkgs.formats.${format.format} { };
              defaultText = literalExpression "pkgs.formats.${format.format} { }";
              description = ''
                A `pkgs.formats`-style generator (providing `type` and `generate`)
                used to build {option}`settings`. Override to use a validating format.
              '';
            };
          }
          // lib.optionalAttrs (format.kind == "plist") {
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
  mkConfigGraftActivationScript =
    {
      lib,
      format,
      entry,
      desired,
      target,
    }:
    if format.kind == "plist" && entry.cfprefsdDomain != null then
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
        run ${lib.getExe entry.package} --format plist "$_live" ${desired} "$_prev"
        run /usr/bin/defaults import "$_domain" "$_live"
        rm -f "$_live"
      ''
    else
      ''
        _target=${lib.escapeShellArg target}
        _i "Reconciling managed ${format.format} file %s" "$_target"
        run ${lib.getExe entry.package} --format ${format.format} "$_target" ${desired} "$_prev"
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
    lib.optionals (format.kind == "plist") (
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
