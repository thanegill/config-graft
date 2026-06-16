# config-graft

Three-way reconcile (kubectl-apply-style) for app-owned **JSON / plist / YAML**
config files. `SPEC.md` is the source of truth for merge semantics; this file is
orientation for working on the code.

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
  (`JsonLeaf`/`YamlLeaf`/`PlistLeaf`), so encoders are total — there is no single
  enum mixing formats. Don't reintroduce one.
- `src/reconcile.rs` — the pure three-way merge, generic `<L: Leaf>`, no I/O.
  Managed key paths are the `KeyPath` newtype.
- `src/format/{json,plist,yaml}.rs` — per-format leaf enum + `ValueCodec`
  (native ⇄ `Node`) + `Format` (parse/serialize). `mod.rs` holds the traits,
  `FormatKind`, `Indent`, `read_file`. Dispatch is **static**: `main` matches the
  `FormatKind` and monomorphizes `run::<F>()` (no `&dyn Format`).
- `src/format/yaml_edit.rs` — YAML writes **edit the original text in place** to
  preserve comments (byte-span splicing via saphyr `MarkedYaml`), with a
  round-trip backstop that **refuses rather than corrupt** on anything it can't
  safely edit. Empty/first-apply targets are emitted canonically.
- `src/error.rs` — typed `Error` (format-specific) + `Outcome`; `main` maps to
  exit codes: `0` ok, `1` runtime error, `2` usage (clap), `3` `--check` pending.

## Gotchas

- `--diff` output comes from `Leaf::render()`; keep it byte-stable (tests assert it).
- saphyr is pre-1.0 (0.0.x), YAML 1.2; `Cargo.lock` is pinned for `nix build`.
- Refactors here are expected to be behavior-preserving — the test suite (unit in
  `src/`, integration in `tests/{json,plist,yaml}.rs` + `tests/common`) is the gate.
