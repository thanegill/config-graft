# Nix-side tests for the module layer, exposed as flake `checks` so `nix flake
# check` runs them. `cargo test` covers the engine; nothing there exercises the
# module's own decisions, so a wrong `binary` default or a re-gated
# `--plist-binary` would reintroduce issue #33 with the Rust suite fully green
# (the engine's floor never runs on a binary write).
#
# Everything here is a pure evaluation of `lib/common.nix` -- no platform module,
# no home-manager input -- so the assertions read as plain expected/actual.
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

  # `binary`'s default is computed from `cfprefsdDomain`, so it has to be observed
  # through an evaluated submodule rather than the plain attrset above.
  resolvedBinary =
    overrides:
    let
      entryType = common.entryType {
        inherit lib pkgs;
        defaultPackage = pkgs.hello;
        format = {
          name = "plist";
          fileExtension = "plist";
          settingsExample = { };
        };
        targetOption = lib.mkOption { type = lib.types.str; };
        cfprefsdDescription = "test";
      };
      evaluated = lib.evalModules {
        modules = [
          {
            options.entries = lib.mkOption {
              type = entryType;
              default = { };
            };
          }
          { entries.probe = overrides; }
        ];
      };
    in
    evaluated.config.entries.probe.binary;

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
''
