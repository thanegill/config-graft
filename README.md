# config-graft

> **AI Disclosure:** This project falls under [Level 5: Bots coded, human understands completely](https://www.visidata.org/blog/2026/ai/). Exact model used varies by commit and is in the commit message. This is disclosed as useful context for anyone depending on, auditing, or forking this code, not because I think it lowers the bar on the quality. Reach out or report bugs the way you would for any other project.

Three-way reconcile/merge for **JSON, plist, YAML, and TOML files** or a
**directory tree**.

It deep-merges a *managed subset* (DESIRED) into a file the application also
writes to (TARGET), while:

- **preserving** keys the app/user wrote that you don't manage,
- **pruning** keys you used to manage but dropped — but only if the user hasn't
  changed them, using a **BASE** snapshot (the previously-applied config) as the
  merge ancestor.

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
config-graft plist --plist-binary app.plist desired.plist    # write a binary plist

config-graft yaml config.yaml desired.yaml                  # YAML, keeping comments
config-graft toml config.toml desired.toml                  # TOML, keeping comments

config-graft directory dest/ desired/                       # reconcile a directory tree
```

The format is a required **subcommand** (`json`, `yaml`, `toml`, `plist`, or
`directory`); each subcommand exposes only the flags that apply to it, so an
unsupported flag/format pairing is a usage error rather than a runtime one.

By default (`--array-strategy merge`) two arrays are reconciled three-way against
BASE, move-aware: keep what either side has, prune a BASE element DESIRED dropped,
respect a BASE element the user deleted from TARGET, and preserve a reordering
made on either side. If TARGET and DESIRED reorder the same elements
*contradictorily*, that's a conflict: it's still resolved deterministically
(TARGET order preferred), but config-graft prints a warning to stderr naming the
array and the conflicting elements (the exit code is unchanged). For **arrays of
keyed records** (a list of objects like `servers`), pass `--merge-key name` (or
`--merge-key servers=name` to scope it to the array at that path) so `merge` matches
elements by that field and reconciles their fields — bump a managed field while keeping an
app-added one, instead of duplicating the record. The other strategies are
`replace` (**atomic** — DESIRED's list wins wholesale; the right choice when you
own the whole list, or for arrays of objects with no key field),
`concat` (append, keeping order and duplicates), and `set` (two-way union,
ignoring order and BASE). Scalars are always replaced. `null` is a real value,
not a delete sentinel — deletion is driven entirely by the BASE↔DESIRED diff.

### Keyed lists: `--merge-key`

The app owns a `servers` list and has added a `status` field to `web`; you manage
each server's `replicas` and want to bump `web` from 2 to 3. TARGET (live) and
DESIRED (managed):

```jsonc
// TARGET
{ "servers": [ { "name": "web", "replicas": 2, "status": "running" } ] }
// DESIRED
{ "servers": [ { "name": "web", "replicas": 3 } ] }
```

Without a key, `merge` compares whole objects, so the edited `web` reads as a
delete + insert — you get **two** `web` entries:

```json
{ "servers": [
  { "name": "web", "replicas": 2, "status": "running" },
  { "name": "web", "replicas": 3 }
] }
```

With `--merge-key name`, `web` is matched by its `name` and its fields reconcile
three-way — the managed `replicas` updates, the app's `status` survives, **one**
entry:

```json
{ "servers": [ { "name": "web", "replicas": 3, "status": "running" } ] }
```

Give several candidate fields (`--merge-key name,id`, first present wins), or scope
a key to the array at a path (`--merge-key servers=name`, or a dotted path like
`--merge-key spec.containers=name` — segments joined by the format separator, `:`
for plist — so same-named arrays at different depths take different rules). Keying
engages only when a key resolves and every element on both sides is an object
carrying it; otherwise `merge` falls back to whole-value matching.

## Formats

The merge engine is format-agnostic; **JSON**, Apple **plist**, **YAML**, and
**TOML** are supported (plus a **directory** mode, below). The format is chosen by
the subcommand (`config-graft json|plist|yaml|toml|directory ...`) and governs
every file in the run (TARGET, DESIRED, BASE, and output) — there is no
cross-format conversion.

Plist notes:

- Reads accept **both** XML and binary plist. Output is normalized **XML by
  default**; pass `--plist-binary` to write a binary plist instead.
- plist's `Date`/`Data`/`Uid` scalars are atomic leaves and round-trip losslessly.
- plist has no `null`. `--indent` is exposed only by the `json` subcommand.

YAML notes:

- **Comments, blank lines, and formatting are preserved** on the parts of the
  file config-graft doesn't change — it edits the existing text in place rather
  than re-emitting it. Only an empty/first-apply target is written canonically.
- For safety it edits only the well-behaved subset of YAML and **refuses (exit 1,
  leaving the file untouched) rather than risk corruption** on anchors/aliases,
  custom tags, multi-document streams, non-string keys, or a non-mapping root.
  Every write is verified to round-trip back to the intended result before it
  lands.
- `--indent` is JSON-only; passing it with YAML is an error.

TOML notes:

- Like YAML, **comments, blank lines, and formatting are preserved** on the parts
  config-graft doesn't change — it edits the existing document in place (via
  `toml_edit`) rather than re-emitting it. Only an empty/first-apply target is
  written canonically. Every write is verified to round-trip back to the intended
  result before it lands, and **refuses (exit 1, leaving the file untouched)
  rather than risk corruption** on an edit it can't make safely.
- TOML date-times round-trip losslessly as atomic leaves; TOML has no `null`.
- `--indent` is JSON-only; passing it with TOML is an error.

## Nix modules

The Nix flake ships declarative wrappers for managing config files on NixOS,
nix-darwin, and home-manager. Each **format** gets a `managed<Format>` option
whose entries reconcile their `settings` into a live file on every activation,
keeping the app's own keys and pruning keys you drop, with the previous
generation as the BASE snapshot.

**Start with [`examples/`](examples)**, which has a complete, self-contained
`flake.nix` for each platform (home-manager, NixOS, nix-darwin).

The flake exposes:

- `homeManagerModules.default`: `home.managed{Json,Plist,Yaml,Toml}` and
  `home.managedDirectory`, targets relative to `$HOME`.
- `nixosModules.default` / `darwinModules.default`: `environment.managed*`,
  absolute targets, reconciled during system activation.
- `overlays.default`: optional. It adds the `config-graft` CLI to `pkgs`; the
  modules don't need it, since they run the flake's own build by store path.

Each byte-format entry takes `settings` (freeform data) or a pre-built `source`
file (any generator, template, or derivation); an entry with neither is inert.
Freeform formats accept a `format` override, any `pkgs.formats`-style generator,
for a validating or specially configured type. `package` overrides the
config-graft build for one entry. Plist entries accept `cfprefsdDomain` to
reconcile through `cfprefsd` (`defaults`/`plutil`) instead of editing the file;
that path is macOS only (asserted at build time), per-user under home-manager and
system/global under nix-darwin. Plist entries also accept `binary = true`, which
generates a **binary** DESIRED (via `libplist` at build time) and reconciles with
`--plist-binary`, so values XML cannot represent — a string with a byte illegal in
XML 1.0, e.g. the ESC `0x1B` separators in `NSUserKeyEquivalents` — round-trip
instead of corrupting the DESIRED.

`managedDirectory` is the `directory` subcommand wrapper: each entry reconciles a
`source` directory *tree* into `target`, keeping app-created files and pruning
files dropped from `source`. It takes `manageRoot`, `noOwner` (set it on a
non-root home-manager activation — a store-built source is root-owned), and
`xattrs` (`all`/`safe`/`none`) in place of `settings`.

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

The `directory` subcommand reconciles a whole **directory tree** instead of a
single file — TARGET, DESIRED, and BASE are directories. A directory is a map and
a file or symlink is an atomic leaf, so the same three-way merge manages *which
files exist and what they contain* one filesystem level up: app-created files are
preserved, and files you stop declaring are pruned (BASE-driven, keeping user
edits). It is opt-in — you request it with the `directory` subcommand.

- **Minimal, in-place writes.** Only changed files are created/updated/deleted,
  so app-owned files keep their inode and mtime. Each file is written atomically,
  its bytes stream straight from source to destination (never buffered), and its
  content identity is a SHA-256 digest — so large trees stay cheap.
- **Full metadata.** A file's — and a directory's — **mode, owner (uid/gid), and
  extended attributes** are part of its identity: a metadata-only change is a
  change (it shows up in `--diff`), and all of it is applied on write. An
  attribute that can't be set (e.g. no privilege to `chown`, or an xattr the
  filesystem rejects) **refuses the run** (nothing lands) rather than leaving a
  half-applied entry. Manage-everything by default, with opt-outs: `--no-owner`
  leaves uid/gid alone, and `--xattrs <all|safe|none>` narrows which extended
  attributes are reconciled (`safe` skips privileged/system namespaces).
- **Symlinks** are managed by target and never followed;
  **FIFOs/sockets/devices**, non-UTF-8 filenames, and case-folding sibling name
  collisions are refused (exit 1). Replacing an app-populated directory with a
  file is refused rather than deleting content it never managed.
- The **root** directory (the one you point at) is left untouched by default;
  `--manage-root` reconciles its own attributes too.
- The `directory` subcommand exposes only `--manage-root` / `--no-owner` /
  `--xattrs` (plus the shared `--diff` / `--check`); the byte-format flags
  (`--stdout`, `--indent`, `--plist-binary`, `--array-strategy`, `--sort-keys`,
  `--merge-key`) don't exist on it, so passing one is a usage error.

```sh
config-graft directory dest/ desired/                  # reconcile a tree
config-graft directory --diff --check dest/ desired/   # preview drift
config-graft directory --manage-root dest/ desired/    # also the root's own attrs
config-graft directory --no-owner --xattrs safe dest/ desired/  # narrow metadata scope
```

The multi-file apply is best-effort, not one transaction: a crash mid-apply
leaves a partial (per-file consistent) tree that a re-run completes. Hardlinks
aren't preserved, the read→apply window is subject to TOCTOU, and an unreadable
directory aborts the whole run — see [SPEC.md](SPEC.md) §10 for the full list.

## Develop

All tooling comes from the flake:

```sh
nix develop          # cargo, rustc, clippy, rustfmt, rust-analyzer, cargo-llvm-cov
cargo test           # unit + integration tests
cargo clippy
cargo llvm-cov       # source-based coverage (LLVM_COV/PROFDATA preset by the shell)
```

Or build/test hermetically through Nix (runs the test suite in `checkPhase`):

```sh
nix build            # ./result/bin/config-graft
nix run . -- --help
```

## Related

- Modeled on `kubectl apply`'s three-way merge (against its
  `last-applied-configuration`), scoped to a single local file rather than a
  cluster object.
- The three-way merge of **ordered arrays** draws on Schwägerl, Uhrig &
  Westfechtel, "A graph-based algorithm for three-way merging of ordered
  collections in EMF models," *Science of Computer Programming* 113 (2015),
  pp. 51–81, [doi:10.1016/j.scico.2015.02.010](https://doi.org/10.1016/j.scico.2015.02.010)
  ([open-access conference version](https://www.scitepress.org/papers/2014/47021/47021.pdf)).
- [`SPEC.md`](SPEC.md) — the full specification: semantics, exit codes, edge cases.
