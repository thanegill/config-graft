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
  configGraftLib = import ./lib;

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
          hm.dag = {
            entryAfter = after: data: {
              inherit after data;
              before = [ ];
            };
            entryBetween = before: after: data: { inherit before after data; };
          };
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
            # A link's `target` defaults to its name, as home-manager's does.
            file = stub (
              lib.types.attrsOf (
                lib.types.submodule (
                  { name, ... }:
                  {
                    freeformType = lib.types.attrsOf lib.types.raw;
                    options.target = lib.mkOption { default = name; };
                    options.enable = lib.mkOption { default = true; };
                  }
                )
              )
            );
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

  # The prune of removed entries, run against a previous generation the builder
  # lays out: a manifest written by `mkManifest` from units built by `mkUnit` and
  # `mkDirectoryUnit`, the snapshots it names, and a target for each. Paths start
  # at `/cg-test-root`, which the builder rewrites to its own directory in the
  # manifest and in the script, since a unit's identity is absolute. `defaults` and
  # `pgrep` are stubs, and config-graft is wrapped to log its arguments, so the
  # flags each unit's meaning maps to are checked against what the live entries
  # pass. Both shells run it: home-manager's `set -euo pipefail` and the system
  # script's none.
  pruneRoot = "/cg-test-root";

  pruneUnit =
    name: overrides:
    common.mkUnit {
      format = {
        inherit name;
      };
      entry = entry overrides;
      identity = "${pruneRoot}/home/${overrides.file}";
      snapshot = "snap/${overrides.snap or overrides.file}";
      domain = overrides.domain or null;
    };

  domainUnit =
    domain: whileRunning:
    common.mkUnit {
      format.name = "plist";
      entry = entry {
        cfprefsdDomain = domain;
        binary = true;
        inherit whileRunning;
      };
      identity = "${pruneRoot}/prefs/${domain}.plist";
      snapshot = "snap/${domain}";
      inherit domain;
    };

  stillUnit = pruneUnit "json" { file = "still.json"; };

  previousUnits = [
    (pruneUnit "json" { file = "removed.json"; })
    stillUnit
    (pruneUnit "json" {
      file = "renamed.json";
      snap = "renamed-old.json";
    })
    (pruneUnit "json" {
      file = "gone/gone.json";
      snap = "gone.json";
    })
    (pruneUnit "json" { file = "nosnap.json"; })
    (pruneUnit "json" { file = "bad.json"; })
    (pruneUnit "yaml" { file = "removed.yaml"; })
    (pruneUnit "toml" { file = "removed.toml"; })
    (pruneUnit "plist" { file = "removed.plist"; })
    (pruneUnit "plist" {
      file = "binary.plist";
      binary = true;
    })
    (pruneUnit "json" {
      file = "followed.json";
      followSymlinks = true;
    })
    (pruneUnit "json" { file = "linked.json"; })
    (pruneUnit "json" {
      file = "store.json";
      followSymlinks = true;
    })
    (pruneUnit "json" { file = "claimed.json"; })
    (pruneUnit "json" {
      file = "running.json";
      whileRunning.skip = [ "MeetingBar" ];
    })
    (domainUnit "com.example.present" { })
    (domainUnit "com.example.absent" { })
    (domainUnit "com.example.exportfails" { })
    (domainUnit "com.example.corrupt" { })
    (domainUnit "com.example.running" { skip = [ "MeetingBar" ]; })
    (domainUnit "com.example.restarting" { restart = [ "com.example.Running" ]; })
    (domainUnit "com.example.uninstalled" { restart = [ "com.example.NotInstalled" ]; })
    (domainUnit "com.example.badid" { restart = [ "not an id" ]; })
    (pruneUnit "json" {
      file = "old-target.json";
      snap = "retarget.json";
    })
    (stillUnit // { identity = "relative.json"; })
    (common.mkDirectoryUnit {
      entry = {
        noOwner = true;
        xattrs = "none";
        whileRunning = whileRunning { };
      };
      identity = "${pruneRoot}/home/tree";
      snapshot = "snap/tree";
    })
    (common.mkDirectoryUnit {
      entry = {
        noOwner = false;
        xattrs = "all";
        whileRunning = whileRunning { };
      };
      identity = "${pruneRoot}/home/treelink";
      snapshot = "snap/tree";
    })
    (common.mkDirectoryUnit {
      entry = {
        noOwner = false;
        xattrs = "all";
        whileRunning = whileRunning { };
      };
      identity = "${pruneRoot}/home/notadir";
      snapshot = "snap/tree";
    })
    (
      stillUnit
      // {
        kind = "ini";
        identity = "${pruneRoot}/home/unknown.ini";
      }
    )
  ];

  previousManifest = pkgs.writeText "previous-manifest.json" (common.mkManifest previousUnits);

  pruneDefaults = pkgs.writeShellScript "prune-defaults" ''
    echo "$*" >> "$STUB_DIR/defaults.log"
    case "$1:$2" in
      read:com.example.absent) exit 1 ;;
      read:*) ;;
      export:com.example.exportfails) exit 1 ;;
      export:com.example.corrupt) echo "not a plist" > "$3" ;;
      export:*) cp "$STUB_DIR/domain-live.plist" "$3" ;;
      import:*) cp "$3" "$STUB_DIR/imported-$2.plist" ;;
    esac
  '';

  prunePgrep = pkgs.writeShellScript "prune-pgrep" ''
    [ "$3" = MeetingBar ] && exit 0
    exit 1
  '';

  # Asked whether an app runs: `com.example.Running` does, `com.example.NotInstalled`
  # fails as an app that is not installed does (-1728), and any other is not
  # running. Anything else (a quit) is logged as a failure of the test.
  pruneOsascript = pkgs.writeShellScript "prune-osascript" ''
    echo "$*" >> "$STUB_DIR/osascript.log"
    case "$*" in
      *'"com.example.Running" is running') echo true ;;
      *'"com.example.NotInstalled" is running')
        echo 'execution error: Can not get application id "com.example.NotInstalled". (-1728)' >&2
        exit 1
        ;;
      *' is running') echo false ;;
      *) echo "unexpected osascript: $*" >> "$STUB_DIR/unexpected.log" ;;
    esac
  '';

  loggingPackage = pkgs.writeShellScriptBin "config-graft" ''
    printf '%s\n' "$*" >> "$STUB_DIR/argv.log"
    exec ${lib.getExe package} "$@"
  '';

  pruneScenario = scenario "prune-scenario" ''
    ${common.mkPruneScript {
      inherit lib pkgs;
      inherit (configGraftLib) formats;
      package = loggingPackage;
      prevRoot = "$PWD/prev";
      hasPrev = "[[ -z \${NO_PREV-} ]]";
      manifestRel = "manifest.json";
      units = [
        stillUnit
        (pruneUnit "json" {
          file = "renamed.json";
          snap = "renamed-new.json";
        })
        (pruneUnit "json" {
          file = "new-target.json";
          snap = "retarget.json";
        })
      ];
      claimed = [ "${pruneRoot}/home/claimed.json" ];
      claimedBy = "home.file";
      commands = {
        defaults = pruneDefaults;
        pgrep = prunePgrep;
        osascript = pruneOsascript;
      };
    }}
    if [[ $STRICT == 1 && $- != *e* ]]; then echo "errexit was not restored"; fi
  '';

  pruneRuns =
    pkgs.runCommand "config-graft-module-prune"
      {
        nativeBuildInputs = [
          pkgs.libplist
          pkgs.jq
        ];
      }
      ''
        export STUB_DIR=$PWD
        sed "s|${pruneRoot}|$PWD/root|g" ${pruneScenario} > prune.sh
        chmod +x prune.sh
        storeJson=${pkgs.writeText "store.json" ''{"managed":1}''}

        plist() { printf '<plist version="1.0"><dict>%s</dict></plist>' "$2" > "$1.xml"; plistutil -f bin -i "$1.xml" -o "$1"; }
        setup() {
          rm -rf prev root defaults.log argv.log osascript.log unexpected.log imported-*.plist out
          mkdir -p prev/snap root/home root/prefs root/dotfiles
          sed "s|${pruneRoot}|$PWD/root|g" ${previousManifest} > prev/manifest.json
          h=root/home
          for f in removed still renamed-old bad followed linked store claimed running retarget; do
            echo '{"managed":1,"edited":5}' > prev/snap/$f.json
          done
          echo '{"managed":1}' > prev/snap/gone.json
          echo '{"managed":1,"edited":9,"app":2}' > $h/removed.json
          echo '{"managed":1,"app":3}' > $h/still.json
          echo '{"managed":1,"app":3}' > $h/renamed.json
          echo '{"managed":1,"app":4}' > $h/nosnap.json
          echo 'not json' > $h/bad.json
          echo '{"managed":1,"app":5}' > root/dotfiles/followed.json
          ln -s ../dotfiles/followed.json $h/followed.json
          echo '{"managed":1,"app":6}' > root/dotfiles/linked.json
          ln -s ../dotfiles/linked.json $h/linked.json
          ln -s $storeJson $h/store.json
          echo '{"managed":1,"app":7}' > $h/claimed.json
          echo '{"managed":1,"app":8}' > $h/running.json
          echo '{"managed":1,"app":9}' > $h/old-target.json
          echo 'a file, not a tree' > $h/notadir
          printf 'managed: 1\n' > prev/snap/removed.yaml
          printf 'managed: 1\n' > $h/removed.yaml
          printf 'managed = 1\n' > prev/snap/removed.toml
          printf 'managed = 1\n# app\napp = 2\n' > $h/removed.toml
          plist prev/snap/removed.plist '<key>managed</key><integer>1</integer>'
          cp prev/snap/removed.plist prev/snap/binary.plist
          for d in present absent exportfails corrupt running restarting uninstalled badid; do
            cp prev/snap/removed.plist prev/snap/com.example.$d
          done
          plist $h/removed.plist '<key>managed</key><integer>1</integer><key>app</key><integer>2</integer>'
          cp $h/removed.plist $h/binary.plist
          plist domain-live.plist '<key>managed</key><integer>1</integer><key>appOwned</key><string>x</string>'
          mkdir -p prev/snap/tree/sub $h/tree/sub root/dotfiles/tree
          echo v1 > prev/snap/tree/managed.txt
          echo v2 > prev/snap/tree/sub/nested.txt
          cp -r prev/snap/tree/. $h/tree/
          cp -r prev/snap/tree/. root/dotfiles/tree/
          echo app > $h/tree/app.txt
          chmod 700 $h/tree
          ln -s ../dotfiles/tree $h/treelink
          cp $h/still.json still.before
        }
        fail() { echo "$1 (strict=$STRICT)"; cat out; exit 1; }
        json() { jq -cS . "$1"; }
        warned() { grep -qF "warning: $1" out || fail "no warning: $1"; }

        for STRICT in 1 0; do
          export STRICT
          setup
          status=0
          ./prune.sh > out 2>&1 || status=$?
          [ "$status" = 0 ] || fail "the prune exited $status"
          grep -q "reached the next entry" out || fail "the prune stopped short"
          ! grep -q "errexit was not restored" out || fail "errexit was not restored"
          h=root/home

          [ "$(json $h/removed.json)" = '{"app":2,"edited":9}' ] || fail "removed.json was not pruned to the app's keys"
          cmp -s still.before $h/still.json || fail "a still-managed entry was touched"
          [ "$(json $h/renamed.json)" = '{"app":3}' ] || fail "a renamed entry was not pruned against its old snapshot"
          [ ! -e $h/gone ] || fail "a target that was gone was created"
          [ "$(json $h/nosnap.json)" = '{"app":4,"managed":1}' ] || fail "a unit without a snapshot was pruned"
          warned "Not pruning removed $PWD/root/home/nosnap.json: the previous generation has no snapshot"
          [ "$(cat $h/bad.json)" = 'not json' ] || fail "a target config-graft refused was touched"
          warned "Could not prune removed $PWD/root/home/bad.json"
          [ "$(cat $h/removed.yaml)" = '{}' ] || fail "an all-managed YAML file was not pruned to {}"
          [ "$(cat $h/removed.toml)" = "$(printf '# app\napp = 2')" ] || fail "removed.toml was not pruned"
          for f in removed binary; do
            [ "$(head -c 7 $h/$f.plist)" = bplist0 ] || fail "$f.plist did not stay binary"
            plistutil -f xml -i $h/$f.plist -o $f.xml
            ! grep -q '<key>managed</key>' $f.xml && grep -q '<key>app</key>' $f.xml || fail "$f.plist was not pruned"
          done
          [ -L $h/followed.json ] || fail "a followed link was replaced"
          [ "$(json root/dotfiles/followed.json)" = '{"app":5}' ] || fail "a followed link was not pruned through"
          [ "$(json root/dotfiles/linked.json)" = '{"app":6,"managed":1}' ] || fail "a link was written through without followSymlinks"
          warned "Not pruning removed $PWD/root/home/linked.json: it is a symlink"
          [ "$(readlink $h/store.json)" = "$storeJson" ] || fail "a store link was replaced"
          warned "Not pruning removed $PWD/root/home/store.json: it resolves into"
          [ "$(json $h/claimed.json)" = '{"app":7,"managed":1}' ] || fail "a target home.file claims was pruned"
          warned "Not pruning removed $PWD/root/home/claimed.json: it is now linked by home.file"
          [ "$(json $h/running.json)" = '{"app":8,"managed":1}' ] || fail "a file was pruned while its app ran"
          warned "Skipping $PWD/root/home/running.json: MeetingBar is running"

          plistutil -f xml -i imported-com.example.present.plist -o present.xml 2>/dev/null || fail "the present domain was not imported"
          ! grep -q '<key>managed</key>' present.xml && grep -q '<key>appOwned</key>' present.xml || fail "the present domain was not pruned to its own keys"
          ! grep -q "export com.example.absent" defaults.log || fail "a domain that does not exist was exported"
          [ ! -e imported-com.example.absent.plist ] || fail "a domain that does not exist was created"
          [ ! -e imported-com.example.exportfails.plist ] || fail "a domain whose export failed was imported"
          warned "Skipping plist domain com.example.exportfails: defaults export failed"
          [ ! -e imported-com.example.running.plist ] || fail "a domain was pruned while its app ran"
          warned "Skipping com.example.running: MeetingBar is running"
          [ ! -e imported-com.example.restarting.plist ] || fail "a domain was pruned while its restart app ran"
          warned "Skipping com.example.restarting: com.example.Running is running (whileRunning.restart)"
          [ -e imported-com.example.uninstalled.plist ] || fail "a restart app that is not installed blocked the prune"
          [ ! -e unexpected.log ] || fail "the prune asked an app to do more than say whether it runs: $(cat unexpected.log)"
          [ ! -e imported-com.example.corrupt.plist ] || fail "a domain whose graft failed was imported"
          warned "Could not prune removed com.example.corrupt"
          [ "$(json $h/old-target.json)" = '{"app":9}' ] || fail "a retargeted entry's old target was not pruned"
          warned "Not pruning removed relative.json: this config-graft cannot read"
          warned "Not pruning removed com.example.badid: this config-graft cannot read"
          ! grep -q "not an id" osascript.log || fail "an invalid restart id reached osascript"
          [ "$(cat $h/notadir)" = 'a file, not a tree' ] || fail "a directory unit pruned a file"
          ! grep -q " $PWD/root/home/notadir " argv.log || fail "a directory unit ran on a file"

          [ ! -e $h/tree/managed.txt ] && [ ! -e $h/tree/sub ] || fail "the tree's managed files were not pruned"
          [ "$(cat $h/tree/app.txt)" = app ] || fail "the tree's app file was not kept"
          [ "$(stat -c %a $h/tree 2>/dev/null || stat -f %Lp $h/tree)" = 700 ] || fail "the tree root's mode was changed"
          [ -e root/dotfiles/tree/managed.txt ] || fail "a symlinked tree root was pruned through"
          warned "Not pruning removed $PWD/root/home/treelink: it is a symlink"
          warned "Not pruning removed $PWD/root/home/unknown.ini: this config-graft cannot read"

          grep -qx "json $PWD/root/home/followed.json [^ ]*/json $PWD/prev/snap/followed.json --follow-symlinks" argv.log \
            || fail "followSymlinks did not map to --follow-symlinks"
          grep -qx "plist $PWD/root/home/binary.plist [^ ]*/plist $PWD/prev/snap/binary.plist --plist-format binary" argv.log \
            || fail "binary did not map to --plist-format binary"
          grep -qx "plist $PWD/root/home/removed.plist [^ ]*/plist $PWD/prev/snap/removed.plist" argv.log \
            || fail "a plist without binary got flags"
          grep -qx "directory $PWD/root/home/tree [^ ]*/directory $PWD/prev/snap/tree --no-owner --xattrs none" argv.log \
            || fail "noOwner and xattrs did not map to their flags"
          ! grep -q -- --manage-root argv.log || fail "a prune managed a root"

          tar -cf first.tar root
          ./prune.sh > out 2>&1 || fail "the second run failed"
          tar -cf second.tar root
          cmp -s first.tar second.tar || fail "a second run changed something"
        done

        # A command the prune does not guard, failing, must not end a home-manager
        # activation: the domain round trip's scratch file cannot be made.
        setup
        sed 's|_live=$(mktemp)|_live=$(false)|' prune.sh > unguarded.sh
        chmod +x unguarded.sh
        STRICT=1 ./unguarded.sh > out 2>&1 || fail "an unguarded failure ended the prune"
        grep -q "reached the next entry" out || fail "an unguarded failure stopped the prune short"
        warned "Skipping plist domain com.example.present: defaults export failed"

        setup
        jq '.version = 2' prev/manifest.json > v2.json && mv v2.json prev/manifest.json
        STRICT=1 ./prune.sh > out 2>&1 || fail "an unknown version failed the run"
        grep -q "is not a version 1 manifest" out || fail "an unknown version was not reported"
        [ "$(json root/home/removed.json)" = '{"app":2,"edited":9,"managed":1}' ] || fail "an unknown version was pruned"

        setup
        NO_PREV=1 STRICT=1 ./prune.sh > out 2>&1 || fail "no previous generation failed the run"
        [ ! -e argv.log ] || fail "something was pruned without a previous generation"
        echo "prune of removed entries ok" > $out
      '';

  # The system module's own script with a previous generation the builder lays
  # out, `/run/current-system` and the test root rewritten to the builder's
  # directory. `shared.json` is declared under a new name, so the previous
  # generation's unit for it is pruned against its old snapshot, dropping `b`,
  # and the live entry, which runs after the prune, puts `a` back. A prune row
  # that fails warns without failing the activation; a live entry that fails still
  # does.
  systemPruneScenario =
    broken:
    systemActivation {
      managedJson = {
        "${pruneRoot}/shared.json".settings.a = 1;
      }
      // lib.optionalAttrs broken { "${pruneRoot}/broken.json".settings.a = 1; };
    };

  systemPreviousManifest = pkgs.writeText "system-previous-manifest.json" (
    common.mkManifest [
      (common.mkUnit {
        format.name = "json";
        entry = entry { };
        identity = "${pruneRoot}/shared.json";
        snapshot = "config-graft/managed-json/old-name";
      })
      (common.mkUnit {
        format.name = "json";
        entry = entry { };
        identity = "${pruneRoot}/bad.json";
        snapshot = "config-graft/managed-json/bad";
      })
      (common.mkUnit {
        format.name = "plist";
        entry = entry { cfprefsdDomain = "com.example.present"; };
        identity = "${pruneRoot}/com.example.present.plist";
        snapshot = "config-graft/managed-plist/domain";
        domain = "com.example.present";
      })
      (common.mkUnit {
        format.name = "plist";
        entry = entry {
          cfprefsdDomain = "com.example.restarting";
          whileRunning.restart = [ "com.example.Running" ];
        };
        identity = "${pruneRoot}/com.example.restarting.plist";
        snapshot = "config-graft/managed-plist/domain";
        domain = "com.example.restarting";
      })
    ]
  );

  systemPruneRun =
    pkgs.runCommand "config-graft-module-system-prune"
      {
        nativeBuildInputs = [ pkgs.jq ];
      }
      ''
        fail() { echo "$1"; cat out; exit 1; }
        for script in ${systemPruneScenario false}:0 ${systemPruneScenario true}:1; do
          rm -rf prev root
          mkdir -p prev/config-graft/managed-json prev/config-graft/managed-plist root
          sed -e "s|${pruneRoot}|$PWD/root|g" ${systemPreviousManifest} > prev/config-graft/manifest.json
          echo '{"a":1,"b":2}' > prev/config-graft/managed-json/old-name
          echo '{"a":1}' > prev/config-graft/managed-json/bad
          echo '<plist version="1.0"><dict/></plist>' > prev/config-graft/managed-plist/domain
          echo '{"a":1,"b":2,"app":true}' > root/shared.json
          echo 'not json' > root/bad.json
          echo 'not json' > root/broken.json
          # The domain's round trip cannot make its scratch file: a failure the
          # prune does not guard, which must not reach the ERR trap either.
          sed -e "s|/run/current-system|$PWD/prev|g" -e "s|${pruneRoot}|$PWD/root|g" \
            -e 's|/usr/bin/defaults|${pruneDefaults}|g' -e 's|_live=$(mktemp)|_live=$(false)|' \
            "''${script%:*}" > activate
          chmod +x activate
          export STUB_DIR=$PWD
          status=0
          ./activate > out 2>&1 || status=$?
          [ "$status" = "''${script#*:}" ] || fail "exited $status, expected ''${script#*:}"
          [ "$(jq -cS . root/shared.json)" = '{"a":1,"app":true}' ] \
            || fail "the shared target was not pruned, then re-applied: $(cat root/shared.json)"
          grep -q "warning: Could not prune removed $PWD/root/bad.json" out || fail "the failed prune row was not reported"
          grep -q "warning: Skipping plist domain com.example.present: defaults export failed" out \
            || fail "the domain's prune did not run"
          grep -q "warning: Skipping com.example.restarting: system.primaryUser is not set" out \
            || fail "a restart app was asked about without system.primaryUser"
          [ ! -e imported-com.example.restarting.plist ] || fail "a restart entry's domain was imported"
        done
        echo "system prune ok" > $out
      '';

  # home-manager's own prune step, as the module generates it, run under
  # home-manager's `set -euo pipefail`: on a first switch, with no `oldGenPath`,
  # it must do nothing, and with one it prunes what that generation's manifest
  # lists and this one does not declare.
  hmPruneScript = pkgs.writeShellScript "hm-prune" ''
    set -euo pipefail
    run() { "$@"; }
    _i() { :; }
    _iWarn() {
      _fmt="$1"
      shift
      printf "warning: $_fmt\n" "$@"
    }
    ${(hmConfig {
      homeDirectory = pruneRoot;
      managedJson."kept.json".settings.a = 1;
    }).home.activation.managedPrune.data
    }
    echo "reached the next entry"
  '';

  hmPreviousManifest = pkgs.writeText "hm-previous-manifest.json" (
    common.mkManifest [
      (common.mkUnit {
        format.name = "json";
        entry = entry { };
        identity = "${pruneRoot}/removed.json";
        snapshot = ".local/state/home-manager/managed-json/removed.json";
      })
    ]
  );

  hmPruneRun = pkgs.runCommand "config-graft-module-hm-prune" { nativeBuildInputs = [ pkgs.jq ]; } ''
    fail() { echo "$1"; cat out; exit 1; }
    sed "s|${pruneRoot}|$PWD/root|g" ${hmPruneScript} > prune.sh
    chmod +x prune.sh
    mkdir -p root gen/home-files/.local/state/home-manager/managed-json
    echo '{"managed":1,"app":2}' > root/removed.json

    (unset oldGenPath; ./prune.sh) > out 2>&1 || fail "a first switch failed the prune"
    grep -q "reached the next entry" out || fail "a first switch stopped the activation"
    [ "$(jq -cS . root/removed.json)" = '{"app":2,"managed":1}' ] || fail "a first switch pruned"

    sed "s|${pruneRoot}|$PWD/root|g" ${hmPreviousManifest} \
      > gen/home-files/.local/state/home-manager/managed-manifest.json
    echo '{"managed":1}' > gen/home-files/.local/state/home-manager/managed-json/removed.json
    oldGenPath=$PWD/gen ./prune.sh > out 2>&1 || fail "the prune failed"
    [ "$(jq -cS . root/removed.json)" = '{"app":2}' ] || fail "the removed entry was not pruned"
    echo "home-manager prune ok" > $out
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
      # Only an enabled link claims its path.
      name = "the prune skips what home.file and environment.etc link";
      expected = [
        true
        false
        true
        false
      ];
      actual =
        let
          hmScript =
            (hmConfig {
              file.".config/linked".source = "/linked";
              file.".config/off" = {
                source = "/off";
                enable = false;
              };
            }).home.activation.managedPrune.data;
          systemScript =
            (systemConfig {
              etc."app.conf" = {
                target = "app.conf";
                enable = true;
              };
              etc."off.conf" = {
                target = "off.conf";
                enable = false;
              };
            }).system.activationScripts.config-graft.text;
        in
        [
          (lib.hasInfix "\"/home/probe/.config/linked\"" hmScript)
          (lib.hasInfix "\"/home/probe/.config/off\"" hmScript)
          (lib.hasInfix "\"/etc/app.conf\"" systemScript)
          (lib.hasInfix "\"/etc/off.conf\"" systemScript)
        ];
    }
    {
      # What the prune compares against: a respelled target is the same file, and
      # a domain is the plist file behind it.
      name = "every platform keys its entries by normalized identity";
      expected = true;
      actual =
        let
          hmScript =
            (hmConfig {
              managedDirectory."./tree/../tree2".source = "/source";
            }).home.activation.managedPrune.data;
          systemScript =
            (systemConfig {
              managedJson."/srv/./app//config.json".settings.a = 1;
              managedDirectory."/srv/tree/".source = "/source";
              managedPlist.prefs = {
                cfprefsdDomain = "com.example.app";
                settings.a = 1;
              };
            }).system.activationScripts.config-graft.text;
        in
        lib.hasInfix ''"identity":"/home/probe/tree2"'' hmScript
        && lib.hasInfix ''"identity":"/srv/app/config.json"'' systemScript
        && lib.hasInfix ''"identity":"/srv/tree"'' systemScript
        && lib.hasInfix ''"identity":"/Library/Preferences/com.example.app.plist"'' systemScript;
    }
    {
      # The generation that removes the last entry must still prune after it.
      name = "a system configuration with no entries still links a manifest and runs the prune";
      expected = true;
      actual =
        let
          config = systemConfig { };
        in
        config.system.activationScripts ? config-graft
        && lib.hasInfix "config-graft/manifest.json" config.system.systemBuilderCommands
        && lib.hasInfix "/run/current-system/config-graft/manifest.json" config.system.activationScripts.config-graft.text;
    }
    {
      name = "a home-manager configuration with no entries still links a manifest and runs the prune";
      expected = true;
      actual =
        let
          config = hmConfig { };
        in
        config.home.file ? ".local/state/home-manager/managed-manifest.json"
        && lib.hasInfix "managed-manifest.json" config.home.activation.managedPrune.data;
    }
    {
      # A domain and the plist file behind it are one entry, and a respelled
      # target is the same file.
      name = "home-manager records normalized identities in the manifest";
      expected = [
        {
          kind = "json";
          identity = "/home/probe/app/config.json";
          domain = null;
        }
        {
          kind = "domain";
          identity = "/home/probe/Library/Preferences/com.example.app.plist";
          domain = "com.example.app";
        }
      ];
      actual =
        map (unit: { inherit (unit) kind identity domain; })
          (builtins.fromJSON
            (hmConfig {
              managedPlist.prefs = {
                cfprefsdDomain = "com.example.app";
                settings.a = 1;
              };
              managedJson.config = {
                target = "./app/../app//config.json";
                settings.a = 1;
              };
            }).home.file.".local/state/home-manager/managed-manifest.json".source.text
          ).units;
    }
    {
      name = "a system domain is recorded by its /Library/Preferences path";
      expected = [
        "/Library/Preferences/.GlobalPreferences.plist"
        "/Library/Preferences/.GlobalPreferences"
      ];
      actual = [
        (common.domainPlist "/Library/Preferences" "NSGlobalDomain")
        (common.systemDomain "NSGlobalDomain")
      ];
    }
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
      name = "home-manager runs the prune, then the entries, after linkGeneration";
      expected = {
        managedJson = {
          after = [
            "writeBoundary"
            "linkGeneration"
          ];
          before = [ ];
        };
        managedDirectory = {
          after = [
            "writeBoundary"
            "linkGeneration"
          ];
          before = [ ];
        };
        managedPrune = {
          after = [
            "writeBoundary"
            "linkGeneration"
          ];
          before = [
            "managedJson"
            "managedPlist"
            "managedToml"
            "managedYaml"
            "managedDirectory"
          ];
        };
      };
      actual = lib.mapAttrs (_: activation: { inherit (activation) after before; }) hmActivation;
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
  cat ${pruneRuns} >> $out
  cat ${systemPruneRun} >> $out
  cat ${hmPruneRun} >> $out
''
