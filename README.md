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
config-graft --array-strategy set config.json desired.json  # union lists, ignoring order

config-graft app.plist desired.plist base.plist             # same merge, plist files
config-graft --format plist config desired                  # force plist on any name
config-graft --plist-binary app.plist desired.plist         # write a binary plist

config-graft config.yaml desired.yaml                       # YAML, keeping comments
config-graft config.toml desired.toml                       # TOML, keeping comments
```

By default arrays (and scalars) are **atomic**: a managed list is replaced
wholesale. `--array-strategy` changes how two arrays combine — `concat` appends
(keeping order and duplicates) or `set` unions them ignoring order and dropping
duplicates. `null` is a real value, not a delete sentinel — deletion is driven
entirely by the BASE↔DESIRED diff.

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
