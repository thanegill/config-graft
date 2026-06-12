# `json-apply` — three-way reconcile for app-owned JSON files

Declaratively reconcile a managed *subset* of a JSON file into a file the
application also writes to, using a last-applied snapshot as the merge base —
i.e. `kubectl apply`'s three-way merge, scoped to a single local file.

---

## 1. Purpose & motivation

Config files like `claude_desktop_config.json` are **co-owned**: a tool
(Nix/home-manager) wants to declare some keys, while the app itself writes others
(auth tokens, UI state). A plain overwrite destroys the app's keys; a plain
deep-merge can never *remove* a key the declarer stopped managing. `json-apply`
solves both by doing a three-way merge against a stored snapshot of "what we
declared last time."

It is the file-scoped equivalent of `kubectl apply` +
`last-applied-configuration`.

## 2. Concepts

| Term        | Role                                                | kubectl analogue            |
| ----------- | --------------------------------------------------- | --------------------------- |
| **TARGET**  | the live file on disk; read and written in place    | the live object             |
| **DESIRED** | the config we want to apply (a subset)              | the new Resource Config     |
| **BASE**    | snapshot of the DESIRED we applied last time        | `last-applied-configuration`|

The caller is responsible for persisting BASE between runs (e.g. as a file in the
previous generation). The tool only reads it.

## 3. CLI interface

### Synopsis

```
json-apply [OPTIONS] <TARGET> <DESIRED> [BASE]
json-apply [OPTIONS] --base <BASE> <TARGET> <DESIRED>
```

### Arguments

- `TARGET` — path to the file to reconcile, in place. Created (with parents) if absent.
- `DESIRED` — path to the managed JSON. Must be a valid JSON object.
- `BASE` — path to the previous snapshot. Optional; absent/empty/invalid ⇒ no pruning (first-run behavior).

### Options

- `--base <PATH>` — alternative to the positional BASE.
- `--no-prune` — deep-merge only; never delete keys (ignore BASE for removals).
- `--stdout` — write the result to stdout; do not modify TARGET.
- `--diff` — print a human-readable, leaf-level diff of the changes.
- `--check` — exit non-zero if applying *would* change TARGET; write nothing (CI / idempotence).
- `--indent <N|tab>` — output indentation (default: 2 spaces).
- `--sort-keys` — sort every object's keys in the output (default: preserve TARGET order, append new keys).
- `--array-strategy <replace|concat|set>` — how DESIRED arrays combine with TARGET arrays: `replace` (atomic, default), `concat` (append, keeping order and duplicates), or `set` (union, ignoring order and dropping duplicates).

### Exit codes

- `0` — success.
- `1` — runtime error (DESIRED unreadable/invalid, TARGET unwritable, I/O).
- `2` — usage error (bad args; emitted by the arg parser).
- `3` — with `--check`: a change is pending (TARGET differs from the reconciled result).

`--check`'s distinct code lets activation scripts detect drift.

## 4. Algorithm

Given parsed objects `target`, `desired`, `base` (each defaulting to `{}`):

1. **Compute managed leaf paths** of `base` and `desired`. A *leaf path* descends **only through objects**; arrays and scalars are atomic leaves (see §5).
2. **Determine removals**: leaf paths in `base` not in `desired`.
3. **Prune** each removal from `target` **iff** `target`'s value at that path still equals `base`'s value at that path (don't clobber a value the user/app changed).
4. **Deep-merge** `desired` into the pruned target; on a leaf conflict, `desired` wins. Objects merge recursively; non-objects replace.
5. **Collapse empties**: delete any object left empty solely by step 3 (cascading to parents).
6. Serialize and write atomically (§8).

Keys in `target` that were never in `base` or `desired` are always preserved.

## 5. Merge & type semantics

- **Objects** — merged recursively (the only container that merges).
- **Arrays** — combine per `--array-strategy`:
  - `replace` (default) — **atomic**: DESIRED's array replaces TARGET's wholesale; no element-wise merge, even for arrays of objects. *Rationale: positional element-merge is ambiguous and index-shift-prone; matches kubectl's "atomic list" default.*
  - `concat` — DESIRED's elements are appended to TARGET's, preserving order and duplicates.
  - `set` — union of both arrays, ignoring order and dropping duplicate values (membership by deep equality). Idempotent: re-applying a DESIRED already contained in TARGET is a no-op.

  The strategy applies only when **both** sides are arrays; an array-vs-non-array always replaces. **Pruning is always atomic** — a managed array is removed or kept as one leaf regardless of strategy.
- **Scalars** — DESIRED replaces.
- **`null`** — a normal value, not a delete sentinel. Removal is driven by BASE↔DESIRED diffing, not by RFC 7386 null.
- **Type changes** (e.g. object→array at a key) — DESIRED's value replaces wholesale.

## 6. Pruning / user-edit preservation (the three-way bit)

A managed key is removed **only** when:

- it was in BASE, **and**
- it is absent from DESIRED, **and**
- TARGET still holds exactly the BASE value (deep-equal).

If the user/app changed the value, it is left intact. With no BASE, nothing is
ever pruned.

## 7. File handling & robustness

- **Missing/unparseable/non-object TARGET** ⇒ treated as `{}` (TARGET becomes a copy of DESIRED, structurally).
- **Missing/empty/unparseable/non-object BASE** ⇒ pruning disabled (first run).
- **Invalid DESIRED** ⇒ hard error (exit 1); TARGET untouched.
- Parent directories of TARGET created as needed.

## 8. Atomicity & formatting

- Write to a temp file in TARGET's directory, `fsync`, then `rename(2)` over TARGET (atomic; no torn writes).
- Preserve TARGET's existing file mode; default `0644` for new files.
- Deterministic output: stable key ordering (per `--sort-keys`), fixed indentation, single trailing newline. Running twice with the same inputs is a no-op (idempotent) and `--check`-clean; an unchanged result skips the write entirely.

## 9. Non-goals

- Not a general diff/patch tool (use `jd`).
- Not RFC 7386 (no null-deletes) or RFC 6902.
- No comment/formatting preservation in TARGET (round-trips as canonical JSON). JSONC/JSON5 out of scope.
- Does not manage the BASE snapshot lifecycle — the caller stores/rotates it.

## 10. Known trade-offs

- Because removal is snapshot-driven (not null-sentinel), the tool **cannot set a managed key to `null`-meaning-delete**; `null` is a real value. This is intentional and the inverse of RFC 7386's limitation.
- Atomic arrays mean you can't manage a single element of an app-written list; you own the whole array or none of it.

## 11. Examples

```sh
# First apply (no base): TARGET gets DESIRED's keys, app keys preserved.
json-apply config.json desired.json

# Subsequent apply with snapshot: prunes keys we dropped, keeps user edits.
json-apply config.json desired.json .state/last-applied.json

# Dry-run drift check in CI:
json-apply --check config.json desired.json .state/last-applied.json || echo "would change"
```

Reconcile example (`a` dropped from DESIRED, `b` user-edited, `appOnly` untouched):

```
TARGET  {"a":1,"b":5,"appOnly":true}
DESIRED {"c":3}
BASE    {"a":1,"b":2}
RESULT  {"b":5,"appOnly":true,"c":3}   # a pruned (==base); b kept; appOnly kept; c added
```

## 12. Test matrix (must-pass)

Deep-merge wins; target-only keys survive; deeply nested merge; type changes
(object↔scalar) replace; prune dropped leaf; prune empties parent; keep
user-edited scalar; **array replaced wholesale**; **array-of-objects atomic**;
**prune dropped list (atomic, no index-shift)**; **prune list empties parent**;
**keep user-edited list**; **arrays concat (order + dups kept)**; **arrays set
(union, order-independent, deduped)**; **set idempotent on subset**; **strategy
applies only when both are arrays**; `null` is a value not a delete;
non-object/missing/invalid TARGET coerced to empty; missing/invalid BASE (no
prune); usage error exit 2; invalid `--array-strategy` exit 2; `--check`
idempotence; `--stdout` leaves TARGET untouched; `--sort-keys`/`--indent`
output; `--diff` add/remove/change lines; file mode preserved.

## 13. Implementation

Implemented in **Rust** (this repo):

- `src/reconcile.rs` — the pure algorithm (no I/O), unit-tested against §12.
- `src/main.rs` — CLI (clap), I/O, atomic write, `--check`/`--diff`/`--stdout`.
- `tests/cli.rs` — integration tests exercising exit codes and file behavior.
- Output uses `serde_json` with `preserve_order` so TARGET key order is kept and
  new keys are appended.

A `jq` + shell implementation of the same core (minus `--check`/`--diff` and
atomic rename) also exists as `sync-json` in the author's nixos-config; this Rust
binary is the hardened generalization.
