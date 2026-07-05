# Format-agnostic leaf helpers shared by the three managed-file modules. These are
# plain functions with no per-platform dispatch: each module writes its own
# `options`/`config` linearly and calls these for the pieces every platform shares
# (the entry submodule, the DESIRED store path, the build-time assertions).
let
  inherit (import ./formats.nix) isFreeform;
  cfprefsdDomainOption = import ./cfprefsd.nix;
in
{
  # DESIRED store path for one entry: a pre-built `source` when given, otherwise
  # generated from `settings` (a `pkgs.formats` generator for freeform formats,
  # `lib.generators.toPlist` for plist).
  mkDesired =
    lib: pkgs: format: name: entry:
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
            cfprefsdDomain = cfprefsdDomainOption lib cfprefsdDescription;
          };

          config.target = lib.mkDefault name;
        }
      )
    );

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
