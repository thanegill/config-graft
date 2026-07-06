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
  there is no single enum mixing formats. Don't reintroduce one. `Node::Map` carries
  **no side payload** — it's a plain `#[derive]`'d enum. A format that needs
  per-directory metadata (directory mode's mode/owner/xattrs) stores it as an
  ordinary leaf under a reserved empty-string key, so it reconciles through the same
  machinery as any entry (no `Leaf::LeafMeta` associated type, no hand-written
  `Node` impls).
- `src/reconcile.rs` — the pure three-way merge, generic `<L: Leaf>`, no I/O.
  Managed key paths are the `KeyPath` newtype. A directory's own attributes are
  just a leaf under the reserved key, so they prune/merge like any entry with no
  special engine path (DESIRED wins on conflict, as for any leaf).
- `src/format/{json,plist,yaml,toml}.rs` — per-format leaf enum + `ValueCodec`
  (native ⇄ `Node`) + `Format` (parse/serialize). `mod.rs` holds the traits,
  `FormatKind`, `Indent`, `read_file`.
- `src/backend.rs` — the `Backend` trait is the I/O boundary of a run (read the
  three inputs → reconcile → diff/check/stdout/apply); one generic `run::<B>`
  driver owns that spine so **every format shares it**. Dispatch is **static**:
  `main` matches the `FormatKind` and monomorphizes `run::<ByteBackend<F>>` for the
  byte formats or `run::<Directory>` for the tree. `ByteBackend<F>(PhantomData<F>)`
  is a newtype over any `Format` (a blanket `impl<F: Format> Backend for F` would
  collide with `Directory` under coherence). `dir_policy(cli)` builds the
  `AttrPolicy` (`--no-owner`/`--xattrs`).
- `src/format/directory.rs` — the `--format directory` tree backend, living beside
  the format codecs but **not a `Format`** (a tree has no byte stream); it plugs
  into the shared `run` via `impl Backend for Directory`. A `DirLeaf` file is a
  **content handle** (len + SHA-256 digest + source path + generic `attrs` map of
  mode/owner/xattrs), so bytes never enter the tree — `read_tree` streams the
  digest, `apply_tree` streams source→dest and applies attrs atomically
  (refuse-on-failure). An `AttrPolicy { owner, xattrs }` (default: manage
  everything) threads through read/apply; `XattrScope::in_scope` filters xattrs.
  A directory's own attrs are a `DirLeaf::DirAttributes` leaf under the reserved
  empty-string key (`DIR_ATTRS_KEY`) in its map; `apply_dir` extracts it and
  applies it to the directory (never as a file), and every entry-iterating site
  (`apply_dir`, `remove_node`) skips that key. The root is unmanaged unless
  `--manage-root`. Robustness: case-fold sibling-collision refuse, `MAX_DEPTH`
  guard, `.config-graft-tmp.` temp-name skip on read, per-entry parent `fsync`. Uses
  `sha2` + `xattr`.
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
  are set on the temp file *before* the rename (order: remove out-of-desired
  in-scope xattrs, set desired xattrs, chown, chmod — chown clears setuid), so any
  attribute failure refuses cleanly. `chown` to the caller's own uid/gid is
  skipped (no-op, avoids needless privilege). Reading xattrs is best-effort
  (unsupported FS ⇒ none); applying is strict but scoped by `AttrPolicy`. Because
  a directory's attrs are a reserved-key leaf, an **empty declared directory is
  prunable** (it has a managed leaf path), unlike a JSON `{}` value.
- Directory mode is intentionally non-transactional across files, doesn't preserve
  hardlinks, and trusts ancestor path components (not the entries it walks). These
  trade-offs are locked by characterization tests (`hardlink_is_broken_on_rewrite`,
  `eacces_mid_walk_refuses_whole_run`) and documented in SPEC §10 — don't "fix"
  them silently.
- Refactors here are expected to be behavior-preserving — the test suite (unit in
  `src/`, integration in `tests/{json,plist,yaml,toml,directory}.rs` + `tests/common`)
  is the gate.
