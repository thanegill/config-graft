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

By default (`--array-strategy merge`) two arrays are reconciled three-way against BASE, move-aware: keep what either side has, prune a BASE element DESIRED dropped, respect a BASE element the user deleted from TARGET, and preserve a reordering made on either side. If TARGET and DESIRED reorder the same elements *contradictorily*, that's a conflict: it's still resolved deterministically (TARGET order preferred), but config-graft prints a warning to stderr naming the array and the conflicting elements (the exit code is unchanged). For **arrays of keyed records** (a list of objects like `servers`), pass `--merge-key name` (or `--merge-key servers=name` to scope it to the array at that path) so `merge` matches elements by that field and reconciles their fields — bump a managed field while keeping an app-added one, instead of duplicating the record. The other strategies are `replace` (**atomic** — DESIRED's list wins wholesale; the right choice when you own the whole list, or for arrays of objects with no key field), `concat` (append, keeping order and duplicates), and `set` (two-way union, ignoring order and BASE). Scalars are always replaced. `null` is a real value, not a delete sentinel — deletion is driven entirely by the BASE↔DESIRED diff.

### Keyed lists: `--merge-key`

The app owns a `servers` list and has added a `status` field to `web`; you manage each server's `replicas` and want to bump `web` from 2 to 3. TARGET (live) and DESIRED (managed):

```jsonc
// TARGET
{ "servers": [ { "name": "web", "replicas": 2, "status": "running" } ] }
// DESIRED
{ "servers": [ { "name": "web", "replicas": 3 } ] }
```

Without a key, `merge` compares whole objects, so the edited `web` reads as a delete + insert — you get **two** `web` entries:

```json
{ "servers": [
  { "name": "web", "replicas": 2, "status": "running" },
  { "name": "web", "replicas": 3 }
] }
```

With `--merge-key name`, `web` is matched by its `name` and its fields reconcile three-way — the managed `replicas` updates, the app's `status` survives, **one** entry:

```json
{ "servers": [ { "name": "web", "replicas": 3, "status": "running" } ] }
```

Give several candidate fields (`--merge-key name,id`, first present wins), or scope a key to the array at a path (`--merge-key servers=name`, or a dotted path like `--merge-key spec.containers=name` — segments joined by the format separator, `:` for plist — so same-named arrays at different depths take different rules). Keying engages only when a key resolves and every element on both sides is an object carrying it; otherwise `merge` falls back to whole-value matching.

## Formats

The merge engine is format-agnostic; **JSON**, Apple **plist**, **YAML**, and **TOML** are supported (plus a **directory** mode, below). The format is chosen by the subcommand (`config-graft json|plist|yaml|toml|directory ...`) and governs every file in the run (TARGET, DESIRED, BASE, and output) — there is no cross-format conversion.

JSON notes:

- Numbers keep their **source spelling**, so a high-precision decimal, an integer larger than `u64`, or an exponent past `f64`'s range round-trips rather than being shortened, turned into `1.2345e29`, or (for the exponent) making the whole file unreadable. They compare by value across every spelling — `10`, `10.0` and `1e1` are one number — so a value the app rewrote in another form still prunes, and a DESIRED spelled differently from TARGET leaves the file alone. Every spelling survives, including an exponent's: `1e1`, `1E2` and `2.50` reach disk exactly as the file wrote them.
- Output is pretty-printed per `--indent`, preserving key order.

Plist notes:

- Reads accept **both** XML and binary plist. Output **keeps the encoding the target already has** (`--plist-format follow`, the default): binary stays binary, XML stays XML, and a target that is absent, empty or neither is written as canonical XML. `--plist-format xml` and `--plist-format binary` force one.
- `Date`/`Data`/`Uid` are atomic leaves and round-trip losslessly, with one exception: an XML run floors a `Date` to a whole second. CFPropertyList's XML parser rejects a fractional-second `<date>`, and a date read out of a binary plist — which is what `defaults export` hands over — almost always has one. The floor is applied as each input is read, so TARGET, DESIRED and BASE stay comparable and a managed date key still prunes; every floored date is reported on stderr. A binary target is written as binary, so a date the app owns is normally never floored at all — reaching this needs `--plist-format xml`, or a date that was in an XML file to begin with.
- Flooring can make two instants under a second apart into one value. Where the merge then keeps only one, the run **says so and writes anyway** — the same normalization is what keeps TARGET comparable to BASE, so refusing would strand the run rather than protect it. An array config-graft does not manage is copied through untouched and never trips it.
- An XML run **refuses** (exit 1, target untouched) when it would *introduce* a string or key holding a character an XML plist cannot carry at all: a C0 control other than tab, newline or carriage return — such as the ESC `0x1B` separators in `NSUserKeyEquivalents`. macOS tolerates those bytes, so writing them would produce a file that loads on macOS and is invalid everywhere else. A value the file already holds is passed through untouched rather than refused, so a run that changes nothing never fails on a file macOS itself wrote — but that leaves a file no conforming parser accepts, so each one is reported on stderr. This applies only when the target is already XML; rewriting a **binary** plist as XML writes every byte afresh, so an unrepresentable value there is refused — reachable only by forcing `--plist-format xml`, since a binary target is otherwise written as binary. A file counts as XML however its prologue opens: comments and processing instructions ahead of the `<?xml` declaration are read the same way every plist parser reads them. When a value is refused because nothing could carry over, the error says which it was — a binary target, or one not recognized as XML. A carriage return is written as `&#13;`, which XML's line-ending normalization leaves alone, so it survives.
- A **binary** write sidesteps all of that: nothing is floored and no byte is out of reach, so values pass through as they stand. Since a binary target is written as binary by default, that is already what a macOS Preferences file gets.
- plist has no `null`. `--indent` is exposed only by the `json` subcommand.

YAML notes:

- **Comments, blank lines, and formatting are preserved** on the parts of the file config-graft doesn't change — it edits the existing text in place rather than re-emitting it. Only an empty/first-apply target is written canonically.
- For safety it edits only the well-behaved subset of YAML and **refuses (exit 1, leaving the file untouched) rather than risk corruption** on custom tags, multi-document streams, non-string keys, a non-mapping root, an edit that would change what an alias resolves to, or editing inside an alias to a non-empty mapping or sequence. Anchors and aliases are otherwise kept as written, except in an entry config-graft re-renders (a changed sequence, a scalar turned into a collection), which is written expanded. Every write is verified to round-trip back to the intended result before it lands.
- `--indent` is JSON-only; passing it with YAML is an error.

TOML notes:

- Like YAML, **comments, blank lines, and formatting are preserved** on the parts config-graft doesn't change — it edits the existing document in place (via `toml_edit`) rather than re-emitting it. Only an empty/first-apply target is written canonically. Every write is verified to round-trip back to the intended result before it lands, and **refuses (exit 1, leaving the file untouched) rather than risk corruption** on an edit it can't make safely.
- TOML date-times round-trip losslessly as atomic leaves; TOML has no `null`.
- `--indent` is JSON-only; passing it with TOML is an error.

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
