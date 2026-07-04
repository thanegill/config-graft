# config-graft

Three-way reconcile (kubectl-apply-style) for app-owned **JSON / plist / YAML /
TOML** config files, plus a **directory-tree** mode (`--format directory`).
`SPEC.md` is the source of truth for merge semantics; this file is orientation for
working on the code.

## Commands (via the Nix dev shell)

```bash
nix develop --command cargo test     # unit (src) + per-format integration (tests/)
nix develop --command cargo clippy
nix develop --command cargo fmt -- --check
nix build                            # hermetic; runs the test suite in checkPhase
```

## Architecture

A format-agnostic engine over a generic value model; formats plug in via traits.

- `src/value.rs` — `Node<L>` + the `Leaf` trait. **Each format owns its leaf type**
  (`JsonLeaf`/`YamlLeaf`/`PlistLeaf`/`TomlLeaf`/`DirLeaf`), so encoders are total —
  there is no single enum mixing formats. Don't reintroduce one. `Node::Map` also
  carries a `Leaf::MapMeta` payload (`()` for JSON/plist/YAML/TOML; a directory's
  own attributes for `DirLeaf`), reconciled through the same engine; `Node`'s
  `Clone`/`PartialEq`/`Debug` are **hand-written** because `derive` won't add the
  `L::MapMeta: Trait` bounds the `Map` field needs.
- `src/reconcile.rs` — the pure three-way merge, generic `<L: Leaf>`, no I/O.
  Managed key paths are the `KeyPath` newtype. `deep_merge` takes DESIRED's map
  metadata on a merge (the same "DESIRED wins" rule as a leaf).
- `src/format/{json,plist,yaml,toml}.rs` — per-format leaf enum + `ValueCodec`
  (native ⇄ `Node`) + `Format` (parse/serialize). `mod.rs` holds the traits,
  `FormatKind`, `Indent`, `read_file`. Dispatch is **static**: `main` matches the
  `FormatKind` and monomorphizes `run::<F>()` (no `&dyn Format`).
- `src/format/directory.rs` — the `--format directory` backend, living beside the
  format codecs but **not a `Format`** (a
  tree has no byte stream): `main` dispatches `FormatKind::Directory` to a separate
  `run_directory` that reuses the shared reconcile engine + diff renderer. A
  `DirLeaf` file is a **content handle** (len + SHA-256 digest + source path +
  generic `attrs` map of mode/owner/xattrs), so bytes never enter the tree —
  `read_tree` streams the digest, `apply_tree` streams source→dest and applies
  attrs atomically (refuse-on-failure). A directory's own attrs ride on its
  `MapMeta`; the root is unmanaged unless `--manage-root`. Uses `sha2` + `xattr`.
- `src/format/yaml_edit.rs` / `toml_edit_apply.rs` — YAML/TOML writes **edit the
  original document in place** to preserve comments, with a round-trip backstop
  that **refuses rather than corrupt** on anything they can't safely edit.
  Empty/first-apply targets are emitted canonically. YAML splices byte spans
  (saphyr `MarkedYaml`); TOML mutates `toml_edit`'s format-preserving `DocumentMut`.
- `src/error.rs` — typed `Error` (format-specific) + `Outcome`; `main` maps to
  exit codes: `0` ok, `1` runtime error, `2` usage (clap), `3` `--check` pending.

## Gotchas

- `--diff` output comes from `Leaf::render()`; keep it byte-stable (tests assert it).
- saphyr is pre-1.0 (0.0.x), YAML 1.2; `Cargo.lock` is pinned for `nix build`.
- TOML root is always a table, so `Toml::parse` → map is total and `NotTomlTable`
  is structurally unreachable (kept only for `FormatKind` symmetry).
- Directory mode: `DirLeaf::File` equality is `(len, digest, attrs)` and ignores
  the source path — that path-independence is what keeps re-apply a no-op. Attrs
  are set on the temp file *before* the rename (order: xattrs, chown, chmod —
  chown clears setuid), so any attribute failure refuses cleanly. Reading xattrs
  is best-effort (unsupported FS ⇒ none); applying is strict. Adding a new leaf
  type means adding its `MapMeta` (`()` unless the format has map metadata).
- Refactors here are expected to be behavior-preserving — the test suite (unit in
  `src/`, integration in `tests/{json,plist,yaml,toml,directory}.rs` + `tests/common`)
  is the gate.
