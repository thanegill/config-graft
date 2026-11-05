# Nix-side tests for the module layer, exposed as flake `checks` so `nix flake
# check` runs them. `cargo test` covers the engine; nothing there exercises the
# module's own decisions, so a wrong `binary` default or a re-gated
# `--plist-format binary` would reintroduce issue #33 with the Rust suite fully
# green (the engine's floor never runs on a binary write).
#
# Almost everything here is a pure evaluation of `lib/common.nix` -- no platform
# module, no home-manager input -- so the assertions read as plain
# expected/actual. `binaryTargetRun`, `domainRuns` and `runningRuns` are the
# exceptions: they run the generated script, with the real binary, against files
# the builder makes.
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
      skipWhileRunning = [ ];
    }
    // overrides;

  # `pkgs` as a Darwin or a Linux host sees it, for the decisions keyed on the
  # platform: `mkAssertions`' cfprefsd guard and which `pgrep` the script runs.
  hostPkgs =
    darwin:
    pkgs
    // {
      stdenv = pkgs.stdenv // {
        hostPlatform = pkgs.stdenv.hostPlatform // {
          isDarwin = darwin;
          system = if darwin then "aarch64-darwin" else "x86_64-linux";
        };
      };
    };

  scriptOn =
    darwin: overrides:
    common.mkEntryReconcileScript {
      inherit lib;
      pkgs = hostPkgs darwin;
      format = {
        name = "plist";
      };
      entry = entry overrides;
      desired = "/desired.plist";
      target = "/target.plist";
    };

  script = scriptOn true;

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

  # `binary`'s default is computed from `cfprefsdDomain`, so it has to be observed
  # through an evaluated submodule rather than the plain attrset above.
  resolvedBinary =
    overrides:
    let
      evaluated = lib.evalModules {
        modules = [
          {
            options.entries = lib.mkOption {
              type = plistEntryType "home";
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
          inherit lib pkgs;
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

  # The cfprefsd path, run against a stub `defaults` (never the real one) in each
  # of the ways an export can turn out, under both of the shells it runs in:
  # home-manager's `set -euo pipefail`, and the system module's script, which sets
  # none. A failed or empty export must leave the domain alone (issue #59) without
  # aborting what follows; a missing domain, which exports as an empty dictionary,
  # is a first run. An export config-graft cannot read must not be imported either:
  # that stays fatal under home-manager and is skipped by the system script.
  domainEntry = entry {
    package = package;
    settings.a = 1;
    cfprefsdDomain = "com.example.app";
    binary = true;
  };

  # A failing export still leaves a well-formed empty plist behind, so the exit
  # status is the only thing that can tell it from a missing domain.
  stubDefaults = pkgs.writeShellScript "stub-defaults" ''
    echo "$*" >> "$STUB_LOG"
    case "$1:$STUB_EXPORT" in
      export:fail)
        cp "$STUB_DIR/empty.plist" "$3"
        echo "stub: export failed" >&2
        exit 1
        ;;
      export:nothing) ;;
      export:missing) cp "$STUB_DIR/empty.plist" "$3" ;;
      export:existing) cp "$STUB_DIR/existing.plist" "$3" ;;
      export:corrupt) echo "not a plist" > "$3" ;;
      import:*) cp "$3" "$STUB_DIR/imported.plist" ;;
    esac
  '';

  # A generated reconcile script as an activation runs it: with the helpers
  # home-manager provides, under `set -euo pipefail` when `STRICT=1` (home-manager)
  # and without errexit otherwise (the system script), and followed by a marker
  # that only prints if nothing aborted the run.
  scenario =
    name: reconcile:
    pkgs.writeShellScript name ''
      if [[ $STRICT == 1 ]]; then set -euo pipefail; fi
      run() { "$@"; }
      _i() { :; }
      _iWarn() {
        _fmt="$1"
        shift
        printf "warning: $_fmt\n" "$@"
      }
      _prev=""
      ${reconcile}
      echo "reached the next entry"
    '';

  domainScript =
    domainEntry:
    common.mkEntryReconcileScript {
      inherit lib pkgs;
      format = {
        name = "plist";
      };
      entry = domainEntry;
      desired = common.mkDesired {
        inherit lib pkgs;
        format = {
          name = "plist";
        };
        name = "com.example.app.plist";
        entry = domainEntry;
      };
      target = "/unused";
      commands = {
        defaults = stubDefaults;
        pgrep = stubPgrep;
      };
    };

  domainScenario = scenario "cfprefsd-scenario" (domainScript domainEntry);

  domainRuns =
    pkgs.runCommand "config-graft-module-cfprefsd"
      {
        nativeBuildInputs = [ pkgs.libplist ];
      }
      ''
        export STUB_DIR=$PWD STUB_LOG=$PWD/log
        printf '<plist version="1.0"><dict/></plist>' > empty.xml
        printf '<plist version="1.0"><dict><key>appOwned</key><string>x</string></dict></plist>' > existing.xml
        plistutil -f bin -i empty.xml -o empty.plist
        plistutil -f bin -i existing.xml -o existing.plist

        fail() { echo "$1 (export=$STUB_EXPORT strict=$STRICT)"; cat out; exit 1; }
        for STRICT in 1 0; do
          for STUB_EXPORT in fail nothing missing existing corrupt; do
            export STRICT STUB_EXPORT
            rm -f imported.plist imported.xml log
            status=0
            ${domainScenario} > out 2>&1 || status=$?
            if [ "$STUB_EXPORT:$STRICT" = corrupt:1 ]; then
              [ "$status" != 0 ] || fail "a failed graft did not fail the activation"
            else
              [ "$status" = 0 ] || fail "the scenario exited $status"
              grep -q "reached the next entry" out || fail "the script stopped short"
            fi
            case $STUB_EXPORT in
              fail|nothing|corrupt)
                [ ! -e imported.plist ] || fail "imported over the domain after a bad export"
                grep -q "warning: Skipping plist domain com.example.app" out \
                  || fail "skipped without a warning"
                ;;
              missing|existing)
                [ -e imported.plist ] || fail "a good export was not imported"
                plistutil -f xml -i imported.plist -o imported.xml
                grep -q '<key>a</key>' imported.xml || fail "the managed key was not grafted"
                ;;
            esac
            if [ "$STUB_EXPORT" = existing ]; then
              grep -q '<key>appOwned</key>' imported.xml || fail "the domain's own key was dropped"
            fi
          done
        done
        echo "cfprefsd export guard ok" > $out
      '';

  # `skipWhileRunning` on each kind of entry, against a stub `pgrep` answering
  # `STUB_FIRST` for the first name and `STUB_SECOND` for the second: 0 (a match)
  # or 2 (pgrep failed) for either must skip the entry, leave its target alone and
  # name the process that decided it; 1 (no match) for both must reconcile it.
  stubPgrep = pkgs.writeShellScript "stub-pgrep" ''
    echo "$*" >> "$STUB_DIR/pgrep.log"
    case "$3" in
      MeetingBar) exit "$STUB_FIRST" ;;
      *) exit "$STUB_SECOND" ;;
    esac
  '';

  watched = [
    "MeetingBar"
    "Other App"
  ];

  fileScenario = scenario "running-file-scenario" (
    common.mkEntryReconcileScript {
      inherit lib pkgs;
      format = {
        name = "json";
      };
      entry = entry {
        inherit package;
        skipWhileRunning = watched;
      };
      desired = pkgs.writeText "desired.json" ''{"a": 1}'';
      target = "target.json";
      commands.pgrep = stubPgrep;
    }
  );

  runningDomainScenario = scenario "running-cfprefsd-scenario" (
    domainScript (domainEntry // { skipWhileRunning = watched; })
  );

  directoryScenario = scenario "running-directory-scenario" (
    common.mkDirectoryReconcileScript {
      inherit lib pkgs;
      entry = {
        inherit package;
        source = pkgs.runCommand "running-directory-source" { } ''
          mkdir $out
          echo managed > $out/managed.txt
        '';
        manageRoot = false;
        noOwner = true;
        xattrs = "none";
        skipWhileRunning = watched;
      };
      target = "target-dir";
      commands.pgrep = stubPgrep;
    }
  );

  runningRuns =
    pkgs.runCommand "config-graft-module-skip-while-running"
      {
        nativeBuildInputs = [ pkgs.libplist ];
      }
      ''
        export STUB_DIR=$PWD STUB_LOG=$PWD/log STUB_EXPORT=existing
        printf '<plist version="1.0"><dict><key>appOwned</key><string>x</string></dict></plist>' > existing.xml
        plistutil -f bin -i existing.xml -o existing.plist

        fail() { echo "$1 (pgrep=$STUB_FIRST:$STUB_SECOND strict=$STRICT)"; cat out; exit 1; }
        for STRICT in 1 0; do
          for statuses in 0:1 2:1 1:0 1:2 1:1; do
            STUB_FIRST=''${statuses%:*} STUB_SECOND=''${statuses#*:}
            export STRICT STUB_FIRST STUB_SECOND
            rm -rf target.json target-dir imported.plist log pgrep.log out
            echo '{"app": true}' > target.json
            mkdir target-dir
            echo app > target-dir/app.txt

            for run in ${fileScenario} ${runningDomainScenario} ${directoryScenario}; do
              status=0
              "$run" >> out 2>&1 || status=$?
              [ "$status" = 0 ] || fail "$run exited $status"
            done
            [ "$(grep -c "reached the next entry" out)" = 3 ] || fail "a script stopped short"

            if [ "$statuses" = 1:1 ]; then
              grep -q '"a"' target.json || fail "the file entry was not reconciled"
              [ -e imported.plist ] || fail "the domain entry was not reconciled"
              [ -e target-dir/managed.txt ] || fail "the directory entry was not reconciled"
              [ "$(grep -c -- '-x -- Other App' pgrep.log)" = 3 ] \
                || fail "not every name was looked for"
              continue
            fi

            [ "$(cat target.json)" = '{"app": true}' ] || fail "the file entry was touched"
            [ ! -e log ] || fail "defaults ran for a skipped domain entry"
            [ ! -e target-dir/managed.txt ] || fail "the directory entry was touched"
            [ "$(grep -c "Skipping" out)" = 3 ] || fail "a skip went unreported"
            if [ "$STUB_FIRST" = 1 ]; then
              name="Other App" status=$STUB_SECOND
            else
              name=MeetingBar status=$STUB_FIRST
              [ "$(grep -c -- '-x -- Other App' pgrep.log)" = 0 ] \
                || fail "pgrep was asked again after a match"
            fi
            if [ "$status" = 0 ]; then
              expected="Skipping com.example.app: $name is running"
            else
              expected="Skipping target.json: pgrep exited $status looking for $name"
            fi
            grep -qF "warning: $expected" out || fail "no \"$expected\" warning"
          done
        done
        echo "skipWhileRunning ok" > $out
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
    map (a: a.message) (
      builtins.filter (a: !a.assertion) (
        common.mkAssertions {
          inherit lib;
          pkgs = hostPkgs darwin;
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
      inherit lib pkgs;
      entry = {
        package = pkgs.hello;
        source = "/source";
        manageRoot = false;
        noOwner = false;
        xattrs = "all";
        skipWhileRunning = [ ];
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
    {
      name = "an entry without skipWhileRunning looks for no process";
      expected = false;
      actual =
        lib.hasInfix "pgrep" (script { })
        || lib.hasInfix "pgrep" (script {
          cfprefsdDomain = "com.example.app";
        })
        || lib.hasInfix "pgrep" (directoryFlags { });
    }
    {
      # macOS's pgrep matches the basename of argv[0] in full.
      name = "on macOS the whole name is matched, as a literal, by /usr/bin/pgrep";
      expected = true;
      actual = lib.hasInfix "/usr/bin/pgrep -x -- 'Long\\.Process-Name\\.App' " (
        scriptOn true {
          skipWhileRunning = [ "Long.Process-Name.App" ];
        }
      );
    }
    {
      # Linux's matches the kernel comm, which keeps 15 bytes.
      name = "on Linux the name is cut to 15 bytes for procps' pgrep";
      expected = true;
      actual =
        let
          linuxScript = scriptOn false { skipWhileRunning = [ "Long.Process-Name.App" ]; };
        in
        lib.hasInfix (builtins.unsafeDiscardStringContext "${pkgs.procps}/bin/pgrep -x -- 'Long\\.Process-Na' ") linuxScript
        && !(lib.hasInfix "/usr/bin/pgrep" linuxScript);
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
  cat ${domainRuns} >> $out
  cat ${runningRuns} >> $out
''
