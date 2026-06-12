# json-apply

Three-way reconcile for **app-owned JSON files** — `kubectl apply`'s merge
semantics scoped to a single local file.

It deep-merges a *managed subset* (DESIRED) into a file the application also
writes to (TARGET), while:

- **preserving** keys the app/user wrote that you don't manage,
- **pruning** keys you used to manage but dropped — but only if the user hasn't
  changed them, using a **BASE** snapshot (the previously-applied config) as the
  merge ancestor.

See [`SPEC.md`](SPEC.md) for the full specification.

## Usage

```sh
json-apply <TARGET> <DESIRED> [BASE]

json-apply config.json desired.json .state/last-applied.json
json-apply --check config.json desired.json base.json    # exit 3 if it would change
json-apply --stdout --diff config.json desired.json       # preview without writing
json-apply --array-strategy set config.json desired.json  # union lists, ignoring order
```

By default arrays (and scalars) are **atomic**: a managed list is replaced
wholesale. `--array-strategy` changes how two arrays combine — `concat` appends
(keeping order and duplicates) or `set` unions them ignoring order and dropping
duplicates. `null` is a real value, not a delete sentinel — deletion is driven
entirely by the BASE↔DESIRED diff.

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
