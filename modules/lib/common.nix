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

  # The attribute *policy* flags of a directory entry, separate from `--manage-root`:
  # the policy says which attributes are reconciled at all, `--manage-root` says
  # whether the target's own attributes are among them. A reconcile wants both; a
  # caller with a DESIRED tree of its own making wants only the policy.
  directoryPolicyFlags =
    { lib, entry }:
    lib.optional entry.noOwner "--no-owner"
    ++ lib.optional (entry.xattrs != "all") "--xattrs ${entry.xattrs}";

  # The write flags of a byte-format file entry: how the result is encoded, and
  # whether a symlinked target is written through. A `cfprefsdDomain` entry takes
  # neither, since it writes a scratch file of its own.
  fileWriteFlags =
    {
      lib,
      format,
      entry,
    }:
    lib.optional (format.name == "plist" && entry.binary) "--plist-format binary"
    ++ lib.optional entry.followSymlinks "--follow-symlinks";

  # `whileRunning`, the same on every entry type.
  whileRunningOption =
    lib:
    lib.mkOption {
      default = { };
      description = ''
        What to do about an app that is running when this entry is reconciled. An
        app that rewrites its own configuration while it runs can lose a write it
        makes between config-graft's read and its write, and for a plist entry
        with `cfprefsdDomain` set that includes keys this entry does not manage,
        since {command}`defaults import` replaces the whole domain.

        This narrows the window rather than closing it: an app that starts after
        the check, or writes while the reconcile runs, can still race it.

        A skipped entry still advances BASE. When the activation warns and
        carries on -- a {option}`skip` match (or a failed {command}`pgrep`), a
        blocked {option}`restart`, or a failed or empty {command}`defaults
        export` -- the generation still records the entry's settings as the ones
        last applied. A key removed from the settings in that generation is then
        never pruned: it stays in the app's configuration until it is removed by
        hand. A failed graft or {command}`defaults import` fails the activation,
        which under home-manager and nix-darwin ends it before BASE moves, so the
        next activation retries against the old one -- on nix-darwin only until
        a reboot, since {command}`darwin-rebuild` has already pointed the system
        profile at the failed generation and boot re-links
        {file}`/run/current-system` to it. NixOS never ends early: it records a
        failed snippet and carries on, so there every skip and failure advances
        BASE. This is deliberate: BASE comes from the previous generation rather
        than from mutable state.
      '';
      type = lib.types.submodule {
        options.skip = lib.mkOption {
          type = lib.types.listOf lib.types.nonEmptyStr;
          default = [ ];
          example = [ "MeetingBar" ];
          description = ''
            Process names that make activation skip this entry, with a warning,
            while any of them is running, leaving it untouched.

            Each name is matched exactly, as {command}`pgrep -x` matches it,
            against processes of every user. On macOS that is the last component
            of the process's `argv[0]`, normally the executable's file name
            (`MeetingBar` for {file}`MeetingBar.app/Contents/MacOS/MeetingBar`),
            in full. On Linux it is the kernel's command name, which keeps only
            the first 15 bytes, so the name is cut the same way before matching;
            a program wrapped by `wrapProgram` runs as `.<name>-wrapped`. Find
            the name to use with {command}`pgrep -l <part of it>` (or
            {command}`ps -o comm=` on Linux). If {command}`pgrep` itself fails,
            the entry is skipped too; a non-ASCII name whose 15-byte cut splits a
            character may do that on Linux.
          '';
        };

        options.restart = lib.mkOption {
          type = lib.types.listOf (lib.types.strMatching "[A-Za-z0-9.-]+");
          default = [ ];
          example = [ "com.leits.MeetingBar" ];
          description = ''
            Bundle ids of apps to quit before this entry is reconciled and to
            relaunch afterwards (macOS only; a build-time assertion rejects it
            on any other host). Checked after {option}`skip`, which still skips
            without quitting anything.

            Each listed app that is running is asked to quit with an Apple Event
            ({command}`osascript`, `tell application id ... to quit`) and given
            {option}`quitTimeout` seconds to exit; it is never force-killed. If
            one does not exit in time, or the quit fails, the entry is skipped
            with a warning, as {option}`skip` would, and every app the quit was
            sent to and that has exited is relaunched. One still running then
            (behind a save dialog, say) is left alone, and is not relaunched if it
            quits later. Otherwise the entry is reconciled and the apps that were
            running are relaunched in the background ({command}`open -g -b`), even
            when the reconcile fails.

            The first quit asks for the Automation permission: macOS shows a
            prompt naming the app that runs the activation (your terminal, for
            example) and the app being quit. Until it is allowed under System
            Settings > Privacy & Security > Automation, the quit fails with error
            -1743 and the entry is skipped. A listed app that is not installed
            counts as not running; any other failure of the running check skips
            the entry. In a system (nix-darwin) entry the app is quit and
            relaunched as {option}`system.primaryUser`, which must be set; a copy
            another logged-in user runs is not seen. A home-manager dry run only
            reports what it would quit.
          '';
        };

        options.quitTimeout = lib.mkOption {
          type = lib.types.ints.positive;
          default = 10;
          description = ''
            Seconds to wait for each app in {option}`restart` to exit after
            being asked to quit, before giving up and skipping the entry.
          '';
        };
      };
    };

  # Wraps an entry's reconcile `body` in quitting the running apps in `ids` and
  # relaunching them afterwards. Running is asked of AppleScript by bundle id,
  # which never launches the app; osascript exits 0 whether it prints `true` or
  # `false`, so the output is what is compared. The quit is sent only after a
  # `true`, since telling an app that is not running to quit can launch it, and
  # under `with timeout`, since the reply waits on the app (a save sheet holds it
  # for AppleScript's default two minutes); one deadline covers the reply and the
  # poll, and a reply timeout (-1712) leaves the poll to decide. Every app the
  # quit reached, even with an error (-609 when it exits before replying), is
  # relaunched afterwards unless it is still running: `open` on a running app
  # sends it a reopen event, which can open a window. A home-manager dry run
  # (`DRY_RUN`) only reports what it would quit.
  #
  # The body runs in a subshell with errexit on, so it stops at its first failure
  # and its status can be taken: home-manager's own errexit is lifted around the
  # subshell, or the failure would end the activation before the relaunch. A
  # failed reconcile still relaunches, then fails with `false`, which stops
  # home-manager and trips the system script's ERR trap.
  guardRestart =
    {
      lib,
      ids,
      quitTimeout,
      label,
      user,
      commands,
      body,
    }:
    let
      esc = lib.escapeShellArg;
      osascript = commands.osascript or "/usr/bin/osascript";
      open = commands.open or "/usr/bin/open";
      sleep = commands.sleep or "/bin/sleep";
      asUser =
        if user == null then
          ""
        else
          "/bin/launchctl asuser \"$(/usr/bin/id -u -- ${esc user})\" /usr/bin/sudo --user=${esc user} -- ";
      timeout = toString quitTimeout;
      seconds = if quitTimeout == 1 then "1 second" else "${timeout} seconds";
      # Only stdout is compared, so a warning on stderr cannot turn `true` into
      # something else. The first check keeps stderr: an app that is not
      # installed fails it with -1728 ("Can't get application id"), which counts
      # as not running; any other failure is shown and blocks the entry. The
      # error number is matched, not the text, which macOS localizes.
      isRunning = id: "${asUser}${osascript} -e ${esc ''application id "${id}" is running''}";
      quitted = i: "_cgQuit${toString i}";
      # Only home-manager has a dry run. The system script's `run` shim would
      # still reconcile, so honouring `DRY_RUN` there would graft with the app up.
      dryRun = if user == null then "[[ -v DRY_RUN ]]" else "false";
      quit = i: id: ''
        ${quitted i}=""
        if [[ -z "$_restartBlocked" ]]; then
          if ! _state=$(${isRunning id} 2>"$_restartErr"); then
            if [[ "$(<"$_restartErr")" == *"(-1728)"* ]]; then _state=false; else _state=""; fi
          fi
          if [[ $_state != true && $_state != false ]]; then
            cat "$_restartErr" >&2
            _restartBlocked="could not tell whether ${id} is running"
          elif [[ $_state == false ]]; then
            :
          elif ${dryRun}; then
            _i "Would quit %s and relaunch it after the reconcile" ${esc id}
          else
            _deadline=$((SECONDS + ${timeout}))
            if ! _state=$(${asUser}${osascript} -e ${esc "with timeout of ${timeout} seconds"} -e ${esc ''tell application id "${id}" to quit''} -e 'end timeout' 2>&1 >/dev/null) \
              && [[ $_state != *-1712* ]]; then
              if [[ $_state == *-1743* || $_state == *-1744* ]]; then
                _restartBlocked="not allowed to ask ${id} to quit; allow it under Privacy & Security > Automation"
              else
                ${quitted i}=1
                _restartBlocked="asking ${id} to quit failed: $_state"
              fi
            else
              ${quitted i}=1
              _tries=0
              until [[ "$(${isRunning id} 2>/dev/null)" == false ]]; do
                if (( ++_tries > ${toString (quitTimeout * 2)} || SECONDS > _deadline )); then
                  _restartBlocked="${id} did not quit within ${seconds}"
                  break
                fi
                ${sleep} 0.5
              done
            fi
          fi
        fi
      '';
      relaunch = i: id: ''
        if [[ -n "''$${quitted i}" && "$(${isRunning id} 2>/dev/null)" != true ]]; then
          ${asUser}${open} -g -b ${esc id} \
            || _iWarn "Could not relaunch %s after %s (whileRunning.restart)" ${esc id} ${esc label}
        fi
      '';
    in
    if ids == [ ] then
      body
    else
      ''
        _restartBlocked=""
        _graftStatus=0
        _restartErr=$(mktemp)
        ${lib.concatStrings (lib.imap0 quit ids)}
        rm -f "$_restartErr"
        if [[ -n "$_restartBlocked" ]]; then
          _iWarn "Skipping %s: %s (whileRunning.restart)" ${esc label} "$_restartBlocked"
        else
          _errexit=""
          if [[ $- == *e* ]]; then _errexit=1; set +e; fi
          (
            set -e
        ${body}
          )
          _graftStatus=$?
          if [[ -n "$_errexit" ]]; then set -e; fi
        fi
        ${lib.concatStrings (lib.imap0 relaunch ids)}
        if (( _graftStatus != 0 )); then false; fi
      '';

  # Wraps an entry's reconcile `body` so it runs only while none of the processes
  # `names` expands to is running. `names` is a list of shell words, so the names
  # can be literals (`lib.escapeShellArg`) or come from a variable at run time, and
  # `label` is one shell word; an empty `names` adds no guard. pgrep exits 1 when
  # nothing matches; a match (0) or a pgrep failure (2 or 3) skips the entry.
  # macOS's pgrep matches the untruncated basename of argv[0], while Linux's
  # matches the 15-byte kernel comm, hence the cut there. pgrep's stderr is dropped
  # because procps warns on every miss for an escaped pattern over 15 bytes; a real
  # failure still surfaces through its status.
  #
  # The pattern is built at run time the way `lib.escapeRegex` and
  # `builtins.substring` would build it: byte by byte, which is why the subshell
  # runs in the C locale, and ended by an `x` that is stripped after it, so a
  # trailing newline survives the command substitution.
  guardRunning =
    {
      lib,
      pkgs,
      names,
      label,
      commands,
      body,
    }:
    let
      isDarwin = pkgs.stdenv.hostPlatform.isDarwin;
      pgrep = commands.pgrep or (if isDarwin then "/usr/bin/pgrep" else lib.getExe' pkgs.procps "pgrep");
    in
    if names == [ ] then
      body
    else
      ''
        _running=""
        for _name in ${lib.concatStringsSep " " names}; do
          _pattern=$(
            LC_ALL=C
            ${lib.optionalString (!isDarwin) "_name=\${_name:0:15}"}
            for (( _k = 0; _k < ''${#_name}; _k++ )); do
              _char=''${_name:_k:1}
              case $_char in
                '\' | '[' | '{' | '(' | ')' | '^' | '$' | '?' | '*' | '+' | '|' | '.') printf '\\%s' "$_char" ;;
                *) printf '%s' "$_char" ;;
              esac
            done
            printf x
          )
          _pattern=''${_pattern%x}
          _pgrepStatus=0
          ${pgrep} -x -- "$_pattern" >/dev/null 2>&1 || _pgrepStatus=$?
          if [[ $_pgrepStatus -ne 1 ]]; then
            _running=$_name
            break
          fi
        done
        if [[ -z "$_running" ]]; then
        ${body}
        elif [[ $_pgrepStatus -eq 0 ]]; then
          _iWarn "Skipping %s: %s is running (whileRunning.skip)" ${label} "$_running"
        else
          _iWarn "Skipping %s: pgrep exited %s looking for %s (whileRunning.skip)" ${label} "$_pgrepStatus" "$_running"
        fi
      '';
in
{
  inherit safeName;

  # Whether a byte-format entry does anything: one with neither `settings` nor
  # `source` is inert.
  isActive = entry: entry.settings != { } || entry.source != null;

  # What the system modules hand `defaults` for a `cfprefsdDomain`. As root,
  # `defaults <id>` addresses root's own user domain, which only root's processes
  # read (/private/var/root/Library/Preferences); the system-wide (any-user) domain
  # is the file under /Library/Preferences, which `defaults` takes as a path.
  # Anything already holding a `/` is taken to be a path and passed through.
  systemDomain =
    domain:
    if builtins.match ".*/.*" domain != null then
      domain
    else if
      builtins.elem domain [
        "NSGlobalDomain"
        "-g"
        "-globalDomain"
      ]
    then
      "/Library/Preferences/.GlobalPreferences"
    else
      "/Library/Preferences/${domain}";

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
  # on every platform except `target` (relative vs absolute), the `cfprefsdDomain`
  # description (per-user vs system) and `package`'s default, which the caller
  # supplies; `parent` (`home` or `environment`) names that default in the docs.
  entryType =
    {
      lib,
      pkgs,
      parent,
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
              defaultText = literalExpression "config.${parent}.managed.package";
              description = ''
                The config-graft package used to reconcile this entry. Defaults to the
                module-level {option}`${parent}.managed.package` (itself defaulting to
                this flake's own build), which the activation script calls by store
                path, so no overlay or `PATH` entry is needed.
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

            whileRunning = whileRunningOption lib;

            followSymlinks = mkOption {
              type = types.bool;
              default = false;
              description = ''
                When {option}`target` is a symlink, write the file it resolves to
                (`--follow-symlinks`). Without it, an activation that would change a
                symlinked {option}`target` fails rather than replacing the link with
                a regular file; one with nothing to change still succeeds. Set it for
                a target linked to a file kept elsewhere, such as a dotfiles checkout.
                A link into {file}`/nix/store` cannot be written through even with
                this set; a former `home.file` or `environment.etc` link is removed
                by activation before this entry runs.
                Ignored when the entry reconciles through `cfprefsdDomain`.
              '';
            };
          }
          // lib.optionalAttrs (format.name == "plist") {
            # `defaults`/`cfprefsd` are macOS-only; `mkAssertions` guards a Darwin
            # host when this is set.
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
                with `--plist-format binary`, so values XML cannot represent
                (characters XML 1.0 forbids, e.g. the ESC 0x1B separators in
                `NSUserKeyEquivalents`) round-trip. When generated from
                {option}`settings` this runs the entry's {option}`format` generator
                and converts its output with `pkgs.libplist` at build time, so a
                {option}`format` override still applies; with {option}`source` it
                only forces the binary write.

                A file entry rarely needs it: at `false` the DESIRED is XML but the
                *write* follows whatever encoding the target already has, so a
                binary target (which is what macOS stores) stays binary and keeps
                its own dates and bytes intact. Set it when {option}`settings`
                themselves hold a character XML cannot carry, which an XML DESIRED
                could not express -- a build-time assertion says so.

                Defaults to `true` for a {option}`cfprefsdDomain` entry, whose whole
                round-trip is binary anyway ({command}`defaults export` produces a
                binary plist and {command}`defaults import` reads one back), so
                nothing a domain holds is squeezed through XML. Setting it to
                `false` builds that entry's DESIRED as XML again, which is fine for
                ordinary values but cannot carry a character XML 1.0 forbids -- the
                write to cfprefsd stays binary either way, so `false` narrows what
                the DESIRED can express without widening anything.
              '';
            };
          };

          config.target = lib.mkDefault name;
        }
      )
    );

  # The per-entry reconcile script, shared by every platform. It uses `run` (run a
  # command), `_i` (info log) and `_iWarn` (warning): home-manager provides these
  # in its activation context, and the system module defines pass-through shims.
  # The caller sets `_prev` (the BASE snapshot path) beforehand and passes the
  # resolved `target`. `commands` replaces the system tools the script calls by
  # absolute path, for tests that run it against stubs; the modules never pass it.
  # `restartUser` is who `whileRunning.restart` quits and relaunches apps as: null
  # (home-manager) runs them as the activating user, a name (the system modules)
  # goes through `launchctl asuser` and `sudo --user`.
  #
  # Shell vars are `_`-prefixed because home-manager runs this inline in its
  # activation shell, shared with every other module's activation code, so bare
  # names like `prev`/`target`/`domain` could clobber or be clobbered by another
  # module's variables. (The system side runs its own script, where it's harmless.)
  mkEntryReconcileScript =
    {
      lib,
      pkgs,
      format,
      entry,
      desired,
      target,
      commands ? { },
      restartUser ? null,
    }:
    let
      defaults = commands.defaults or "/usr/bin/defaults";
      isDomain = format.name == "plist" && entry.cfprefsdDomain != null;
      label = if isDomain then entry.cfprefsdDomain else target;

      domainScript = ''
        _domain=${lib.escapeShellArg entry.cfprefsdDomain}
        _i "Reconciling managed plist domain %s" "$_domain"

        # Read the live domain through cfprefsd (not the on-disk file, which may be
        # staler than cfprefsd's cache). A domain that does not exist yet exports
        # as an empty dictionary with status 0, so a failed or empty export is never
        # a first run: grafting onto nothing and importing the result would replace
        # every key the domain holds with the managed ones alone.
        _live=$(mktemp)
        if ! ${defaults} export "$_domain" "$_live"; then
          _iWarn "Skipping plist domain %s: defaults export failed, so it was left untouched" "$_domain"
        elif [[ ! -s "$_live" ]]; then
          _iWarn "Skipping plist domain %s: defaults export wrote nothing, so it was left untouched" "$_domain"
        # Graft our settings into the live state, then push it back through cfprefsd
        # so it adopts the merged result. `--plist-format binary` is unconditional
        # here, not `binary`-gated: XML would drop XML-illegal bytes and sub-second
        # dates that both `defaults` ends carry, and the flag keeps the write from
        # depending on what the export produced. A failed graft must not reach the
        # import, which would rewrite the domain from a stale snapshot. `false` is
        # what reports it: fatal under home-manager, counted by the system script's
        # ERR trap (the failed `run` sits in a condition, which doesn't).
        elif ! run ${lib.getExe entry.package} plist "$_live" ${desired} "$_prev" --plist-format binary; then
          _iWarn "Skipping plist domain %s: config-graft failed, so it was left untouched" "$_domain"
          rm -f "$_live"
          false
        else
          run ${defaults} import "$_domain" "$_live"
        fi
        rm -f "$_live"
      '';

      fileScript = ''
        _target=${lib.escapeShellArg target}
        _i "Reconciling managed ${format.name} file %s" "$_target"
        run ${lib.getExe entry.package} ${format.name} "$_target" ${desired} "$_prev"${
          lib.concatMapStrings (flag: " ${flag}") (fileWriteFlags {
            inherit lib format entry;
          })
        }
      '';
    in
    guardRunning {
      inherit lib pkgs commands;
      names = map lib.escapeShellArg entry.whileRunning.skip;
      label = lib.escapeShellArg label;
      body = guardRestart {
        inherit lib label commands;
        inherit (entry.whileRunning) quitTimeout;
        ids = entry.whileRunning.restart;
        user = restartUser;
        body = if isDomain then domainScript else fileScript;
      };
    };

  # Directory-format entry type. Unlike the byte formats there is no `settings`
  # (freeform data through a `pkgs.formats` generator): the DESIRED is a prebuilt
  # directory tree given as `source`, so the `directory` subcommand gets its own entry
  # type with the directory-specific reconcile flags. Every declared entry is
  # active (`source` is required), so there is no `settings != {}` liveness test.
  directoryEntryType =
    {
      lib,
      pkgs,
      parent,
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
              defaultText = literalExpression "config.${parent}.managed.package";
              description = ''
                The config-graft package used to reconcile this entry. Defaults to the
                module-level {option}`${parent}.managed.package` (itself defaulting to
                this flake's own build), called by store path, so no overlay or `PATH`
                entry is needed.
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

            whileRunning = whileRunningOption lib;
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
      pkgs,
      entry,
      target,
      commands ? { },
      restartUser ? null,
    }:
    let
      flags = lib.concatStringsSep " " (
        lib.optional entry.manageRoot "--manage-root" ++ directoryPolicyFlags { inherit lib entry; }
      );
    in
    guardRunning {
      inherit lib pkgs commands;
      names = map lib.escapeShellArg entry.whileRunning.skip;
      label = lib.escapeShellArg target;
      body = guardRestart {
        inherit lib commands;
        inherit (entry.whileRunning) quitTimeout;
        ids = entry.whileRunning.restart;
        label = target;
        user = restartUser;
        body = ''
          _target=${lib.escapeShellArg target}
          _i "Reconciling managed directory tree %s" "$_target"
          run ${lib.getExe entry.package} directory ${flags} "$_target" ${entry.source} "$_prev"
        '';
      };
    };

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
        hasXmlIllegalChar =
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
              but cfprefsd and defaults exist only on macOS (this configuration
              targets ${pkgs.stdenv.hostPlatform.system}). Unset it to edit the
              plist file in place instead.
            '';
          }
          {
            # An XML DESIRED cannot carry such a character, and the reconcile refuses
            # that write rather than emitting a file only macOS can read. Catch it at
            # build time instead of at activation.
            assertion = entry.binary || entry.source != null || !(hasXmlIllegalChar entry.settings);
            message = ''
              ${parent}.${format.optionName}."${name}" has `settings` holding a
              character an XML plist cannot carry (a C0 control other than tab,
              newline or carriage return -- e.g. the ESC 0x1B separators in
              `NSUserKeyEquivalents` -- or U+FFFE or U+FFFF), but `binary` is false,
              so its DESIRED would be generated as XML and the reconcile would
              refuse the write. Set `binary = true` on this entry.
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

  # Build-time guards for `whileRunning.restart` on any entry type: it drives
  # osascript and open, which exist only on macOS, and a system entry acts as
  # `restartUser` (`system.primaryUser`), which must be set (`needsUser`).
  mkRestartAssertions =
    {
      lib,
      pkgs,
      parent,
      optionName,
      entries,
      needsUser ? false,
      restartUser ? null,
    }:
    lib.concatLists (
      lib.mapAttrsToList (
        name: entry:
        lib.optionals (entry.whileRunning.restart != [ ]) [
          {
            assertion = pkgs.stdenv.hostPlatform.isDarwin;
            message = ''
              ${parent}.${optionName}."${name}".whileRunning.restart is set, but
              quitting and relaunching an app uses osascript and open, which
              exist only on macOS (this configuration targets
              ${pkgs.stdenv.hostPlatform.system}). Use whileRunning.skip instead.
            '';
          }
          {
            assertion = !needsUser || !pkgs.stdenv.hostPlatform.isDarwin || restartUser != null;
            message = ''
              ${parent}.${optionName}."${name}".whileRunning.restart is set, but
              system.primaryUser is not: a system entry quits and relaunches the
              app as that user. Set system.primaryUser.
            '';
          }
        ]
      ) entries
    );
}
