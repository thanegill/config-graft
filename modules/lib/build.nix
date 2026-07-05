# Assemble `{ options; config; }` for one platform. Called from inside a module,
# so `config`/`lib`/`pkgs` come from that module's own arguments.
let
  inherit (import ./formats.nix) formats isFreeform;
in
{
  config,
  lib,
  pkgs,
  platform,
  defaultPackage,
}:
let
  inherit (lib)
    mkOption
    mkIf
    mkMerge
    types
    filterAttrs
    mapAttrsToList
    concatStringsSep
    optionalAttrs
    optionals
    literalExpression
    ;

  # DESIRED store path for one entry: a pre-built `source` file when given,
  # otherwise generated from `settings`: a `pkgs.formats` generator for
  # freeform formats (overridable per entry via `format`), `lib.generators.toPlist`
  # for plist.
  mkDesired =
    spec: name: entry:
    if entry.source != null then
      entry.source
    else if isFreeform spec then
      entry.format.generate "managed-${spec.format}-${name}.${spec.fileExtension}" entry.settings
    else
      pkgs.writeText "managed-plist-${name}.plist" (
        lib.generators.toPlist { escape = true; } entry.settings
      );

  # plist shares JSON's value model (minus null, which it cannot emit).
  settingsType =
    spec: entryConfig:
    if isFreeform spec then entryConfig.format.type else (pkgs.formats.json { }).type;

  formatOption =
    spec:
    mkOption {
      type = types.raw;
      default = pkgs.formats.${spec.format} { };
      defaultText = literalExpression "pkgs.formats.${spec.format} { }";
      description = ''
        A `pkgs.formats`-style generator (providing `type` and `generate`) used
        to build {option}`settings`. Override to use a validating format.
      '';
    };

  mkSubmodule =
    spec:
    types.submodule (
      { name, config, ... }:
      {
        options = {
          target = platform.targetOption spec;

          package = mkOption {
            type = types.package;
            default = defaultPackage;
            defaultText = literalExpression "config-graft.packages.\${system}.default";
            description = ''
              The config-graft package used to reconcile this entry. Defaults
              to this flake's own build, so no overlay or `PATH` entry is
              needed, since the activation script calls it by store path.
            '';
          };

          settings = mkOption {
            type = settingsType spec config;
            default = { };
            example = spec.settingsExample;
            description = "Freeform ${spec.format} data reconciled into {option}`target`. Empty disables the entry.";
          };

          source = mkOption {
            type = types.nullOr types.path;
            default = null;
            example = literalExpression "./managed.${spec.fileExtension}";
            description = ''
              A pre-built ${spec.format} file to reconcile into {option}`target`,
              as an alternative to {option}`settings`, for a DESIRED built some
              other way (another generator, a rendered template, a checked-in
              file, a derivation). Mutually exclusive with {option}`settings`;
              setting either one makes the entry active.
            '';
          };
        }
        // optionalAttrs (isFreeform spec) { format = formatOption spec; }
        // platform.extraEntryOptions spec;

        config = platform.targetConfig name;
      }
    );

  perSpec =
    spec:
    let
      cfg = config.${platform.parent}.${spec.optionName};
      active = filterAttrs (_: entry: entry.settings != { } || entry.source != null) cfg;

      # Per-entry data, built from `active`. It must only feed config *values*,
      # never config *keys* (see the header note on `_module.freeformType`).
      entries = mapAttrsToList (name: entry: rec {
        snapshotRel = platform.snapshotRel spec name;
        desired = mkDesired spec name entry;
        script = platform.mkScript {
          inherit
            spec
            entry
            desired
            snapshotRel
            ;
          target = platform.targetPath config entry;
        };
      }) active;

      activationText = concatStringsSep "\n" (map (e: e.script) entries);

      # `cfprefsdDomain` (plist only, on any platform that offers it) drives
      # `defaults`/`plutil`/`cfprefsd`, which are macOS-only, so guard it at build
      # time rather than failing mid-activation on a non-Darwin host.
      cfprefsdAssertions = optionals (spec.kind == "plist") (
        mapAttrsToList (name: entry: {
          assertion = entry.cfprefsdDomain == null || pkgs.stdenv.hostPlatform.isDarwin;
          message = ''
            ${platform.parent}.${spec.optionName}."${name}".cfprefsdDomain is set,
            but cfprefsd, defaults, and plutil exist only on macOS (this
            configuration targets ${pkgs.stdenv.hostPlatform.system}). Unset it to
            edit the plist file in place instead.
          '';
        }) active
      );

      # `settings` and `source` are two ways to build the same DESIRED.
      sourceAssertions = mapAttrsToList (name: entry: {
        assertion = !(entry.settings != { } && entry.source != null);
        message = ''
          ${platform.parent}.${spec.optionName}."${name}" sets both `settings` and
          `source`; they are mutually exclusive; use one.
        '';
      }) active;
    in
    {
      inherit (spec) optionName;

      option = mkOption {
        default = { };
        description = platform.optionDescription spec;
        type = types.attrsOf (mkSubmodule spec);
      };

      config = mkIf (active != { }) (mkMerge [
        (platform.recordSnapshots { inherit spec entries; })
        (platform.wireActivation {
          inherit spec;
          text = activationText;
        })
        { assertions = cfprefsdAssertions ++ sourceAssertions; }
      ]);
    };

  built = map perSpec formats;
in
{
  options.${platform.parent} = builtins.listToAttrs (
    map (m: {
      name = m.optionName;
      value = m.option;
    }) built
  );

  config = mkMerge (map (m: m.config) built);
}
