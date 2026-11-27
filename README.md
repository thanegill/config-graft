# config-graft

> **AI Disclosure:** This project falls under [Level 5: Bots coded, human understands completely](https://www.visidata.org/blog/2026/ai/). Exact model used varies by commit and is in the commit message. This is disclosed as useful context for anyone depending on, auditing, or forking this code, not because I think it lowers the bar on the quality. Reach out or report bugs the way you would for any other project.

Three-way reconcile/merge for **JSON, plist, YAML, and TOML files** or a **directory tree**.

It deep-merges a *managed subset* (DESIRED) into a file the application also writes to (TARGET), while:

- **preserving** keys the app/user wrote that you don't manage,
- **pruning** keys you used to manage but dropped — but only if the user hasn't changed them, using a **BASE** snapshot (the previously-applied config) as the merge ancestor.

## Usage

```sh
config-graft <FORMAT> <TARGET> <DESIRED> [BASE]   # FORMAT: json | yaml | toml | plist | directory

config-graft json config.json desired.json .state/last-applied.json
config-graft json --check config.json desired.json base.json    # exit 3 if it would change
config-graft json --stdout --diff config.json desired.json       # preview without writing
config-graft json --array-strategy replace config.json desired.json  # own the list wholesale
config-graft json --merge-key name config.json desired.json          # match list-of-objects by `name`

config-graft plist app.plist desired.plist base.plist        # same merge, plist files
config-graft plist config desired                            # plist on any name (the subcommand names the format)
config-graft plist --plist-format binary app.plist desired.plist  # force a binary plist

config-graft yaml config.yaml desired.yaml                  # YAML, keeping comments
config-graft toml config.toml desired.toml                  # TOML, keeping comments

config-graft directory dest/ desired/                       # reconcile a directory tree
```

The format is a required **subcommand** (`json`, `yaml`, `toml`, `plist`, or `directory`); each subcommand exposes only the flags that apply to it, so an unsupported flag/format pairing is a usage error rather than a runtime one.

Arrays merge three-way by default; `--array-strategy` and `--merge-key` change that. See [`docs/arrays.md`](docs/arrays.md).

## Formats

JSON, plist, YAML and TOML each write differently: see [`docs/formats.md`](docs/formats.md).

## Nix modules

The Nix flake ships declarative wrappers for managing config files on NixOS, nix-darwin, and home-manager. Each **format** gets a `managed<Format>` option whose entries reconcile their `settings` into a live file on every activation, keeping the app's own keys and pruning keys you drop, with the previous generation as the BASE snapshot.

**Start with [`examples/`](examples)**, which has a complete, self-contained `flake.nix` for each platform (home-manager, NixOS, nix-darwin).

The flake exposes:

- `homeManagerModules.default`: `home.managed{Json,Plist,Yaml,Toml}` and `home.managedDirectory`, targets relative to `$HOME`. They run after `linkGeneration`, so an entry moved from `home.file` finds its old store link already removed.
- `nixosModules.default` / `darwinModules.default`: `environment.managed*`, absolute targets, reconciled during system activation, after `/etc` is set up.
- `overlays.default`: optional. It adds the `config-graft` CLI to `pkgs`; the modules don't need it, since they run the flake's own build by store path.

Each byte-format entry takes `settings` (freeform data) or a pre-built `source` file (any generator, template, or derivation); an entry with neither is inert. Freeform formats accept a `format` override, any `pkgs.formats`-style generator, for a validating or specially configured type. `package` overrides the config-graft build for one entry. `followSymlinks` passes `--follow-symlinks`, for a target linked to a file kept elsewhere; without it, an activation that would change a symlinked target fails rather than replacing the link. Plist entries take two more options.

`cfprefsdDomain` reconciles through `cfprefsd` (`defaults export`, then `defaults import`) instead of editing the file — macOS only, asserted at build time, per-user under home-manager and system-wide under nix-darwin. nix-darwin activates as root, so a domain id there is addressed by path as the any-user domain, `/Library/Preferences/<id>` (`NSGlobalDomain` as `/Library/Preferences/.GlobalPreferences`); anything holding a `/` is used as a path, as given. That path always writes its scratch snapshot as a **binary** plist, since `defaults export` hands over binary and `defaults import` reads it back, so nothing a domain holds is squeezed through XML on the way. The import replaces the whole domain, so when the export fails or writes nothing the entry is **skipped with a warning** and the domain left untouched; the rest of the activation carries on. A graft that fails is never imported either; the failure fails the activation (see below). A domain that does not exist yet exports as an empty dictionary and is created.

`binary` covers both ends of a plain file target: it generates a **binary** DESIRED (by running the entry's `format` generator and converting the output with `libplist` at build time) *and* forces the write with `--plist-format binary`. It defaults to `true` for a `cfprefsdDomain` entry, whose round-trip is already binary at both ends, and to `false` otherwise; set it explicitly to override either way. A file entry rarely needs it: at `false` the write follows the target's own encoding, so a binary Preferences file stays binary and keeps its dates and bytes intact. What `binary` adds is a DESIRED that can carry a value XML cannot represent — a string holding a character XML 1.0 forbids, such as the ESC `0x1B` separators in `NSUserKeyEquivalents`. If `settings` holds such a character while `binary` is false, a build-time assertion says so rather than letting the reconcile refuse at activation.

`managedDirectory` is the `directory` subcommand wrapper: each entry reconciles a `source` directory *tree* into `target`, keeping app-created files and pruning files dropped from `source`. It takes `manageRoot`, `noOwner` (set it on a non-root home-manager activation — a store-built source is root-owned), and `xattrs` (`all`/`safe`/`none`) in place of `settings`.

Every entry, `managedDirectory` included, takes `whileRunning`. Its `skip` is a list of process names that, while any of them is running, make activation skip that entry with a warning and leave it untouched. It is for an app that rewrites its own configuration while it runs, whose write between config-graft's read and its write would otherwise be lost; on a `cfprefsdDomain` entry that includes keys the entry doesn't manage, since `defaults import` replaces the whole domain. Names are matched exactly, as `pgrep -x` matches them, against every user's processes: on macOS the basename of `argv[0]` in full (normally the executable's file name), on Linux the kernel's 15-byte command name, so a longer name is cut to match. A `pgrep` that fails also skips the entry. The check narrows the window rather than closing it: an app that starts after it, or writes while the reconcile runs, can still race it.

On macOS, `whileRunning.restart` quits an app instead of skipping it. It takes bundle ids: each listed app that is running is asked to quit with an Apple Event and given `whileRunning.quitTimeout` seconds (default 10) to exit, the entry is reconciled, and the apps that were running are relaunched in the background, even when the reconcile fails. An app is never force-killed: if one does not exit in time, the quit fails, or the running check itself fails, the entry is skipped with a warning and the apps it asked to quit that have exited are relaunched (one still running, behind a save dialog say, is left alone, and not relaunched if it quits later). An app that is not installed counts as not running. A home-manager dry run only reports what it would quit. `skip` is checked first and skips without quitting anything. The first quit asks for the Automation permission, so macOS shows a prompt naming the app that runs the activation (your terminal, say) and the app being quit; until it is allowed under System Settings > Privacy & Security > Automation, the entry is skipped. A system (nix-darwin) entry quits and relaunches the app as `system.primaryUser`, which must be set, and any non-macOS host fails the build when `restart` is set.

An entry that fails to reconcile fails the activation. Under home-manager the activation stops there; under NixOS and nix-darwin every other entry is still reconciled first, and the activation then reports the failure. nix-darwin stops before it swaps `/run/current-system`, so the next switch prunes against the generation before the failed one.

A skipped entry still advances BASE. When the activation warns and carries on, the generation still records the entry's settings as the ones last applied, so a key removed from `settings` in that generation is never pruned: it stays in the app's configuration until you remove it by hand. That happens for a `skip` match (or a failed `pgrep`), a blocked `restart`, and a failed or empty `defaults export`. A failed graft or `defaults import` fails the activation (above), which under home-manager and nix-darwin ends it before BASE moves, so the next activation retries against the old one; on nix-darwin that holds only until a reboot, since `darwin-rebuild` has already pointed the system profile at the failed generation and boot re-links `/run/current-system` to it. It cuts both ways: entries that did succeed in a failed run are next reconciled against the older BASE too. NixOS never ends early: it records a failed snippet and carries on, so there every skip and failure advances BASE. This is deliberate: BASE comes from the previous generation rather than from mutable state.

A home-manager `flake.nix` sketch:

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

## Directory mode

`config-graft directory dest/ desired/` reconciles a whole tree, file by file, including mode, owner and extended attributes. See [`docs/directory.md`](docs/directory.md).

## Develop

All tooling comes from the flake:

```sh
nix develop          # cargo, rustc, clippy, rustfmt, rust-analyzer, cargo-llvm-cov
cargo test           # unit + integration tests
cargo clippy --all-targets -- -D warnings
cargo llvm-cov       # source-based coverage (LLVM_COV/PROFDATA preset by the shell)
```

Or build/test hermetically through Nix (runs the test suite in `checkPhase`):

```sh
nix build            # ./result/bin/config-graft
nix run . -- --help
```

## Related

- Modeled on `kubectl apply`'s three-way merge (against its `last-applied-configuration`), scoped to a single local file rather than a cluster object.
- The three-way merge of **ordered arrays** draws on Schwägerl, Uhrig & Westfechtel, "A graph-based algorithm for three-way merging of ordered collections in EMF models," *Science of Computer Programming* 113 (2015), pp. 51–81, [doi:10.1016/j.scico.2015.02.010](https://doi.org/10.1016/j.scico.2015.02.010) ([open-access conference version](https://www.scitepress.org/papers/2014/47021/47021.pdf)).
- [`SPEC.md`](SPEC.md) — the full specification: semantics, exit codes, edge cases.
