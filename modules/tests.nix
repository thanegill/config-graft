# Nix-side tests for the module layer, exposed as flake `checks` so `nix flake
# check` runs them. `cargo test` covers the engine; nothing there exercises the
# module's own decisions, so a wrong `binary` default or a re-gated
# `--plist-format binary` would reintroduce issue #33 with the Rust suite fully
# green (the engine's floor never runs on a binary write).
#
# Almost everything here is a pure evaluation of `lib/common.nix`, so the
# assertions read as plain expected/actual. `binaryTargetRun`,
# `homeFileMigrationRun`, `domainRuns`, `runningRuns`, `restartRuns` and
# `failingSystemRun` are the exceptions: they run the generated script, with the
# real binary, against files the builder makes, and `patternRuns` runs it against
# a logging `pgrep` alone. `hmActivation` evaluates the home-manager module
# itself, with home-manager's own pieces stubbed, and `failingSystemRun` evaluates
# the system module itself.
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
    // overrides
    // {
      whileRunning = whileRunning (overrides.whileRunning or { });
    };

  # `whileRunning` as the submodule would fill it in.
  whileRunning =
    overrides:
    {
      skip = [ ];
      restart = [ ];
      quitTimeout = 10;
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

  # A file entry's script with `whileRunning.restart` set, as `user` would run it
  # (null for home-manager).
  restartScript =
    user:
    common.mkEntryReconcileScript {
      inherit lib pkgs;
      format = {
        name = "plist";
      };
      entry = entry { whileRunning.restart = [ "com.example.First" ]; };
      desired = "/desired.plist";
      target = "/target.plist";
      restartUser = user;
    };

  # The messages of the `whileRunning.restart` assertions that fail for one entry
  # that sets it.
  restartFailures =
    {
      darwin ? true,
      needsUser ? false,
      restartUser ? null,
    }:
    map (a: a.message) (
      builtins.filter (a: !a.assertion) (
        common.mkRestartAssertions {
          inherit lib needsUser restartUser;
          pkgs = hostPkgs darwin;
          parent = "environment";
          optionName = "managedPlist";
          entries.probe = entry { whileRunning.restart = [ "com.example.First" ]; };
        }
      )
    );

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
  # that stays fatal under home-manager, and the system script skips the domain
  # while its ERR trap counts the failure (see `failingSystemRun`).
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
      export:importfails) cp "$STUB_DIR/existing.plist" "$3" ;;
      import:importfails)
        echo "stub: import failed" >&2
        exit 1
        ;;
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
          for STUB_EXPORT in fail nothing missing existing corrupt importfails; do
            export STRICT STUB_EXPORT
            rm -f imported.plist imported.xml log
            status=0
            ${domainScenario} > out 2>&1 || status=$?
            if [ "$STUB_EXPORT:$STRICT" = corrupt:1 ] || [ "$STUB_EXPORT:$STRICT" = importfails:1 ]; then
              [ "$status" != 0 ] || fail "a failed graft or import did not fail the activation"
            else
              [ "$status" = 0 ] || fail "the scenario exited $status"
              grep -q "reached the next entry" out || fail "the script stopped short"
            fi
            case $STUB_EXPORT in
              importfails)
                [ ! -e imported.plist ] || fail "the stub imported after failing"
                ;;
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

  # The home-manager module's activation entries, evaluated without a home-manager
  # input: `lib.hm.dag.entryAfter` and the few options the module writes are
  # stubbed, which is enough to read each entry's ordering.
  hmConfig =
    home:
    let
      hmLib = lib.extend (
        _: _: {
          hm.dag.entryAfter = after: data: { inherit after data; };
        }
      );
      stub = type: lib.mkOption { inherit type; };
    in
    (hmLib.evalModules {
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
            home = {
              homeDirectory = "/home/probe";
            }
            // home;
          };
        }
      ];
    }).config;

  hmActivation =
    (hmConfig {
      managedJson.probe = {
        settings.a = 1;
        followSymlinks = true;
      };
      managedDirectory.tree.source = "/source";
    }).home.activation;

  # The messages of the assertions that fail for a configuration.
  failedAssertions = config: map (a: a.message) (builtins.filter (a: !a.assertion) config.assertions);

  # An entry moved from `home.file` to `managedJson`: until home-manager's
  # `linkGeneration` removes it, the target is still the old generation's link into
  # the store. The module's script must refuse that link rather than write through
  # it, and once the link is gone must create a regular file -- which is why the
  # activation is ordered after `linkGeneration` (`hmActivation` above).
  homeFileMigrationRun =
    let
      oldHomeFile = pkgs.writeText "old-home-file.json" ''{"a":0}'';
      reconcile = common.mkEntryReconcileScript {
        inherit lib pkgs;
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

  # `whileRunning.skip` on each kind of entry, against a stub `pgrep` answering
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
        whileRunning.skip = watched;
      };
      desired = pkgs.writeText "desired.json" ''{"a": 1}'';
      target = "target.json";
      commands.pgrep = stubPgrep;
    }
  );

  runningDomainScenario = scenario "running-cfprefsd-scenario" (
    domainScript (domainEntry // { whileRunning = whileRunning { skip = watched; }; })
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
        whileRunning = whileRunning { skip = watched; };
      };
      target = "target-dir";
      commands.pgrep = stubPgrep;
    }
  );

  # The pgrep pattern `whileRunning.skip` builds at run time, for each name, must
  # be what `lib.escapeRegex` makes of it: the whole name on macOS, whose pgrep
  # matches the basename of argv[0] in full, and its first 15 bytes on Linux,
  # whose pgrep matches the kernel comm. The names hold every character the
  # escape touches and some it must leave alone, a space, a quote, a trailing
  # newline, and non-ASCII text, one name cut through the middle of a character.
  # The stub pgrep logs each pattern and reports nothing running.
  patternNames = [
    "Long.Process-Name.App"
    "a\\b[c{d(e)f^g$h?i*j+k|l.m]n}o"
    "Other App"
    "it's"
    "Trailing\n"
    "short"
    "abcdefghijklmn${builtins.fromJSON ''"\u00fc"''}x"
    "${builtins.fromJSON ''"\u00dcn\u00efc\u00f6d\u00e9"''}-Prozess-Name"
  ];

  patternRuns =
    let
      loggingPgrep = pkgs.writeShellScript "logging-pgrep" ''
        printf '%s\n' "$3" >> patterns
        exit 1
      '';
      expected =
        darwin:
        pkgs.writeText "expected-patterns" (
          lib.concatMapStrings (
            name: lib.escapeRegex (if darwin then name else builtins.substring 0 15 name) + "\n"
          ) patternNames
        );
      guarded =
        darwin:
        common.mkEntryReconcileScript {
          inherit lib;
          pkgs = hostPkgs darwin;
          format = {
            name = "json";
          };
          entry = entry { whileRunning.skip = patternNames; };
          desired = "/desired.json";
          target = "/target.json";
          commands.pgrep = loggingPgrep;
        };
      check = darwin: ''
        rm -f patterns reconciled
        run() { touch reconciled; }
        _i() { :; }
        _prev=""
        ${guarded darwin}
        cmp patterns ${expected darwin} \
          || { echo "patterns (darwin=${lib.boolToString darwin}) differ:"; diff patterns ${expected darwin}; exit 1; }
        [ -e reconciled ] || { echo "nothing was running, yet the entry was skipped"; exit 1; }
      '';
    in
    pkgs.runCommand "config-graft-module-skip-patterns" { } ''
      # An activation runs in the user's locale; the cut must still be in bytes.
      export LC_ALL=C.UTF-8
      ${check true}
      ${check false}
      echo "whileRunning.skip patterns ok" > $out
    '';

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
        echo "whileRunning.skip ok" > $out
      '';

  # `whileRunning.restart` against stub `osascript`, `open` and `sleep` (never the
  # real ones). Each app's running state is a file the stubs share: `osascript`
  # answers `is running` from it (or fails, `queryfails`, or fails as an app that
  # is not installed does, `notinstalled`, -1728) and, told to quit, clears it
  # (`ok`), leaves it set (`stuck`), clears it but reports the reply timing out
  # (`slow`, -1712) or the connection dropping (`dies`, -609), keeps answering
  # `true` from the first check after the quit and while the poll sleeps between
  # checks, then exits at the first check not preceded by a sleep (`late`: after
  # the poll gives up, by either bound), or refuses with the Automation error
  # (`denied`), per app; `open` sets it again and logs the relaunch. The stub
  # `sleep` only marks that a sleep happened.
  stubOsascript = pkgs.writeShellScript "stub-osascript" ''
    script="$*"
    id=''${script#*application id \"}
    id=''${id%%\"*}
    echo "$script" >> "$STUB_DIR/osascript.log"
    case "$id" in
      com.example.First) mode=$STUB_QUIT_FIRST ;;
      *) mode=$STUB_QUIT_SECOND ;;
    esac
    case "$script" in
      *"is running"*)
        if [ "$mode" = queryfails ]; then
          echo "execution error: Connection is invalid. (-609)" >&2
          exit 1
        fi
        if [ "$mode" = notinstalled ]; then
          # As macOS prints it, curly apostrophe (U+2019) and all.
          printf 'syntax error: Can\342\200\231t get application id "%s". (-1728)\n' "$id" >&2
          exit 1
        fi
        echo "warning: noise on stderr" >&2
        if [ -f "$STUB_DIR/late.$id" ]; then
          if [ "$(cat "$STUB_DIR/late.$id")" = quit ]; then
            echo polling > "$STUB_DIR/late.$id"
          elif ! rm "$STUB_DIR/slept" 2>/dev/null; then
            echo false > "$STUB_DIR/app.$id"
          fi
        fi
        cat "$STUB_DIR/app.$id"
        ;;
      *"to quit"*)
        case "$mode" in
          ok) echo false > "$STUB_DIR/app.$id" ;;
          stuck) ;;
          slow)
            echo false > "$STUB_DIR/app.$id"
            echo "execution error: AppleEvent timed out. (-1712)" >&2
            exit 1
            ;;
          dies)
            echo false > "$STUB_DIR/app.$id"
            echo "execution error: Connection is invalid. (-609)" >&2
            exit 1
            ;;
          late) echo quit > "$STUB_DIR/late.$id" ;;
          denied)
            echo "execution error: Not authorized to send Apple events to $id. (-1743)" >&2
            exit 1
            ;;
        esac
        ;;
    esac
  '';

  stubOpen = pkgs.writeShellScript "stub-open" ''
    echo "$*" >> "$STUB_DIR/open.log"
    echo true > "$STUB_DIR/app.''${*: -1}"
  '';

  restartScenario = scenario "restart-scenario" (
    common.mkEntryReconcileScript {
      inherit lib pkgs;
      format = {
        name = "json";
      };
      entry = entry {
        inherit package;
        whileRunning = {
          skip = [ "MeetingBar" ];
          restart = [
            "com.example.First"
            "com.example.Second"
          ];
          quitTimeout = 1;
        };
      };
      desired = pkgs.writeText "desired.json" ''{"a": 1}'';
      target = "target.json";
      commands = {
        pgrep = stubPgrep;
        osascript = stubOsascript;
        open = stubOpen;
        sleep = pkgs.writeShellScript "stub-sleep" ''touch "$STUB_DIR/slept"'';
      };
    }
  );

  # One case per line: name, then each app's state before the run and how it
  # answers a quit, then what must happen. `relaunched` lists the apps `open` must
  # be called for, `grafted` whether the target was reconciled, and `warning` a
  # substring the skip warning must carry (`-` for no warning).
  restartRuns =
    pkgs.runCommand "config-graft-module-restart"
      {
        nativeBuildInputs = [ pkgs.jq ];
      }
      ''
        export STUB_DIR=$PWD STUB_FIRST=1 STUB_SECOND=1
        fail() { echo "$1 ($case strict=$STRICT)"; cat out; exit 1; }
        while read -r case first second quitFirst quitSecond target grafted relaunched warning; do
          for STRICT in 1 0; do
            export STRICT STUB_QUIT_FIRST=$quitFirst STUB_QUIT_SECOND=$quitSecond
            rm -f osascript.log open.log pgrep.log out late.* slept
            echo "$first" > app.com.example.First
            echo "$second" > app.com.example.Second
            if [ "$target" = bad ]; then echo 'not json' > target.json; else echo '{"app": true}' > target.json; fi
            [ "$case" = skip ] && export STUB_FIRST=0 || export STUB_FIRST=1
            [ "$case" = dryRun ] && export DRY_RUN=1 || unset DRY_RUN

            status=0
            ${restartScenario} > out 2>&1 || status=$?
            if [ "$target" = bad ] && [ "$STRICT" = 1 ]; then
              [ "$status" != 0 ] || fail "a failed reconcile did not fail the activation"
            else
              [ "$status" = 0 ] || fail "the scenario exited $status"
              grep -q "reached the next entry" out || fail "the script stopped short"
            fi

            if [ "$grafted" = yes ]; then
              [ "$(jq -r .a target.json)" = 1 ] || fail "the entry was not reconciled"
            elif [ "$target" = good ]; then
              [ "$(cat target.json)" = '{"app": true}' ] || fail "a skipped entry was reconciled"
            fi

            touch open.log
            actual=$(sed 's/^-g -b com\.example\.//' open.log | paste -sd , -)
            [ "''${actual:--}" = "$relaunched" ] || fail "relaunched ''${actual:--}, not $relaunched"

            if [ "$warning" = - ]; then
              ! grep -q "warning: Skipping" out || fail "skipped without cause"
            else
              grep -qF "''${warning//_/ }" out || fail "no warning carrying \"''${warning//_/ }\""
            fi
            case "$case:$first:$second" in
              skip:* | dryRun:* | *:false:false)
                ! grep -q "to quit" osascript.log 2>/dev/null || fail "asked an app to quit"
                ;;
              notInstalled:*)
                ! grep -q 'First" to quit' osascript.log || fail "asked an app that is not installed to quit"
                ;;
              firstStuck:* | queryFails:*)
                ! grep -q 'Second" to quit' osascript.log || fail "went on to Second after First was blocked"
                ;;
            esac
            if [ "$case" = queryFails ]; then
              grep -qF "(-609)" out || fail "the failed check's stderr was not shown"
            fi
            [ "$case" != skip ] || [ ! -e osascript.log ] || fail "restart ran after a skip"
          done
        done <<'CASES'
        quits true true ok ok good yes First,Second -
        notRunning false false ok ok good yes - -
        oneRunning false true ok ok good yes Second -
        timesOut true true ok stuck good no First com.example.Second_did_not_quit_within_1_second
        firstStuck true true stuck ok good no - com.example.First_did_not_quit_within_1_second
        slowReply true false slow ok good yes First -
        dies true false dies ok good no First asking_com.example.First_to_quit_failed
        lateExit true false late ok good no First com.example.First_did_not_quit_within_1_second
        denied true true ok denied good no First Privacy_&_Security_>_Automation
        queryFails true true queryfails ok good no - could_not_tell_whether_com.example.First_is_running
        notInstalled true true notinstalled ok good yes Second -
        graftFails true false ok ok bad no First -
        skip true true ok ok good no - MeetingBar_is_running
        dryRun true true ok ok good yes - -
        CASES
        echo "whileRunning.restart ok" > $out
      '';

  # The system module's activation script, from a real evaluation of
  # `lib/system.nix` with `managed` as its `environment` settings, so what runs is
  # the wiring around the shared reconcile bodies: the shims, the BASE probe and the
  # failure handling. The few platform options the module sets are declared here as
  # plain sinks.
  systemConfig =
    managed:
    (lib.evalModules {
      modules = [
        (
          { config, ... }:
          import ./lib/system.nix {
            inherit config lib pkgs;
            defaultPackage = package;
            activationWiring = script: { config-graft = script; };
          }
        )
        {
          options = {
            system.activationScripts = lib.mkOption {
              type = lib.types.attrsOf lib.types.raw;
              default = { };
            };
            system.systemBuilderCommands = lib.mkOption {
              type = lib.types.lines;
              default = "";
            };
            assertions = lib.mkOption {
              type = lib.types.listOf lib.types.raw;
              default = [ ];
            };
            environment.etc = lib.mkOption {
              type = lib.types.attrsOf lib.types.raw;
              default = { };
            };
          };
        }
        { environment = managed; }
      ];
    }).config;

  systemActivation = managed: (systemConfig managed).system.activationScripts.config-graft;

  # A failing entry fails the system activation without stopping the entries after
  # it: NixOS reports the snippet and fails the switch, nix-darwin aborts at the
  # end. The script is the module's own, copied with `/usr/bin/defaults` and
  # `pgrep` pointed at the stubs. `bad.json` sorts first and holds no JSON, so
  # config-graft refuses it; a cfprefsd graft that fails counts as a failure, while
  # a skip and a pgrep probe that finds nothing do not.
  systemScenario = systemActivation {
    managedJson = {
      "bad.json".settings.a = 1;
      "good.json".settings.a = 1;
    };
    managedPlist."com.example.app.plist" = {
      cfprefsdDomain = "com.example.app";
      settings.a = 1;
      whileRunning.skip = [ "MeetingBar" ];
    };
  };

  failingSystemRun =
    pkgs.runCommand "config-graft-module-system-failure"
      {
        nativeBuildInputs = [ pkgs.libplist ];
      }
      ''
        export STUB_DIR=$PWD STUB_LOG=$PWD/log STUB_FIRST=1
        printf '<plist version="1.0"><dict/></plist>' > empty.xml
        plistutil -f bin -i empty.xml -o empty.plist
        printf '<plist version="1.0"><dict><key>appOwned</key><string>x</string></dict></plist>' > existing.xml
        plistutil -f bin -i existing.xml -o existing.plist
        sed -e 's|/usr/bin/defaults|${stubDefaults}|g' \
          -e 's|[^ ]*/bin/pgrep |${stubPgrep} |g' ${systemScenario} > activate
        chmod +x activate
        grep -q '${stubPgrep} -x' activate || { echo "pgrep was not stubbed"; exit 1; }

        fail() { echo "$1"; cat out; exit 1; }
        expect() {
          echo '{"app": true}' > good.json
          rm -f imported.plist
          status=0
          ./activate > out 2>&1 || status=$?
          [ "$status" = "$1" ] || fail "$2: exited $status, expected $1"
          grep -q '"a"' good.json || fail "$2: good.json was not reconciled"
        }

        echo 'not json' > bad.json
        STUB_EXPORT=existing expect 1 "a refused file entry"
        [ "$(cat bad.json)" = 'not json' ] || fail "the refused target was touched"
        [ -e imported.plist ] || fail "the domain after the failure was not reconciled"
        grep -q "a managed entry failed" out || fail "the failure went unreported"

        echo '{"app": true}' > bad.json
        rm -f log
        STUB_EXPORT=existing expect 0 "a clean run"
        [ -e imported.plist ] || fail "a clean run did not reconcile the domain"
        for verb in export import; do
          grep -q "^$verb /Library/Preferences/com.example.app " log \
            || fail "$verb did not address the system-wide domain by path"
        done
        STUB_EXPORT=corrupt expect 1 "a failed cfprefsd graft"
        [ ! -e imported.plist ] || fail "a failed cfprefsd graft was imported"
        STUB_EXPORT=importfails expect 1 "a failed cfprefsd import"
        [ ! -e imported.plist ] || fail "the stub imported after failing"
        STUB_EXPORT=fail expect 0 "a skipped export"
        [ ! -e imported.plist ] || fail "a skipped export was imported"
        STUB_FIRST=0 STUB_EXPORT=existing expect 0 "a skip while running"
        [ ! -e imported.plist ] || fail "a running app's domain was reconciled"
        echo "system failure handling ok" > $out
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
        whileRunning = whileRunning { };
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
      name = "home-manager entries whose names share a snapshot path are rejected";
      expected = 1;
      actual = builtins.length (
        failedAssertions (hmConfig {
          managedJson."a/b.json".settings.a = 1;
          managedJson."a-b.json".settings.a = 1;
          managedJson."c.json".settings.a = 1;
        })
      );
    }
    {
      name = "a relative system target is rejected, and a domain entry's ignored one is not";
      expected = 2;
      actual = builtins.length (
        builtins.filter (lib.hasInfix "which is not an") (
          failedAssertions (systemConfig {
            managedJson."etc/app.json".settings.a = 1;
            managedDirectory."srv/tree".source = "/source";
            managedJson."/etc/ok.json".settings.a = 1;
            managedPlist."com.example.app" = {
              cfprefsdDomain = "com.example.app";
              settings.a = 1;
            };
          })
        )
      );
    }
    {
      name = "system entries whose names share a snapshot path are rejected";
      expected = 1;
      actual = builtins.length (
        failedAssertions (systemConfig {
          managedDirectory."/srv/a/b".source = "/source";
          managedDirectory."/srv-a-b".source = "/source";
        })
      );
    }
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
      name = "an XML-illegal character with `binary = false` is rejected";
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
      # Run as root, a bare id would be root's own user domain.
      name = "a system cfprefsdDomain id becomes its /Library/Preferences path";
      expected = [
        "/Library/Preferences/com.example.app"
        "/Library/Preferences/.GlobalPreferences"
        "/Library/Preferences/.GlobalPreferences"
        "/Library/Preferences/.GlobalPreferences"
        "/Library/Preferences/Custom/com.example.app"
        "relative/com.example.app"
      ];
      actual = map common.systemDomain [
        "com.example.app"
        "NSGlobalDomain"
        "-g"
        "-globalDomain"
        "/Library/Preferences/Custom/com.example.app"
        "relative/com.example.app"
      ];
    }
    {
      # home-manager runs as the user, whose own domain a bare id already is.
      name = "a home-manager cfprefsdDomain stays a bare id";
      expected = true;
      actual = lib.hasInfix "_domain=com.example.app\n" (script {
        cfprefsdDomain = "com.example.app";
      });
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
      name = "an entry without whileRunning.skip looks for no process";
      expected = false;
      actual =
        lib.hasInfix "pgrep" (script { })
        || lib.hasInfix "pgrep" (script {
          cfprefsdDomain = "com.example.app";
        })
        || lib.hasInfix "pgrep" (directoryFlags { });
    }
    {
      name = "an entry without whileRunning.restart quits nothing";
      expected = false;
      actual =
        lib.hasInfix "osascript" (script { })
        || lib.hasInfix "osascript" (script {
          cfprefsdDomain = "com.example.app";
        })
        || lib.hasInfix "osascript" (directoryFlags { });
    }
    {
      # nix-darwin's own pattern for acting as `system.primaryUser`.
      name = "a system restart quits and relaunches as system.primaryUser";
      expected = true;
      actual =
        let
          asAlice = ''/bin/launchctl asuser "$(/usr/bin/id -u -- alice)" /usr/bin/sudo --user=alice -- '';
          systemScript = restartScript "alice";
        in
        lib.hasInfix "${asAlice}/usr/bin/osascript -e 'application id \"com.example.First\" is running'" systemScript
        && lib.hasInfix "${asAlice}/usr/bin/osascript -e 'with timeout of 10 seconds' -e 'tell application id \"com.example.First\" to quit'" systemScript
        && lib.hasInfix "${asAlice}/usr/bin/open -g -b com.example.First" systemScript
        && !(lib.hasInfix "DRY_RUN" systemScript);
    }
    {
      name = "a home-manager restart runs osascript and open as the activating user";
      expected = true;
      actual =
        let
          homeScript = restartScript null;
        in
        !(lib.hasInfix "launchctl" homeScript)
        && lib.hasInfix "/usr/bin/open -g -b com.example.First" homeScript
        && lib.hasInfix "[[ -v DRY_RUN ]]" homeScript;
    }
    {
      # Once, for the platform: NixOS has no system.primaryUser to point at.
      name = "restart on a non-Darwin host is rejected";
      expected = 1;
      actual = builtins.length (restartFailures {
        darwin = false;
        needsUser = true;
      });
    }
    {
      name = "a system restart without system.primaryUser is rejected";
      expected = 1;
      actual = builtins.length (restartFailures {
        needsUser = true;
      });
    }
    {
      name = "a system restart with system.primaryUser is accepted";
      expected = 0;
      actual = builtins.length (restartFailures {
        needsUser = true;
        restartUser = "alice";
      });
    }
    {
      name = "a home-manager restart needs no user";
      expected = 0;
      actual = builtins.length (restartFailures { });
    }
    {
      # What each pattern holds is `patternRuns`' to check; this is which pgrep.
      name = "macOS runs /usr/bin/pgrep and Linux procps' pgrep";
      expected = true;
      actual =
        let
          linuxScript = scriptOn false { whileRunning.skip = [ "App" ]; };
        in
        lib.hasInfix "/usr/bin/pgrep -x -- " (scriptOn true { whileRunning.skip = [ "App" ]; })
        && lib.hasInfix (builtins.unsafeDiscardStringContext "${pkgs.procps}/bin/pgrep -x -- ") linuxScript
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
  cat ${patternRuns} >> $out
  cat ${homeFileMigrationRun} >> $out
  cat ${restartRuns} >> $out
  cat ${failingSystemRun} >> $out
''
