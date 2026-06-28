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

The flake ships declarative wrappers so you rarely call the CLI by hand. Each
**format** gets a `managed<Format>` option whose entries reconcile their
`settings` into a live file on every activation — keeping the app's own keys and
pruning keys you drop, with the previous generation as the BASE snapshot.

- `homeManagerModules.default` — `home.managed{Json,Plist,Yaml,Toml}` (targets
  relative to `$HOME`). The plist option also takes a `cfprefsdDomain` for macOS
  preference domains.
- `nixosModules.default` / `darwinModules.default` — `environment.managed{Json,Plist,Yaml,Toml}`
  (absolute targets, reconciled during system activation).
- `overlays.default` — adds `config-graft` to `pkgs`, which the modules use by
  default (or set each entry's `package`).

Each module is also exposed under the name `config-graft` (e.g.
`homeManagerModules.config-graft`), identical to `default`.

```nix
{
  inputs.config-graft.url = "github:thanegill/config-graft";

  # NixOS / nix-darwin: apply the overlay so the modules find the binary.
  nixpkgs.overlays = [ inputs.config-graft.overlays.default ];
}
```

```nix
# home-manager: graft a few keys into a file the app keeps rewriting.
{
  imports = [ inputs.config-graft.homeManagerModules.default ];

  home.managedJson.claude-code = {
    target = ".claude/settings.json";
    settings.permissions.ask = [ "Bash(git push)" ];
  };

  home.managedYaml.app = {
    target = ".config/app/config.yaml";   # comments in the live file are preserved
    settings.theme = "dark";
  };
}
```

An entry with empty `settings` is inert. Freeform formats (JSON/YAML/TOML) accept
a `format` override (any `pkgs.formats`-style generator) for schema-checked
output. There's a module per platform — [`managed.nix`](modules/managed.nix),
[`managed-nixos.nix`](modules/managed-nixos.nix),
[`managed-darwin.nix`](modules/managed-darwin.nix) — sharing the per-format specs
and assembly in [`lib.nix`](modules/lib.nix); see them for the option docs and the
snapshot/prune rationale.

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
