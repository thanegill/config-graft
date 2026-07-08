# config-graft

Three-way reconcile (kubectl-apply-style) for app-owned **JSON / plist / YAML /
TOML** config files, plus a **directory-tree** mode (the `directory` subcommand).
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

- `src/value.rs` — `Node<L>` + the `Leaf` trait (bound `Clone + Eq + Hash + Debug`;
  `Eq`/`Hash` let array set-union and the GTS internals dedup via `HashSet` rather than
  linear scans, so every leaf keeps `Hash` consistent with `Eq` — the float-carrying
  leaves canonicalize `-0.0`/`NaN` for that). **Each format owns its leaf type**
  (`JsonLeaf`/`YamlLeaf`/`PlistLeaf`/`TomlLeaf`/`FsLeaf`), so encoders are total —
  there is no single enum mixing formats. Don't reintroduce one. `Node::Map` carries
  **no side payload**: a format that needs per-directory metadata (directory mode's
  mode/owner/xattrs) stores it as an ordinary leaf under a reserved empty-string key,
  so it reconciles through the same machinery as any entry (no `Leaf::LeafMeta`
  associated type, no directory-specific `Node` impls). `Node` derives everything
  except `Hash`, which is hand-written so a `Map` hashes order-independently (matching
  `IndexMap`'s order-independent `PartialEq`). `Leaf::is_dir_attrs()` lets `--diff`
  tell a directory's own-attributes leaf from a real empty-string key.
- `src/reconcile.rs` — the pure three-way merge, generic `<L: Leaf>`, no I/O.
  Managed key paths are the `KeyPath` newtype. A directory's own attributes are
  just a leaf under the reserved key, so they prune/merge like any entry with no
  special engine path (DESIRED wins on conflict, as for any leaf).
- `src/format/{json,plist,yaml,toml}.rs` — per-format leaf enum + `ValueCodec`
  (native ⇄ `Node`) + `Format` (parse/serialize). `mod.rs` holds the traits,
  `FormatKind` (a per-format tag, no longer the dispatch selector), `Indent`,
  `read_file`.
- `src/backend.rs` — the `Backend` trait is the I/O boundary of a run (read the
  three inputs → reconcile → diff/check/stdout/apply); its provided `Backend::run`
  method owns that spine so **every format shares it**. Dispatch is **static**:
  `main` matches the clap subcommand, builds a `RunArgs` (the internal options
  struct the run reads) from that subcommand's flags, and calls
  `ByteBackend::<F>::run(&args)` for the byte formats or `Directory::run(&args)`
  for the tree. Because each subcommand exposes only its own flags, an unsupported
  flag/format pairing is a clap usage error (exit 2), not a runtime check -- there
  is no `check_cli_args`/`IncompatibleFlag`/`StdoutUnsupportedForDirectory`, and no
  extension inference (no `FormatKind::detect`). `ByteBackend<F>(PhantomData<F>)`
  is a newtype over any `Format` (a blanket `impl<F: Format> Backend for F` would
  collide with `Directory` under coherence). `RunArgs::dir_policy()` builds the
  `AttrPolicy` (`--no-owner`/`--xattrs`).
- `src/format/directory.rs` — the `directory` subcommand tree backend, living beside
  the format codecs but **not a `Format`** (a tree has no byte stream); it plugs
  into the shared `run` via `impl Backend for Directory`. A `FsLeaf` file is a
  **content handle** (len + SHA-256 digest + source path + generic `attrs` map of
  mode/owner/xattrs), so bytes never enter the tree — `read_tree` streams the
  digest, `apply_tree` streams source→dest and applies attrs atomically
  (refuse-on-failure). An `AttrPolicy { owner, xattrs }` (default: manage
  everything) threads through read/apply; `XattrScope::in_scope` filters xattrs.
  A directory's own attrs are a `FsLeaf::DirectoryAttributes` leaf under the reserved
  empty-string key (`DIR_ATTRS_KEY`) in its map — that's the *reconcile* shape
  (read/diff/prune treat it as a normal leaf). The *write* path never touches that
  key: `apply_tree` calls `parse` once at the boundary to lift each directory's
  attrs into a type-guarded `FsTree { Dir { attrs, entries }, File, Symlink }`, so
  `apply_dir`/`apply_node`/`remove_node` build paths only from real entries and
  **cannot** express attrs-as-an-entry (illegal states unrepresentable). `parse` is
  the single place the reserved key is interpreted. The root is unmanaged unless
  `--manage-root`. Robustness: case-fold sibling-collision refuse, `MAX_DEPTH`
  guard, `.config-graft-tmp.` temp-name skip on read, per-directory (once, after its
  entries settle) parent `fsync`. Uses `sha2` + `xattr`.
- `src/format/yaml_edit.rs` / `toml_edit_apply.rs` — YAML/TOML writes **edit the
  original document in place** to preserve comments, with a round-trip backstop
  that **refuses rather than corrupt** on anything they can't safely edit.
  Empty/first-apply targets are emitted canonically. YAML splices byte spans
  (saphyr `MarkedYaml`); TOML mutates `toml_edit`'s format-preserving `DocumentMut`.
- `src/error.rs` — typed `Error` (format-specific) + `Outcome`; `main` maps to
  exit codes: `0` ok, `1` runtime error, `2` usage (clap), `3` `--check` pending.
- `modules/` — the Nix wrappers exposed by the flake (`homeManagerModules` +
  `nixos`/`darwinModules` + `overlays.default`). **Three separate module files**,
  one per platform: `home-manager.nix` (`home.managed*`), `nixos.nix` and
  `darwin.nix` (`environment.managed*`). There is **no generic engine and no
  per-platform dispatch record**: each module writes its own `options`/`config`
  linearly (loop the `formats`, declare the `attrsOf` submodule, `mkIf (active !=
  {})` the snapshot + activation + assertions) and pulls the format-agnostic pieces
  from `modules/lib/`. `home-manager.nix` is fully linear (its activation script
  inline). `nixos.nix`/`darwin.nix` are thin wrappers over `lib/system.nix` (the
  shared linear system module), passing only their `activationWiring` (NixOS: a
  named `system.activationScripts.<name>`; darwin: appended to `postActivation`).
  `lib/` holds `formats.nix` (the static per-format descriptors), `common.nix`
  (`entryType` for the entry submodule, `mkDesired` for the DESIRED store path,
  `mkAssertions`; re-exported as `./lib` via `default.nix`), `system.nix`, and
  `cfprefsd.nix` (the `cfprefsdDomain` option, used by `common.nix`). Each entry's
  DESIRED comes from `settings` (a `pkgs.formats` generator, overridable per entry
  via `format`), or a pre-built `source` file; the two are mutually exclusive
  (asserted); `target` defaults to the attribute name (entries keyed by path);
  `package` defaults to the flake's own build (threaded in via `self`), so no
  overlay is needed. **`managedDirectory`** (the `directory` subcommand) lives *beside*
  the `formats` loop, not in it (a tree has no `settings`/generator): `common.nix`
  provides `directoryEntryType` (its `source` is a required directory, plus
  `manageRoot`/`noOwner`/`xattrs`) and `mkDirectoryReconcileScript`; each module
  wires it as a parallel track (its own snapshot + activation entry). `cfprefsdDomain`
  (plist only) is offered on both home and
  system, guarded by a build-time assertion (`mkAssertions`) that it's only set on a
  Darwin host. Each module also asserts a managed target isn't also declared in
  `home.file` / `environment.etc` (those create an immutable store symlink;
  config-graft edits a mutable file in place, so a path can't be both). The
  recursion trap the module system punishes via
  `_module.freeformType` is avoided by construction: (1) the module is chosen by the
  *file*, never `pkgs.stdenv`, so config *keys* never depend on `pkgs`; (2) every
  config fragment uses a **static top-level key** whose value aggregates over
  `active` (e.g. `home.file = mapAttrs' … entries`), so the `mkIf` body shape is
  fixed and `active` (hence `config`) isn't forced while keys are determined — never
  `mkMerge (mapAttrsToList … active)` at the top. Pruning uses the previous
  generation as BASE: HM via `$oldGenPath/home-files/<snap>`, system via
  `/run/current-system/<snap>` embedded with `system.systemBuilderCommands`.
  `examples/{home-manager,nixos,nix-darwin}/` are self-contained consumer flakes
  (input `github:thanegill/config-graft`; override with `--override-input
  config-graft path:.` to test a local checkout) that each evaluate to a full
  `toplevel`.

## Gotchas

- `--diff` output comes from `Leaf::render()`; keep it byte-stable (tests assert it).
- saphyr is pre-1.0 (0.0.x), YAML 1.2; `Cargo.lock` is pinned for `nix build`.
- TOML root is always a table, so `Toml::parse` → map is total and `NotTomlTable`
  is structurally unreachable (kept only for `FormatKind` symmetry).
- Directory mode: `FsLeaf::File` equality is `(len, digest, attrs)` and ignores
  the source path — that path-independence is what keeps re-apply a no-op. Attributes
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
