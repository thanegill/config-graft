# config-graft

Three-way reconcile for **app-owned JSON, plist, YAML, and TOML files**.

It deep-merges a *managed subset* (DESIRED) into a file the application also
writes to (TARGET), while:

- **preserving** keys the app/user wrote that you don't manage,
- **pruning** keys you used to manage but dropped — but only if the user hasn't
  changed them, using a **BASE** snapshot (the previously-applied config) as the
  merge ancestor.

## Usage

```sh
config-graft <TARGET> <DESIRED> [BASE]

config-graft config.json desired.json .state/last-applied.json
config-graft --check config.json desired.json base.json    # exit 3 if it would change
config-graft --stdout --diff config.json desired.json       # preview without writing
config-graft --array-strategy replace config.json desired.json  # own the list wholesale
config-graft --merge-key name config.json desired.json          # match list-of-objects by `name`

config-graft app.plist desired.plist base.plist             # same merge, plist files
config-graft --format plist config desired                  # force plist on any name
config-graft --plist-binary app.plist desired.plist         # write a binary plist

config-graft config.yaml desired.yaml                       # YAML, keeping comments
config-graft config.toml desired.toml                       # TOML, keeping comments
```

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
**TOML** are supported. The format is inferred from TARGET's extension
(`.plist` → plist, `.yaml`/`.yml` → YAML, `.toml` → TOML, else JSON) and governs
every file in the run (TARGET, DESIRED, BASE, and output) — there is no
cross-format conversion. Override detection with `--format json|plist|yaml|toml`.

Plist notes:

- Reads accept **both** XML and binary plist. Output is normalized **XML by
  default**; pass `--plist-binary` to write a binary plist instead.
- plist's `Date`/`Data`/`Uid` scalars are atomic leaves and round-trip losslessly.
- plist has no `null`. `--indent` is JSON-only; passing it with plist is an error.

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

- `homeManagerModules.default`: `home.managed{Json,Plist,Yaml,Toml}`, targets
  relative to `$HOME`.
- `nixosModules.default` / `darwinModules.default`: `environment.managed*`,
  absolute targets, reconciled during system activation.
- `overlays.default`: optional. It adds the `config-graft` CLI to `pkgs`; the
  modules don't need it, since they run the flake's own build by store path.

Each entry takes `settings` (freeform data) or a pre-built `source` file (any
generator, template, or derivation); an entry with neither is inert. Freeform
formats accept a `format` override, any `pkgs.formats`-style generator, for a
validating or specially configured type. `package` overrides the config-graft
build for one entry. Plist entries accept `cfprefsdDomain` to reconcile through
`cfprefsd` (`defaults`/`plutil`) instead of editing the file; that path is macOS
only (asserted at build time), per-user under home-manager and system/global
under nix-darwin.

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
