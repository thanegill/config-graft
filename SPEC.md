# `config-graft` — three-way reconcile for app-owned JSON, plist, YAML, and TOML files

Declaratively reconcile a managed *subset* of a JSON, plist, YAML, or TOML file into a
file the application also writes to, using a last-applied snapshot as the merge
base — i.e. `kubectl apply`'s three-way merge, scoped to a single local file. The
merge engine is format-agnostic; see §5a for the supported formats.

---

## 1. Purpose & motivation

Config files like `claude_desktop_config.json` are **co-owned**: a tool
(Nix/home-manager) wants to declare some keys, while the app itself writes others
(auth tokens, UI state). A plain overwrite destroys the app's keys; a plain
deep-merge can never *remove* a key the declarer stopped managing. `config-graft`
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
config-graft [OPTIONS] <TARGET> <DESIRED> [BASE]
config-graft [OPTIONS] --base <BASE> <TARGET> <DESIRED>
```

### Arguments

- `TARGET` — path to the file to reconcile, in place. Created (with parents) if absent. With `--format directory` it is a directory tree (§5b).
- `DESIRED` — path to the managed config. Must be a valid JSON object / plist dictionary / YAML mapping / TOML table (or, with `--format directory`, a directory).
- `BASE` — path to the previous snapshot. Optional; absent/empty/invalid ⇒ no pruning (first-run behavior).

### Options

- `--base <PATH>` — alternative to the positional BASE.
- `--no-prune` — deep-merge only; never delete keys (ignore BASE for removals).
- `--stdout` — write the result to stdout; do not modify TARGET.
- `--diff` — print a human-readable, leaf-level diff of the changes. Key-paths use the format's separator: `.` for JSON/YAML/TOML, `:` for plist (PlistBuddy) — same as the `merge` conflict warning.
- `--check` — exit non-zero if applying *would* change TARGET; write nothing (CI / idempotence).
- `--indent <N|tab>` — output indentation (default: 2 spaces). **JSON only** — passing it with another format is an error (exit 1).
- `--sort-keys` — sort every object's keys in the output (default: preserve TARGET order, append new keys).
- `--array-strategy <merge|replace|concat|set>` — how DESIRED arrays combine with TARGET arrays: `merge` (three-way, move-aware against BASE; **the default**), `replace` (atomic), `concat` (append, keeping order and duplicates), or `set` (two-way union, ignoring order and dropping duplicates).
- `--merge-key <[PATH=]FIELD>` — for `merge`, identify object-array elements by a field so keyed records are merged in place instead of matched by whole value (see §5). `FIELD` / `f1,f2` (candidate fields, first present wins) applies to any object-array; `PATH=FIELD` scopes it to the array at `PATH` — its full path from the document root, segments joined by the format separator (`.`, or `:` for plist). Repeatable.
- `--format <json|plist|yaml|toml|directory>` — input/output format. Default: inferred from TARGET's extension (`.plist` → plist, `.yaml`/`.yml` → yaml, `.toml` → toml, else json). Governs every file in the run (§5a). `directory` is never inferred — it must be passed explicitly, and makes TARGET/DESIRED/BASE *directories* rather than files (§5b).
- `--plist-binary` — write plist output as binary instead of XML. **Plist only** — passing it with another format is an error (exit 1).
- `--manage-root` — also reconcile the TARGET directory's *own* attributes (mode/owner/xattrs), not just its contents. **`--format directory` only** — passing it with another format is an error (exit 1). Off by default (§5b).

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
  - `replace` — **atomic**: DESIRED's array replaces TARGET's wholesale; no element-wise merge, even for arrays of objects. The right choice when you own the whole list, or when its elements are structurally anonymous objects that `merge` can't match. (This is kubectl's "atomic list" behavior, which config-graft used as its default before `merge` existed.)
  - `concat` — DESIRED's elements are appended to TARGET's, preserving order and duplicates.
  - `set` — *two-way* union of both arrays, ignoring order and dropping duplicate values (membership by deep equality). Idempotent: re-applying a DESIRED already contained in TARGET is a no-op. BASE is not consulted, so an element dropped from DESIRED is **not** removed.
  - `merge` (**the default**) — *three-way* reconciliation using BASE, in two parts. **Membership**: an element survives iff it is present in TARGET or DESIRED and was deleted on **neither** branch relative to BASE — a BASE element dropped from DESIRED is pruned; a BASE element the user removed from TARGET stays removed; an insertion on either side is kept (membership by deep equality; duplicate values collapse, so it is an ordered set, not a bag). **Ordering**: survivors are ordered *move-aware* by a generalized topological sort (GTS) — a relative order that BASE and a branch agree on is preserved even when an element was moved, and a contradictory cross-over move (each side reorders the other's pair) is broken consistently with one input rather than by inventing a new order. Every non-deterministic choice takes a fixed tie-break — earliest position in TARGET, then DESIRED, then insertion order — so output is deterministic and idempotent. A contradictory cross-over move (a cycle) is a **conflict**: the result is still produced deterministically (TARGET order preferred), but a warning naming the array's path (format-separated: `.`, or `:` for plist) and the elements involved is printed to stderr so the reorder isn't applied silently. Conflicts are diagnostic only — they do **not** change the exit code. With no BASE every element is an insertion, so `merge` degenerates to `set` (TARGET order, then DESIRED-only insertions appended). Elements are matched by whole value by default (well-defined for uniquely-valued elements). **Keyed matching** (`--merge-key`) instead identifies object-array elements by a field, so a record whose non-key field changes is *merged in place* (its fields reconciled three-way, like top-level object keys) rather than seen as a delete+insert (a duplicate). `FIELD` (or `f1,f2` — candidates, first present wins) applies to any object-array; `PATH=FIELD` scopes it to the array whose full path from the document root equals `PATH` (segments joined by the format separator — `.`, or `:` for plist), so same-named arrays at different depths (`spec.containers` vs `spec.template.containers`) can take different rules. Because arrays are atomic in this path model, a scope path addresses within one array-free subtree; a matched keyed record is reconciled as its own subtree, so a path is anchored at the document root or the nearest enclosing keyed record. It engages only when a key resolves and **every** element of both sides is an object carrying one; otherwise value matching. Membership (prune/keep) and move-aware ordering then run on the keys. Duplicate keys within one side collapse (first wins). A conflict *inside* a merged record (a contradictory reorder of one of its nested arrays) is surfaced too: its path carries a `[field=value]` element selector attached to the array key, e.g. `servers[name="web"].tags` (`servers[name="web"]:tags` for plist). Arrays of anonymous objects with no key are still atomic — use `replace`. Algorithm: Schwagerl, Uhrig & Westfechtel, *Sci. Comput. Program.* 113 (2015), [doi:10.1016/j.scico.2015.02.010](https://doi.org/10.1016/j.scico.2015.02.010).

  The strategy applies only when **both** sides are arrays; an array-vs-non-array always replaces. **Pruning is always atomic** — a managed array dropped wholesale from DESIRED is removed or kept as one leaf regardless of strategy (only `merge`/`set` reconcile *within* an array still present on both sides).
- **Scalars** — DESIRED replaces.
- **`null`** — a normal value, not a delete sentinel. Removal is driven by BASE↔DESIRED diffing, not by RFC 7386 null. (JSON/YAML have null; plist/TOML don't.)
- **Type changes** (e.g. object→array at a key) — DESIRED's value replaces wholesale.

## 5a. Formats

The engine runs on an internal value model; each format has a codec that maps
its native value type ⇄ that model. Reconciliation is **homogeneous** — one
format governs TARGET, DESIRED, BASE, and output — so a run is never a
cross-format conversion. The format is inferred from TARGET's extension
(`.plist` → plist, `.yaml`/`.yml` → YAML, `.toml` → TOML, else JSON) and can be
forced with `--format`.

- **JSON** — objects, arrays, strings, numbers, booleans, `null`. Output is
  pretty-printed per `--indent`, key order preserved (§8).
- **plist** — dictionaries, arrays, strings, integers, reals, booleans, and the
  plist-only scalars **`Date`**, **`Data`**, and **`Uid`**. The engine treats
  every non-dictionary value as an atomic leaf, so these exotic scalars
  **round-trip losslessly** without the engine understanding them. Reads accept
  **both XML and binary** plist; output is normalized **XML by default** (a binary
  or differently-formatted target is rewritten as canonical XML on first apply —
  the same normalize-on-write behavior JSON has), or **binary** with
  `--plist-binary`. plist has no `null`.
- **YAML** (1.2, via `saphyr`) — mappings, sequences, strings, integers, floats,
  booleans, `null`. **Unlike JSON/plist, an existing target is *not* normalized:**
  config-graft edits the original file text in place, so **comments, blank lines,
  quoting, and indentation are preserved** on every region it doesn't change.
  Only an empty/first-apply target is written canonically. To guarantee this is
  safe, it edits only the well-behaved subset and **refuses (exit 1, file
  untouched) rather than risk corruption** on anchors/aliases, custom tags,
  multi-document streams, non-string keys, or a non-mapping root; every write is
  verified to round-trip back to the reconciled result before it lands. `--indent`
  does not apply.
- **TOML** (via `toml_edit`) — tables, arrays, strings, integers, floats,
  booleans, and date-times (treated as atomic leaves, round-tripping losslessly);
  TOML has no `null`. Like YAML, **an existing target is *not* normalized:**
  config-graft mutates the original document in place, so **comments, blank lines,
  and formatting are preserved** on every region it doesn't change. Only an
  empty/first-apply target is written canonically (idiomatic `[section]` tables).
  Every write is verified to round-trip back to the reconciled result and
  **refuses (exit 1, file untouched) rather than risk corruption** on an edit it
  can't make safely. A TOML document's root is always a table, so a desired TOML
  file that parses is always a valid mapping. `--indent` does not apply.

## 5b. Directory mode (`--format directory`)

Instead of a single file, TARGET / DESIRED / BASE are **directory trees**, and
config-graft reconciles *which files exist and their contents* — the same
three-way merge, one filesystem level up. It is opt-in (`--format directory` is
never inferred). The tree maps onto the same value model as every other format:

- a **directory** is the map/object shape (the container that merges); it also
  carries its **own metadata** (mode/owner/xattrs), reconciled the same way a
  file's is. The **root** directory (the target you point at) is the exception:
  by default its own attributes are left alone — config-graft never chmod/chown's
  the directory you aim it at, only what lives inside — but `--manage-root`
  reconciles the root too (useful when you own the whole tree; note a DESIRED tree
  owned by another user, e.g. a Nix store path, then needs privilege to apply);
- a **regular file** is an atomic leaf carrying its whole contents **and its
  metadata** (see below) — config-graft never merges *within* a file, and a
  metadata-only change counts as a change;
- a **symlink** is an atomic leaf carrying its target; it is **never followed**
  (a symlink to a directory is a leaf, not a directory), and dangling links are
  fine. Symlinks carry no metadata.

A file's contents are held as a **content handle** (length + SHA-256 digest +
source path), not loaded into memory — so the tree costs O(number of files), and
bytes stream straight from the source to the destination on write. A file's
**metadata** is a generic, filesystem-agnostic map of `name → value`, so new
attribute kinds need no type change. Tracked today: **mode** (permission bits,
incl. setuid/sticky), **owner** (`uid`/`gid`), and every **extended attribute**
the OS reports. All of it is part of the file's identity: two files are equal
only if content *and* every tracked attribute match.

Reading extended attributes is best-effort — a filesystem that doesn't support
them contributes none. **Applying** metadata is strict: attributes are set on the
temp file *before* the atomic rename, and any failure (e.g. no privilege to
`chown`, or a filesystem that can't store an xattr) **refuses the run** (exit 1,
nothing landed) rather than leaving a partially-attributed file. Because owner is
tracked, a DESIRED tree owned by a different user (e.g. a Nix store path owned by
root) will try to `chown` and therefore needs privilege to apply — run as that
owner, or keep DESIRED's ownership matching the target.

Reads **do not follow** symlinks inside the tree and **refuse (exit 1, nothing
written)** on a FIFO/socket/device or a non-UTF-8 filename. A TARGET that exists
but is not a directory is a hard error (unlike the single-file coerce-to-empty
rule — we won't silently treat a plain file as an empty tree and delete it).

Writes are **minimal and in place** (like YAML/TOML, not like JSON's canonical
rewrite): config-graft diffs the reconciled tree against the current one and only
creates/updates/deletes what changed, so **app-owned files keep their inode and
mtime**. Each individual file write is atomic (temp-in-same-dir + `fsync` + set
attributes + `rename`); see §8 and §10 for the cross-file-atomicity trade-off.
`--stdout` is unsupported (a tree has no single byte stream); `--indent` /
`--plist-binary` error as with any non-matching format; `--array-strategy` /
`--sort-keys` are inert (a tree has no arrays, and on-disk order isn't stored).

## 6. Pruning / user-edit preservation (the three-way bit)

A managed key is removed **only** when:

- it was in BASE, **and**
- it is absent from DESIRED, **and**
- TARGET still holds exactly the BASE value (deep-equal).

If the user/app changed the value, it is left intact. With no BASE, nothing is
ever pruned.

## 7. File handling & robustness

- **Missing/unparseable/non-object TARGET** ⇒ treated as `{}` (TARGET becomes a copy of DESIRED, structurally). "Object" here means a JSON object / plist dictionary / YAML mapping / TOML table. (Exception: a non-empty YAML or TOML target that can't be edited safely is **refused**, not overwritten — see §5a. In directory mode a *missing* TARGET is likewise treated as empty, but a TARGET that exists and is **not** a directory is **refused**, not coerced — see §5b.)
- **Missing/empty/unparseable/non-object BASE** ⇒ pruning disabled (first run).
- **Invalid DESIRED** ⇒ hard error (exit 1); TARGET untouched.
- Parent directories of TARGET created as needed.
- Files are parsed as the resolved format (§5a); a TARGET that doesn't parse as that format is treated as empty, as above.

## 8. Atomicity & formatting

- Write to a temp file in TARGET's directory, `fsync`, then `rename(2)` over TARGET (atomic; no torn writes).
- Preserve TARGET's existing file mode; default `0644` for new files.
- Deterministic output: stable key ordering (per `--sort-keys`), fixed indentation, single trailing newline. Running twice with the same inputs is a no-op (idempotent) and `--check`-clean; an unchanged result skips the write entirely.
- **Directory mode (§5b):** each file/symlink is written atomically the same way (a file with its reconciled mode; new dirs `0755`), but the *whole-tree* apply is **not one transaction** — a crash mid-apply leaves a partial (though per-file consistent) tree, which a re-run completes idempotently (§10).

## 9. Non-goals

- Not a general diff/patch tool (use `jd`).
- Not RFC 7386 (no null-deletes) or RFC 6902.
- No comment/formatting preservation for **JSON** (canonical pretty-print) or **plist** (canonical XML); JSONC/JSON5 out of scope. (**YAML** and **TOML** do preserve comments/formatting on untouched regions — see §5a.)
- **No cross-format conversion** (e.g. JSON in / plist out) — a run is homogeneous.
- **YAML:** no editing of anchors/aliases, custom tags, multi-document streams, or non-string keys (refused, not converted); comments are preserved but not relocated when their key moves.
- **TOML:** comments are preserved but not relocated when their key moves; an edit the in-place editor can't make so it round-trips is refused, not forced.
- Does not manage the BASE snapshot lifecycle — the caller stores/rotates it.

## 10. Known trade-offs

- Because removal is snapshot-driven (not null-sentinel), the tool **cannot set a managed key to `null`-meaning-delete**; `null` is a real value. This is intentional and the inverse of RFC 7386's limitation.
- Atomic arrays mean you can't manage a single element of an app-written list; you own the whole array or none of it.
- **Directory mode (§5b):** the multi-file apply is best-effort, not transactional (§8) — re-run to complete a partial apply. An **empty declared directory** is created but is effectively **unprunable** (it has no managed leaf path to diff against, the same way a key whose value is `{}` can't be pruned).

## 11. Examples

```sh
# First apply (no base): TARGET gets DESIRED's keys, app keys preserved.
config-graft config.json desired.json

# Subsequent apply with snapshot: prunes keys we dropped, keeps user edits.
config-graft config.json desired.json .state/last-applied.json

# Dry-run drift check in CI:
config-graft --check config.json desired.json .state/last-applied.json || echo "would change"
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
(union, order-independent, deduped)**; **set idempotent on subset**; **arrays
merge (three-way: prune BASE element dropped from DESIRED, keep unmanaged TARGET
element, append DESIRED insertion, respect user deletion, dedupe, no-BASE ==
set)**; **arrays merge move-aware (preserve a TARGET move, preserve a DESIRED
move, combine move + insert, idempotent under moves, GTS worked example)**;
**merge conflict on contradictory reorder (warns on stderr, names the path, exit
unchanged; clean merge and non-merge strategies don't warn)**; **keyed `merge`
(`--merge-key`: match object-array records by field, merge their fields, keep
app-added, prune dropped, candidate fallthrough, scoped by object key, fall back
to value matching when not all keyed)**; **default array-strategy is `merge`**;
**strategy applies only when both are arrays**;
`null` is a value not a delete;
non-object/missing/invalid TARGET coerced to empty; missing/invalid BASE (no
prune); usage error exit 2; invalid `--array-strategy` exit 2; `--check`
idempotence; `--stdout` leaves TARGET untouched; `--sort-keys`/`--indent`
output; `--diff` add/remove/change lines; file mode preserved. **Directory mode
(§5b):** first-apply creates the tree; app-owned file preserved (inode kept);
prune dropped file on unchanged; keep user-edited file on prune; prune collapses
empty parent; `--no-prune` keeps dropped file; file↔dir type change both ways;
symlink create/update/dangling; symlink-to-dir not followed; refuse special file
(exit 1); non-directory TARGET refused (exit 1); `--check` pending exit 3 (tree
untouched); `--diff` with `/`-joined paths; mode set on create and on mode-only
change; empty declared dir created; re-apply idempotent; **file owner/xattrs
tracked and round-tripped; directory mode reconciled with drift corrected; root
attributes unmanaged by default and managed with `--manage-root`; `--manage-root`
rejected for non-directory formats (exit 1).**

## 13. Implementation

Implemented in **Rust** (this repo):

- `src/value.rs` — the internal value model: a `Leaf` trait and a `Node<L>`
  generic over it. Each format supplies **its own** leaf type (no single enum
  mixing every format's value space), so the encoders are total — a JSON node
  can't hold a plist `Date`, by construction. A `Leaf::MapMeta` associated type
  rides on every `Node::Map` (unit `()` for JSON/plist/YAML/TOML; a directory's
  own attributes for directory mode) and reconciles through the same engine;
  `Node`'s `Clone`/`PartialEq`/`Debug` are hand-written to carry it.
- `src/reconcile.rs` — the pure algorithm (no I/O), generic over `<L: Leaf>`,
  unit-tested against §12. Managed key paths are a `KeyPath` newtype.
- `src/directory.rs` — the `--format directory` backend (§5b): a `DirLeaf` leaf
  that stores each file as a **content handle** (length + SHA-256 digest + source
  path + a generic attribute map of mode/owner/xattrs) or a symlink; a recursive
  `read_tree` (streaming the digest, never buffering the bytes) and a minimal-diff
  `apply_tree` that streams bytes source→dest and applies attributes atomically,
  refusing on failure. A directory's own attributes ride on its map node's
  `MapMeta`; the root is unmanaged unless `--manage-root`. Not a `Format` (a tree
  has no byte stream); `main` dispatches `FormatKind::Directory` to a separate
  `run_directory` that reuses the shared reconcile engine and diff renderer. Uses
  the `sha2` and `xattr` crates.
- `src/format/` — one module per format (`json`/`plist`/`yaml`/`toml`), each
  defining its leaf enum and implementing `ValueCodec` (native ⇄ `Node`) and
  `Format` (parse/serialize). `mod.rs` holds those traits, the `FormatKind`
  selector, `Indent`, and `read_file`. The node type varies per format, so
  dispatch is **static**: `main` resolves the `FormatKind` and monomorphizes
  `run::<F>()`.
- `src/format/yaml_edit.rs` — the comment-preserving YAML writer: a structural
  diff of the original vs reconciled `Node<YamlLeaf>` trees drives minimal
  byte-span edits against the original text (spans from `saphyr`'s `MarkedYaml`),
  with a round-trip backstop that refuses any write that wouldn't reproduce the
  reconciled result.
- `src/format/toml_edit_apply.rs` — the comment-preserving TOML writer: mutates
  `toml_edit`'s format-preserving `DocumentMut` in place (touching only the keys
  that changed), with the same round-trip backstop that refuses any write that
  wouldn't reproduce the reconciled result.
- `src/error.rs` — a typed `Error` enum (format-specific DESIRED errors) and an
  `Outcome`; `src/main.rs` maps these to exit codes.
- `src/main.rs` — CLI (clap), I/O, atomic write, `--check`/`--diff`/`--stdout`/`--format`.
- `tests/{json,plist,yaml,toml}.rs` (+ `tests/common`) — per-format integration
  tests exercising exit codes and file behavior.
- Map nodes use `indexmap` (and the JSON codec keeps `serde_json`'s
  `preserve_order`) so TARGET key order is kept and new keys are appended.

A `jq` + shell implementation of the same core (minus `--check`/`--diff` and
atomic rename) also exists as `sync-json` in the author's nixos-config; this Rust
binary is the hardened generalization.
