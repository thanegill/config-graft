# json-apply

Three-way reconcile for **app-owned JSON, plist, and YAML files**.

It deep-merges a *managed subset* (DESIRED) into a file the application also
writes to (TARGET), while:

- **preserving** keys the app/user wrote that you don't manage,
- **pruning** keys you used to manage but dropped — but only if the user hasn't
  changed them, using a **BASE** snapshot (the previously-applied config) as the
  merge ancestor.

## Usage

```sh
json-apply <TARGET> <DESIRED> [BASE]

json-apply config.json desired.json .state/last-applied.json
json-apply --check config.json desired.json base.json    # exit 3 if it would change
json-apply --stdout --diff config.json desired.json       # preview without writing
json-apply --array-strategy set config.json desired.json  # union lists, ignoring order

json-apply app.plist desired.plist base.plist             # same merge, plist files
json-apply --format plist config desired                  # force plist on any name
json-apply --plist-binary app.plist desired.plist         # write a binary plist

json-apply config.yaml desired.yaml                       # YAML, keeping comments
```

By default arrays (and scalars) are **atomic**: a managed list is replaced
wholesale. `--array-strategy` changes how two arrays combine — `concat` appends
(keeping order and duplicates) or `set` unions them ignoring order and dropping
duplicates. `null` is a real value, not a delete sentinel — deletion is driven
entirely by the BASE↔DESIRED diff.

## Formats

The merge engine is format-agnostic; **JSON**, Apple **plist**, and **YAML** are
supported. The format is inferred from TARGET's extension (`.plist` → plist,
`.yaml`/`.yml` → YAML, else JSON) and governs every file in the run (TARGET,
DESIRED, BASE, and output) — there is no cross-format conversion. Override
detection with `--format json|plist|yaml`.

Plist notes:

- Reads accept **both** XML and binary plist. Output is normalized **XML by
  default**; pass `--plist-binary` to write a binary plist instead.
- plist's `Date`/`Data`/`Uid` scalars are atomic leaves and round-trip losslessly.
- plist has no `null`. `--indent` is JSON-only; passing it with plist is an error.

YAML notes:

- **Comments, blank lines, and formatting are preserved** on the parts of the
  file json-apply doesn't change — it edits the existing text in place rather
  than re-emitting it. Only an empty/first-apply target is written canonically.
- For safety it edits only the well-behaved subset of YAML and **refuses (exit 1,
  leaving the file untouched) rather than risk corruption** on anchors/aliases,
  custom tags, multi-document streams, non-string keys, or a non-mapping root.
  Every write is verified to round-trip back to the intended result before it
  lands.
- `--indent` is JSON-only; passing it with YAML is an error.

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
nix build            # ./result/bin/json-apply
nix run . -- --help
```

## Related

- Modeled on `kubectl apply`'s three-way merge (against its
  `last-applied-configuration`), scoped to a single local file rather than a
  cluster object.
- [`SPEC.md`](SPEC.md) — the full specification: semantics, exit codes, edge cases.
