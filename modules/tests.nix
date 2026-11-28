# Nix-side tests for the module layer, exposed as flake `checks` so `nix flake
# check` runs them. `cargo test` covers the engine; nothing there exercises the
# module's own decisions, so a wrong `binary` default or a re-gated
# `--plist-format binary` would reintroduce issue #33 with the Rust suite fully
# green (the engine's floor never runs on a binary write).
#
# Almost everything here is a pure evaluation of `lib/common.nix` -- no platform
# module, no home-manager input -- so the assertions read as plain
# expected/actual. `binaryTargetRun` and `homeFileMigrationRun` are the exceptions:
# they run the real binary. `hmActivation` evaluates the home-manager module itself,
# with home-manager's own pieces stubbed.
{
  lib,
  pkgs,
  common,
  package,
}:
let
  # An entry as the submodule would produce it, so `mkEntryReconcileScript` and
  # `mkDesired` see the same shape they see in a real module.
  entry =
    overrides:
    {
      package = pkgs.hello;
      source = null;
      settings = { };
      format = pkgs.formats.plist { };
      cfprefsdDomain = null;
      binary = false;
      followSymlinks = false;
    }
    // overrides;

  script =
    overrides:
    common.mkEntryReconcileScript {
      inherit lib;
      format = {
        name = "plist";
      };
      entry = entry overrides;
      desired = "/desired.plist";
      target = "/target.plist";
    };

  plistEntryType =
    parent:
    common.entryType {
      inherit lib pkgs parent;
      defaultPackage = pkgs.hello;
      format = {
        name = "plist";
        fileExtension = "plist";
        settingsExample = { };
      };
      targetOption = lib.mkOption { type = lib.types.str; };
      cfprefsdDescription = "test";
    };

  directoryEntryType =
    parent:
    common.directoryEntryType {
      inherit lib pkgs parent;
      defaultPackage = pkgs.hello;
      targetOption = lib.mkOption { type = lib.types.str; };
      sourceDescription = "test";
    };

  # The documented default of an entry's `package` option, as the manual renders it.
  packageDefaultText = type: (type.getSubOptions [ ]).package.defaultText.text;

  # An entry's option defaults, `binary`'s computed from `cfprefsdDomain` among
  # them, observed through an evaluated submodule rather than the plain attrset above.
  resolvedEntry =
    parent: overrides:
    let
      evaluated = lib.evalModules {
        modules = [
          {
            options.entries = lib.mkOption {
              type = plistEntryType parent;
              default = { };
            };
          }
          { entries.probe = overrides; }
        ];
      };
    in
    evaluated.config.entries.probe;

  resolvedBinary = overrides: (resolvedEntry "home" overrides).binary;

  # The DESIRED derivation for one entry, as a path so two can be compared without
  # building either.
  desiredDrv =
    overrides:
    (common.mkDesired {
      inherit lib pkgs;
      format = {
        name = "plist";
      };
      name = "com.example.app.plist";
      entry = entry overrides;
    }).drvPath;

  # The entry submodule shape a plain `managedPlist` file entry resolves to, with
  # this flake's own build so the script can actually be run.
  fileEntry = entry {
    package = package;
    settings.a = 1;
  };

  # The one case pure evaluation cannot answer: what the module's own decisions do
  # to a *binary* plist on disk. macOS stores Preferences as binary, and an entry at
  # its defaults must leave the app's own XML-illegal bytes intact (issue #43) --
  # which holds only while the script omits `--plist-format`, the engine follows the
  # target's encoding, and the two agree about what "follow" means. A flag mapping
  # test sees none of that.
  binaryTargetRun =
    pkgs.runCommand "config-graft-module-binary-target"
      {
        nativeBuildInputs = [
          pkgs.libplist
          package
        ];
      }
      ''
        # Raw ESC 0x1B, as macOS writes into `NSUserKeyEquivalents`. Built by printf
        # rather than spelled here so nothing along the way has to carry the byte.
        printf '%s' \
          '<?xml version="1.0" encoding="UTF-8"?>' \
          '<plist version="1.0"><dict><key>NSUserKeyEquivalents</key><dict>' \
          > target.xml
        printf '<key>\033Window</key><string>@~n</string></dict></dict></plist>' >> target.xml
        plistutil -f bin -i target.xml -o target.plist

        run() { "$@"; }
        _i() { :; }
        _prev=""
        ${common.mkEntryReconcileScript {
          inherit lib;
          format = {
            name = "plist";
          };
          entry = fileEntry;
          desired = common.mkDesired {
            inherit lib pkgs;
            format = {
              name = "plist";
            };
            name = "com.example.app.plist";
            entry = fileEntry;
          };
          target = "target.plist";
        }}

        [ "$(head -c 7 target.plist)" = "bplist0" ] \
          || { echo "target was re-encoded, not followed"; exit 1; }
        plistutil -f xml -i target.plist -o roundtrip.xml
        grep -qa "$(printf '\033')Window" roundtrip.xml \
          || { echo "the app's own ESC byte did not survive"; exit 1; }
        grep -q '<key>a</key>' roundtrip.xml \
          || { echo "the managed key was not grafted in"; exit 1; }
        echo ok > $out
      '';

  # The home-manager module's activation entries, evaluated without a home-manager
  # input: `lib.hm.dag.entryAfter` and the few options the module writes are
  # stubbed, which is enough to read each entry's ordering.
  hmActivation =
    let
      hmLib = lib.extend (
        _: _: {
          hm.dag.entryAfter = after: data: { inherit after data; };
        }
      );
      stub = type: lib.mkOption { inherit type; };
      evaluated = hmLib.evalModules {
        modules = [
          (import ./home-manager.nix {
            self.packages.${pkgs.stdenv.hostPlatform.system}.default = package;
          })
          {
            options.home = {
              file = stub (lib.types.attrsOf lib.types.raw);
              activation = stub (lib.types.attrsOf lib.types.raw);
              homeDirectory = stub lib.types.str;
            };
            options.assertions = stub (lib.types.listOf lib.types.raw);
            config = {
              _module.args.pkgs = pkgs;
              home.homeDirectory = "/home/probe";
              home.managedJson.probe = {
                settings.a = 1;
                followSymlinks = true;
              };
              home.managedDirectory.tree.source = "/source";
            };
          }
        ];
      };
    in
    evaluated.config.home.activation;

  # An entry moved from `home.file` to `managedJson`: until home-manager's
  # `linkGeneration` removes it, the target is still the old generation's link into
  # the store. The module's script must refuse that link rather than write through
  # it, and once the link is gone must create a regular file -- which is why the
  # activation is ordered after `linkGeneration` (`hmActivation` above).
  homeFileMigrationRun =
    let
      oldHomeFile = pkgs.writeText "old-home-file.json" ''{"a":0}'';
      reconcile = common.mkEntryReconcileScript {
        inherit lib;
        format = {
          name = "json";
        };
        entry = entry {
          inherit package;
          format = pkgs.formats.json { };
        };
        desired = pkgs.writeText "desired.json" ''{"a":1}'';
        target = "target.json";
      };
    in
    pkgs.runCommand "config-graft-module-home-file-migration" { } ''
      run() { "$@"; }
      _i() { :; }
      _prev=""
      ln -s ${oldHomeFile} target.json

      if ( ${reconcile} ) 2> refused.log; then
        echo "wrote through the old home.file link"; exit 1
      fi
      grep -q -- '--follow-symlinks' refused.log \
        || { echo "the refusal did not name the flag:"; cat refused.log; exit 1; }
      [ "$(readlink target.json)" = ${oldHomeFile} ] \
        || { echo "the old link was replaced"; exit 1; }

      rm target.json
      ${reconcile}
      [ -f target.json ] && [ ! -L target.json ] \
        || { echo "no regular file after the link was removed"; exit 1; }
      grep -q '"a": 1' target.json \
        || { echo "the managed key was not written"; exit 1; }
      echo ok > $out
    '';

  # A generator distinguishable from the default only by what it emits. Under the
  # old `builtins.toJSON` path the entry's `format` was bypassed, so this produced
  # a derivation identical to the default one -- which is exactly the regression
  # this case catches.
  markedPlist = {
    inherit ((pkgs.formats.plist { })) type;
    generate =
      name: value: (pkgs.formats.plist { }).generate name (value // { CustomGeneratorRan = true; });
  };

  # `mkAssertions` for one format's active entries, as a list of the messages of
  # the assertions that *failed* -- empty means the configuration is accepted.
  assertionFailures =
    {
      entries,
      formatName ? "plist",
      darwin ? true,
    }:
    let
      hostPkgs =
        if darwin then
          pkgs
          // {
            stdenv = pkgs.stdenv // {
              hostPlatform = pkgs.stdenv.hostPlatform // {
                isDarwin = true;
                system = "aarch64-darwin";
              };
            };
          }
        else
          pkgs
          // {
            stdenv = pkgs.stdenv // {
              hostPlatform = pkgs.stdenv.hostPlatform // {
                isDarwin = false;
                system = "x86_64-linux";
              };
            };
          };
    in
    map (a: a.message) (
      builtins.filter (a: !a.assertion) (
        common.mkAssertions {
          inherit lib;
          pkgs = hostPkgs;
          parent = "home";
          format = {
            name = formatName;
            optionName = "managed${lib.toUpper (builtins.substring 0 1 formatName)}${
              builtins.substring 1 (-1) formatName
            }";
          };
          active = lib.mapAttrs (_: entry) entries;
        }
      )
    );

  directoryFlags =
    overrides:
    common.mkDirectoryReconcileScript {
      inherit lib;
      entry = {
        package = pkgs.hello;
        source = "/source";
        manageRoot = false;
        noOwner = false;
        xattrs = "all";
      }
      // overrides;
      target = "/target";
    };

  # A settings value carrying the ESC 0x1B separator, built via fromJSON so no
  # control byte appears in this file.
  escSettings = {
    NSUserKeyEquivalents = {
      "${builtins.fromJSON ''"\u001b"''}Window" = "@~n";
    };
  };

  cases = [
    {
      name = "cfprefsdDomain on a non-Darwin host is rejected";
      expected = 1;
      actual = builtins.length (assertionFailures {
        darwin = false;
        entries.probe = {
          cfprefsdDomain = "com.example.app";
        };
      });
    }
    {
      name = "cfprefsdDomain on Darwin is accepted";
      expected = 0;
      actual = builtins.length (assertionFailures {
        entries.probe = {
          cfprefsdDomain = "com.example.app";
        };
      });
    }
    {
      name = "settings and source together are rejected";
      expected = 1;
      actual = builtins.length (assertionFailures {
        entries.probe = {
          settings.a = 1;
          source = "/prebuilt.plist";
        };
      });
    }
    {
      name = "an XML-illegal byte with `binary = false` is rejected";
      expected = 1;
      actual = builtins.length (assertionFailures {
        entries.probe = {
          settings = escSettings;
          binary = false;
        };
      });
    }
    {
      name = "the same settings with `binary = true` are accepted";
      expected = 0;
      actual = builtins.length (assertionFailures {
        entries.probe = {
          settings = escSettings;
          binary = true;
        };
      });
    }
    {
      name = "a non-character with `binary = false` is rejected";
      expected = 1;
      actual = builtins.length (assertionFailures {
        entries.probe = {
          # U+FFFF: refused by the writer like a C0 control, but `toJSON` renders
          # it raw rather than as an escape.
          settings.a = builtins.fromJSON ''"\uFFFF"'';
          binary = false;
        };
      });
    }
    {
      name = "a carriage return is accepted with `binary = false`";
      expected = 0;
      actual = builtins.length (assertionFailures {
        entries.probe = {
          # The writer emits a CR as `&#13;`, which XML carries, so unlike the ESC
          # above this needs no binary DESIRED.
          settings.a = "line1\rline2";
          binary = false;
        };
      });
    }
    {
      name = "ordinary settings are accepted with `binary = false`";
      expected = 0;
      actual = builtins.length (assertionFailures {
        entries.probe = {
          settings.a = "a plain string\twith a tab";
          binary = false;
        };
      });
    }
    {
      name = "managedDirectory maps manageRoot/noOwner/xattrs to flags";
      expected = true;
      actual =
        let
          script = directoryFlags {
            manageRoot = true;
            noOwner = true;
            xattrs = "safe";
          };
        in
        lib.hasInfix "--manage-root" script
        && lib.hasInfix "--no-owner" script
        && lib.hasInfix "--xattrs safe" script;
    }
    {
      name = "managedDirectory omits every flag at its defaults";
      expected = false;
      actual =
        let
          script = directoryFlags { };
        in
        lib.hasInfix "--manage-root" script
        || lib.hasInfix "--no-owner" script
        || lib.hasInfix "--xattrs" script;
    }
    {
      name = "cfprefsd path writes binary even when `binary` is false";
      expected = true;
      actual = lib.hasInfix " --plist-format binary" (script {
        cfprefsdDomain = "com.example.app";
        binary = false;
      });
    }
    {
      name = "file path honours `binary = true`";
      expected = true;
      actual = lib.hasInfix " --plist-format binary" (script {
        binary = true;
      });
    }
    {
      # Without the flag the run follows the target's own encoding, which is what
      # keeps a binary Preferences file binary (issue #43).
      name = "file path omits the flag when `binary` is false";
      expected = false;
      actual = lib.hasInfix " --plist-format" (script {
        binary = false;
      });
    }
    {
      name = "file path passes `--follow-symlinks` when `followSymlinks` is set";
      expected = true;
      actual = lib.hasInfix " --follow-symlinks" (script {
        followSymlinks = true;
      });
    }
    {
      name = "file path omits `--follow-symlinks` by default";
      expected = false;
      actual = lib.hasInfix "--follow-symlinks" (script { });
    }
    {
      # The domain path writes a scratch file it made itself, never `target`.
      name = "cfprefsd path ignores `followSymlinks`";
      expected = false;
      actual = lib.hasInfix "--follow-symlinks" (script {
        cfprefsdDomain = "com.example.app";
        followSymlinks = true;
      });
    }
    {
      name = "`followSymlinks` defaults to false";
      expected = false;
      actual = (resolvedEntry "home" { settings.a = 1; }).followSymlinks;
    }
    {
      # Through the real module and a non-plist entry, so the option cannot move
      # into the plist-only block unnoticed.
      name = "a home-manager managedJson entry maps `followSymlinks` to the flag";
      expected = true;
      actual = lib.hasInfix " --follow-symlinks" hmActivation.managedJson.data;
    }
    {
      name = "home-manager runs byte-format and directory entries after linkGeneration";
      expected = {
        managedJson = [
          "writeBoundary"
          "linkGeneration"
        ];
        managedDirectory = [
          "writeBoundary"
          "linkGeneration"
        ];
      };
      actual = lib.mapAttrs (_: activation: activation.after) hmActivation;
    }
    {
      name = "a binary DESIRED runs the entry's own `format` generator";
      expected = true;
      actual =
        desiredDrv {
          binary = true;
          format = markedPlist;
        } != desiredDrv { binary = true; };
    }
    {
      name = "`binary` defaults to true for a cfprefsdDomain entry";
      expected = true;
      actual = resolvedBinary { cfprefsdDomain = "com.example.app"; };
    }
    {
      name = "`binary` defaults to false for a plain file entry";
      expected = false;
      actual = resolvedBinary { settings.a = 1; };
    }
    {
      name = "an explicit `binary = false` still wins on a domain entry";
      expected = false;
      actual = resolvedBinary {
        cfprefsdDomain = "com.example.app";
        binary = false;
      };
    }
    {
      name = "a home entry documents its package default as the module's";
      expected = "config.home.managed.package";
      actual = packageDefaultText (plistEntryType "home");
    }
    {
      name = "a system entry documents its package default as the module's";
      expected = "config.environment.managed.package";
      actual = packageDefaultText (plistEntryType "environment");
    }
    {
      name = "a home managedDirectory entry documents its package default as the module's";
      expected = "config.home.managed.package";
      actual = packageDefaultText (directoryEntryType "home");
    }
    {
      name = "a system managedDirectory entry documents its package default as the module's";
      expected = "config.environment.managed.package";
      actual = packageDefaultText (directoryEntryType "environment");
    }
  ];

  failures = builtins.filter (c: c.actual != c.expected) cases;
  report = c: "  ${c.name}: expected ${builtins.toJSON c.expected}, got ${builtins.toJSON c.actual}";
in
assert lib.assertMsg (failures == [ ]) (
  "module assertions failed:\n" + lib.concatMapStringsSep "\n" report failures
);
pkgs.runCommand "config-graft-module-tests" { } ''
  echo "${toString (builtins.length cases)} module assertions passed" > $out
  cat ${binaryTargetRun} >> $out
  cat ${homeFileMigrationRun} >> $out
''
