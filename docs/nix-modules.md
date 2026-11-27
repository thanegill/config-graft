# Nix modules

The flake ships modules for home-manager, NixOS and nix-darwin. Each format gets a `managed<Format>` option. Every entry reconciles its `settings` (or `source`) into a live file on each activation: keys the app wrote are kept, and keys you drop are pruned, with the previous generation's snapshot as BASE.

For a complete, working `flake.nix` per platform, start with [`examples/`](../examples).

## What the flake exposes

- `homeManagerModules.default`: `home.managed{Json,Plist,Yaml,Toml}` and `home.managedDirectory`. Targets are relative to `$HOME`. The entries run after `linkGeneration`, so an entry moved from `home.file` finds its old store link already removed.
- `nixosModules.default` and `darwinModules.default`: `environment.managed*` with absolute targets, reconciled during system activation, after `/etc` is set up.
- `overlays.default` (optional): adds the `config-graft` CLI to `pkgs`. The modules don't need it, since they run the flake's own build by store path.

## Every entry

- The attribute name is the target path; set `target` to use a different one.
- `package` is the config-graft build for this entry. The default for every entry is `home.managed.package` (or `environment.managed.package`), which defaults to this flake's own build.
- `whileRunning` handles an app that rewrites its own configuration while it runs, whose write between config-graft's read and its write would otherwise be lost. See below.

### `whileRunning`

`whileRunning.skip` is a list of process names. While any of them is running, activation skips the entry with a warning and leaves it untouched until the next activation.

- Names are matched exactly, as `pgrep -x` matches them, against every user's processes: on macOS the basename of `argv[0]` in full (normally the executable's file name), on Linux the kernel's 15-byte command name, so a longer name is cut to match.
- A `pgrep` that fails also skips the entry.
- The check narrows the race rather than closing it: an app that starts after the check, or writes while the reconcile runs, can still lose a write.

On macOS, `whileRunning.restart` takes bundle ids and quits the app instead of skipping:

- Each listed app that is running is asked to quit with an Apple Event and given `whileRunning.quitTimeout` seconds (default 10) to exit. The entry is reconciled, and the apps that were running are relaunched in the background, even when the reconcile fails.
- An app is never force-killed. If one doesn't exit in time, the quit fails, or the running check fails, the entry is skipped with a warning, and the apps it asked to quit that have exited are relaunched. One still running (behind a save dialog, say) is left alone, and isn't relaunched if it quits later. An app that isn't installed counts as not running.
- `skip` is checked first and skips without quitting anything. A home-manager dry run only reports what it would quit.
- The first quit asks for the Automation permission: macOS prompts, naming the app that runs the activation (your terminal, say) and the app being quit. Until it is allowed under System Settings > Privacy & Security > Automation, the entry is skipped.
- A nix-darwin entry quits and relaunches the app as `system.primaryUser`, which must be set. Setting `restart` on a non-macOS host fails the build.

### When an entry fails, or is skipped

An entry that fails to reconcile fails the activation. Under home-manager the activation stops there. Under NixOS and nix-darwin every other entry is still reconciled first, and the activation then reports the failure; nix-darwin stops before it swaps `/run/current-system`, so the next switch prunes against the generation before the failed one.

A skipped entry still advances BASE. When the activation warns about an entry and carries on, the new generation still records that entry's settings as the ones last applied. So a key you removed from `settings` in that generation is never pruned; it stays in the app's configuration until you remove it by hand. That happens for:

- a `skip` match or a failed `pgrep`, and a blocked `restart`;
- a failed or empty `defaults export`.

A failed graft or `defaults import` fails the activation (above), which under home-manager and nix-darwin ends it before BASE moves, so the next activation retries against the old one. On nix-darwin that holds only until a reboot: `darwin-rebuild` has already pointed the system profile at the failed generation, and boot re-links `/run/current-system` to it. It cuts both ways: entries that did succeed in a failed run are next reconciled against the older BASE too. NixOS never ends early: it records a failed snippet and carries on, so there every skip and failure advances BASE. This is deliberate: BASE comes from the previous generation, not from mutable state.

## File entries

Each JSON, plist, YAML or TOML entry takes:

- `settings`: freeform data to reconcile into the target.
- `source`: a pre-built file instead of `settings`, from any generator, template or derivation. The two are mutually exclusive, and an entry with neither is inert.
- `format`: a `pkgs.formats`-style generator used to build `settings`, for a validating or specially configured type.
- `followSymlinks`: when the target is a symlink, write the file it resolves to (`--follow-symlinks`), for a target linked to a file kept elsewhere, such as a dotfiles checkout. Without it, an activation that would change a symlinked target fails rather than replacing the link; one with nothing to change still succeeds. A link into `/nix/store` can't be written through even with it set. It is ignored for a `cfprefsdDomain` entry.

```nix
home.managedJson.".config/app/config.json".settings = {
  theme = "dark";
  editor.fontSize = 14;
};
```

## plist entries

plist entries take two more options.

### `cfprefsdDomain`

Set it to a preference domain (`com.example.app`) to reconcile through `cfprefsd` with `defaults` instead of editing the file, so the change isn't lost to cfprefsd's in-memory cache. `target` is ignored in this mode.

- macOS only, asserted at build time.
- Per-user domains under home-manager; system-wide domains under nix-darwin. nix-darwin activates as root, so there a domain id is addressed by path as the any-user domain, `/Library/Preferences/<id>` (`NSGlobalDomain` as `/Library/Preferences/.GlobalPreferences`); anything holding a `/` is used as a path, as given. A user's own value for a key still overrides it for that user.
- The scratch file always goes through as a binary plist, because `defaults export` hands over binary and `defaults import` reads it back, so nothing a domain holds is squeezed through XML.
- The import replaces the whole domain, so when `defaults export` fails or writes nothing, the entry is skipped with a warning and the domain left untouched; the rest of the activation carries on. A domain that doesn't exist yet exports as an empty dictionary and is created.
- A graft that fails is never imported, and the failure fails the activation ([see above](#when-an-entry-fails-or-is-skipped)).
- A running app keeps its own copy of its preferences, so quit it before switching and relaunch it afterwards: `whileRunning.restart` does that for you, or `whileRunning.skip` leaves the domain alone while the app runs. Since the import replaces the whole domain, a write the app makes during the run is lost even under keys the entry doesn't manage.

### `binary`

`binary = true` builds a binary DESIRED (running the entry's `format` generator, then converting with libplist at build time) and forces the write with `--plist-format binary`. With `source`, it only forces the binary write.

- It defaults to `true` for a `cfprefsdDomain` entry and `false` otherwise.
- A file entry rarely needs it. At `false` the write follows the target's own encoding, so a binary Preferences file stays binary and keeps its dates and bytes.
- Set it when `settings` hold a character XML 1.0 forbids, such as the ESC (`0x1B`) separators in `NSUserKeyEquivalents`. If such a character appears while `binary` is false, a build-time assertion says so, rather than the reconcile refusing at activation.

See [formats.md](formats.md#plist) for what XML can and can't carry.

## Directory entries

`managedDirectory` wraps the `directory` subcommand. Each entry reconciles a `source` directory tree into its target, keeping files the app created and pruning files dropped from `source`. In place of `settings` it takes:

- `manageRoot`: also reconcile the target directory's own mode, owner and xattrs.
- `noOwner`: leave uid/gid alone. Set it on a non-root home-manager activation, since a store-built `source` is owned by root.
- `xattrs`: `all` (default), `safe` or `none`.

```nix
home.managedDirectory.".config/app/plugins" = {
  source = ./plugins;
  noOwner = true;
};
```

See [directory.md](directory.md) for how a tree is reconciled.

## A home-manager flake

```nix
{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    home-manager = {
      url = "github:nix-community/home-manager";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    config-graft = {
      url = "github:thanegill/config-graft";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      nixpkgs,
      home-manager,
      config-graft,
      ...
    }:
    {
      homeConfigurations."me" = home-manager.lib.homeManagerConfiguration {
        pkgs = import nixpkgs { system = "x86_64-linux"; };
        modules = [
          config-graft.homeManagerModules.default

          # your home identity
          {
            home.username = "me";
            home.homeDirectory = "/home/me";
            home.stateVersion = "24.05";
          }

          # Graft a few keys into a JSON file the app rewrites; the attribute name
          # is the target path, relative to $HOME.
          {
            home.managedJson.".config/app/config.json".settings = {
              theme = "dark";
              editor.fontSize = 14;
            };
          }

          # comments in the live file are preserved
          {
            home.managedYaml.".config/tool/config.yaml".settings.plugins = [ "git" ];
          }
        ];
      };
    };
}
```
