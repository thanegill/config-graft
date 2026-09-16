# Format-agnostic pieces shared by the three managed-file modules. These are plain
# functions with no per-platform dispatch: each module writes its own
# `options`/`config` linearly and calls these for the entry submodule, the DESIRED
# store path, the reconcile script, the generation manifest and its orphan-prune, and
# the build-time assertions. Every format is uniform, serialized by its
# `pkgs.formats.<name>` generator (`format.name` is the format name, e.g. "json"; the
# entry's `format` option is that generator).
let
  # An entry's attribute name may be a path (e.g. ".config/app/config.json"). Turn it
  # into a flat, filesystem-safe id for snapshot and DESIRED filenames: no "/" (so no
  # nested dirs or a leading "//"), and no doubled extension since the name already
  # ends in one.
  safeName = builtins.replaceStrings [ "/" ] [ "-" ];

  # The attribute *policy* flags of a directory entry, separate from `--manage-root`:
  # the policy says which attributes are reconciled at all, `--manage-root` says
  # whether the target's own attributes are among them. A reconcile wants both; a
  # caller with a DESIRED tree of its own making wants only the policy.
  directoryPolicyFlags =
    { lib, entry }:
    lib.optional entry.noOwner "--no-owner"
    ++ lib.optional (entry.xattrs != "all") "--xattrs ${entry.xattrs}";

  # The empty DESIRED of every `kind` the orphan-prune can reconcile back to empty,
  # in one store tree named by the manifest's `kind` column so the script path-joins
  # instead of dispatching on the format a second time.
  #
  # Written literally rather than through each format's `pkgs.formats` generator: an
  # empty document is a constant, and generating it would drag every generator (for
  # TOML, remarshal and its Python closure) into the build of a consumer who declares
  # no entry of that format. The assertion ties the literals back to `formats.nix`, so
  # a format added there without an empty document fails the build.
  emptyDesired =
    {
      lib,
      pkgs,
      formats,
    }:
    let
      documents = {
        json = "{}";
        yaml = "{}\n";
        toml = "";
        plist = ''
          <?xml version="1.0" encoding="UTF-8"?>
          <!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
          <plist version="1.0">
          <dict/>
          </plist>
        '';
      };
      undocumented = lib.subtractLists (builtins.attrNames documents) (map (f: f.name) formats);
    in
    assert lib.assertMsg (undocumented == [ ]) (
      "config-graft: no empty document for format(s) ${toString undocumented}"
    );
    pkgs.runCommand "config-graft-empty-desired" { } (
      "mkdir -p $out/directory\n"
      + lib.concatStrings (
        lib.mapAttrsToList (
          name: text: "cp ${pkgs.writeText "config-graft-empty-${name}" text} $out/${name}\n"
        ) documents
      )
    );
in
{
  inherit safeName;

  # DESIRED store path for one entry: a pre-built `source` when given, otherwise
  # generated from `settings` by the entry's `pkgs.formats` generator. A plist entry
  # with `binary = true` runs that same generator and converts its output to a binary
  # plist with libplist's `plistutil`, so the entry's `format` override still decides
  # how `settings` is serialized. `-s` sorts keys for byte-stable output; key order is
  # irrelevant to reconcile.
  #
  # The intermediate is XML holding raw bytes that XML 1.0 forbids (the ESC 0x1B
  # separators in `NSUserKeyEquivalents`), which `plistutil` accepts. It exists only
  # inside this build and is consumed immediately, so a stricter parser some day
  # fails the build loudly rather than corrupting a DESIRED at activation time.
  mkDesired =
    {
      lib,
      pkgs,
      format,
      name,
      entry,
    }:
    let
      generated = entry.format.generate "managed-${format.name}-${safeName name}" entry.settings;
    in
    if entry.source != null then
      entry.source
    else if format.name == "plist" && entry.binary then
      pkgs.runCommand "managed-plist-${safeName name}" { nativeBuildInputs = [ pkgs.libplist ]; } ''
        ${lib.getExe pkgs.libplist} -f bin -s -i ${generated} -o $out
      ''
    else
      generated;

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

            binary = mkOption {
              type = types.bool;
              default = config.cfprefsdDomain != null;
              defaultText = literalExpression "config.cfprefsdDomain != null";
              description = ''
                Generate the plist DESIRED as a binary plist and force the write
                with `--plist-format binary`, so values XML cannot represent (bytes
                illegal in XML 1.0, e.g. the ESC 0x1B separators in
                `NSUserKeyEquivalents`) round-trip. When generated from
                {option}`settings` this runs the entry's {option}`format` generator
                and converts its output with `pkgs.libplist` at build time, so a
                {option}`format` override still applies; with {option}`source` it
                only forces the binary write.

                A file entry rarely needs it: at `false` the DESIRED is XML but the
                *write* follows whatever encoding the target already has, so a
                binary target (which is what macOS stores) stays binary and keeps
                its own dates and bytes intact. Set it when {option}`settings`
                themselves hold a byte XML cannot carry, which an XML DESIRED could
                not express -- a build-time assertion says so.

                Defaults to `true` for a {option}`cfprefsdDomain` entry, whose whole
                round-trip is binary anyway ({command}`defaults export` produces a
                binary plist and {command}`defaults import` reads one back), so
                nothing a domain holds is squeezed through XML. Setting it to
                `false` builds that entry's DESIRED as XML again, which is fine for
                ordinary values but cannot carry a byte XML 1.0 forbids -- the write
                to cfprefsd stays binary either way, so `false` narrows what the
                DESIRED can express without widening anything.
              '';
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
        # so it adopts the merged result. `--plist-format binary` is unconditional
        # here -- not `binary`-gated, and not left to `follow`: routing this scratch
        # file through XML would drop XML-illegal bytes and sub-second dates that
        # both `defaults` ends carry, and an empty domain's scratch file is the
        # `plutil -create xml1` one above, which `follow` would read as XML.
        run ${lib.getExe entry.package} plist "$_live" ${desired} "$_prev" --plist-format binary
        run /usr/bin/defaults import "$_domain" "$_live"
        rm -f "$_live"
      ''
    else
      ''
        _target=${lib.escapeShellArg target}
        _i "Reconciling managed ${format.name} file %s" "$_target"
        run ${lib.getExe entry.package} ${format.name} "$_target" ${desired} "$_prev"${
          lib.optionalString (format.name == "plist" && entry.binary) " --plist-format binary"
        }
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
        lib.optional entry.manageRoot "--manage-root" ++ directoryPolicyFlags { inherit lib entry; }
      );
    in
    ''
      _target=${lib.escapeShellArg target}
      _i "Reconciling managed directory tree %s" "$_target"
      run ${lib.getExe entry.package} directory ${flags} "$_target" ${entry.source} "$_prev"
    '';

  # One manifest row per managed unit, the sibling of the reconcile scripts above:
  # they say how to apply an entry *this* generation, these say what a *later* one
  # needs to reconcile it back to empty once it is gone. The two must agree on which
  # plist entries are domains rather than files, hence the shared branch.
  #
  # `kind` picks the prune (a format name, `domain`, or `directory`), `identity` is
  # what the unit is known by across generations (its absolute target, or its
  # cfprefsd domain), `snapshotRel` locates the BASE inside a generation, and `flags`
  # are the write flags to carry.
  mkPruneRow =
    {
      lib,
      format,
      entry,
      target,
      snapshotRel,
    }:
    if format.name == "plist" && entry.cfprefsdDomain != null then
      {
        kind = "domain";
        identity = entry.cfprefsdDomain;
        inherit snapshotRel;
        flags = [ ];
      }
    else
      {
        kind = format.name;
        identity = target;
        inherit snapshotRel;
        flags = lib.optional (format.name == "plist" && entry.binary) "--plist-format binary";
      };

  # The directory sibling of `mkPruneRow`. It carries the attribute *policy* but not
  # `--manage-root`: the prune's DESIRED is an empty store directory, so managing the
  # root would stamp that store directory's own mode and ownership onto the target
  # instead of leaving a tree we no longer manage alone.
  mkDirectoryPruneRow =
    {
      lib,
      entry,
      target,
      snapshotRel,
    }:
    {
      kind = "directory";
      identity = target;
      inherit snapshotRel;
      flags = directoryPolicyFlags { inherit lib entry; };
    };

  # This generation's rows as manifest text, for the caller to place inside the
  # generation beside the snapshots it points at. Tab-separated, one row per line: no
  # field may hold a tab or a newline, which no target path reachable through
  # `home.file` or `environment.etc` can anyway.
  mkManifest =
    { lib, rows }:
    lib.concatMapStrings (
      row: "${row.kind}\t${row.identity}\t${row.snapshotRel}\t${lib.concatStringsSep " " row.flags}\n"
    ) rows;

  # Enforce the manifest's one format rule at build time. A tab in a `target` or
  # `cfprefsdDomain` shifts every later field of its row, so the prune reads a
  # truncated identity and a snapshot path that is really the rest of the target; a
  # newline splits the row in two. The row then no longer matches the membership test
  # built from the same values, so a *still-declared* entry falls through to the prune
  # branch. Nothing downstream can recover from that, and neither field has a
  # legitimate use for either character.
  mkManifestAssertions =
    {
      lib,
      parent,
      rows,
    }:
    lib.concatMap (
      row:
      map
        (field: {
          assertion = !(lib.hasInfix "\t" field || lib.hasInfix "\n" field);
          message = ''
            A `${parent}.managed*` entry reconciling `${row.kind}` has a tab or newline
            in its target or attribute name. config-graft records what each generation
            manages in a tab-separated manifest, so that a later generation can prune
            the entry once you remove it; neither character can be written there.
          '';
        })
        [
          row.identity
          row.snapshotRel
        ]
    ) rows;

  # Reconcile back to empty every unit the previous generation managed and this one
  # does not, against that unit's own previous snapshot as BASE -- so a removed entry
  # prunes exactly the keys or files it last grafted and leaves everything the app or
  # user wrote. Without it, per-entry pruning only ever fires while an entry stays
  # declared: emptying an entry's `settings` or deleting it outright drops both its
  # reconcile and its snapshot, freezing what it last applied.
  #
  # The previous generation is read as *data* (its manifest) and pruned by the code
  # and binary of the current one, so a fix here reaches generations built before it.
  # The caller sets `_cgRoot` to the previous generation's root beforehand, empty when
  # there is none (the first switch), and must run this unconditionally -- the
  # generation that removes the *last* entry is exactly the one with nothing left to
  # key it off.
  mkOrphanPruneScript =
    {
      lib,
      pkgs,
      formats,
      package,
      manifestRel,
      rows,
    }:
    let
      exe = lib.getExe package;
      empty = emptyDesired { inherit lib pkgs formats; };
      # Sentinel newlines on both ends so a row matches whole, never as the prefix of
      # a longer target.
      stillManaged = "\n" + lib.concatMapStrings (row: "${row.kind}\t${row.identity}\n") rows;
    in
    ''
      if [[ -n "$_cgRoot" && -e "$_cgRoot/${manifestRel}" ]]; then
        while IFS=$'\t' read -r _cgKind _cgId _cgSnap _cgFlags; do
          [[ -n "$_cgKind" ]] || continue
          case ${lib.escapeShellArg stillManaged} in
            *$'\n'"$_cgKind"$'\t'"$_cgId"$'\n'*) continue ;;
          esac
          _cgBase="$_cgRoot/$_cgSnap"
          [[ -e "$_cgBase" ]] || continue
          # A prune never *creates*. An absent target has nothing left to prune, and
          # reconciling one against an empty DESIRED would write it back as an empty
          # document: to the engine a missing TARGET is a first apply, so dropping an
          # entry after deleting its file would recreate the file (and its parent
          # directories) holding `{}`. Every kind but `domain` is identified by a path;
          # the domain's own existence test is in its arm.
          [[ "$_cgKind" == domain || -e "$_cgId" ]] || continue
          # $_cgFlags is unquoted on purpose: it is a flag *list* ("--xattrs safe"),
          # built by this module and never user text.
          case "$_cgKind" in
            ${lib.concatMapStringsSep "|" (format: format.name) formats})
              _i "Pruning removed managed %s file %s" "$_cgKind" "$_cgId"
              run ${exe} "$_cgKind" "$_cgId" "${empty}/$_cgKind" "$_cgBase" $_cgFlags
              ;;
            domain)
              # Same rule as the path kinds above: `defaults export` reports success
              # and writes an empty plist for a domain that does not exist, so its
              # absence has to be asked about directly.
              /usr/bin/defaults read "$_cgId" >/dev/null 2>&1 || continue
              _i "Pruning removed managed plist domain %s" "$_cgId"
              _cgLive=$(mktemp)
              /usr/bin/defaults export "$_cgId" "$_cgLive" 2>/dev/null || true
              [[ -s "$_cgLive" ]] || /usr/bin/plutil -create xml1 "$_cgLive"
              run ${exe} plist "$_cgLive" ${empty}/plist "$_cgBase" --plist-format binary
              run /usr/bin/defaults import "$_cgId" "$_cgLive"
              rm -f "$_cgLive"
              ;;
            directory)
              _i "Pruning removed managed directory tree %s" "$_cgId"
              run ${exe} directory $_cgFlags "$_cgId" ${empty}/directory "$_cgBase"
              ;;
          esac
        done < "$_cgRoot/${manifestRel}"
      fi
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
      let
        # Whether any string in `value` -- keys included -- holds a character an XML
        # plist cannot carry. Tested on the JSON rendering, where exactly those
        # characters survive as an escape: `builtins.toJSON` writes the C0 controls
        # as `\uXXXX`, `\b` or `\f`, and the three XML keeps (tab, newline, carriage
        # return) as `\t`/`\n`/`\r`. `\r` is off the list because the writer emits a
        # CR as the character reference `&#13;`. Escaped backslashes are dropped
        # first, so a value holding the literal text `\u001b` is not mistaken for a
        # control byte. Nix has no character-class regex, hence the substring test.
        hasXmlIllegalByte =
          value:
          let
            escapes = builtins.replaceStrings [ "\\\\" ] [ "" ] (builtins.toJSON value);
          in
          lib.any (needle: lib.hasInfix needle escapes) [
            "\\u00"
            "\\b"
            "\\f"
            # The two non-characters the writer also refuses. `builtins.toJSON`
            # emits these raw rather than as an escape, so they are matched as
            # themselves.
            (builtins.fromJSON ''"\uFFFE"'')
            (builtins.fromJSON ''"\uFFFF"'')
          ];
      in
      lib.concatLists (
        lib.mapAttrsToList (name: entry: [
          {
            assertion = entry.cfprefsdDomain == null || pkgs.stdenv.hostPlatform.isDarwin;
            message = ''
              ${parent}.${format.optionName}."${name}".cfprefsdDomain is set,
              but cfprefsd, defaults, and plutil exist only on macOS (this
              configuration targets ${pkgs.stdenv.hostPlatform.system}). Unset it to
              edit the plist file in place instead.
            '';
          }
          {
            # An XML DESIRED cannot carry such a character, and the reconcile refuses
            # that write rather than emitting a file only macOS can read. Catch it at
            # build time instead of at activation.
            assertion = entry.binary || entry.source != null || !(hasXmlIllegalByte entry.settings);
            message = ''
              ${parent}.${format.optionName}."${name}" has `settings` holding a
              character an XML plist cannot carry (a C0 control other than tab,
              newline or carriage return -- e.g. the ESC 0x1B separators in
              `NSUserKeyEquivalents`), but `binary` is false, so its DESIRED would be
              generated as XML and the reconcile would refuse the write. Set
              `binary = true` on this entry.
            '';
          }
        ]) active
      )
    )
    ++ lib.mapAttrsToList (name: entry: {
      assertion = !(entry.settings != { } && entry.source != null);
      message = ''
        ${parent}.${format.optionName}."${name}" sets both `settings` and
        `source`; they are mutually exclusive; use one.
      '';
    }) active;
}
