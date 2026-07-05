# config-graft

Three-way reconcile (kubectl-apply-style) for app-owned **JSON / plist / YAML /
TOML** config files. `SPEC.md` is the source of truth for merge semantics; this
file is orientation for working on the code.

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
  (`JsonLeaf`/`YamlLeaf`/`PlistLeaf`/`TomlLeaf`), so encoders are total — there is
  no single enum mixing formats. Don't reintroduce one.
- `src/reconcile.rs` — the pure three-way merge, generic `<L: Leaf>`, no I/O.
  Managed key paths are the `KeyPath` newtype.
- `src/format/{json,plist,yaml,toml}.rs` — per-format leaf enum + `ValueCodec`
  (native ⇄ `Node`) + `Format` (parse/serialize). `mod.rs` holds the traits,
  `FormatKind`, `Indent`, `read_file`. Dispatch is **static**: `main` matches the
  `FormatKind` and monomorphizes `run::<F>()` (no `&dyn Format`).
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
  `darwin.nix` (`environment.managed*`). Each is a thin normal
  `{ config, lib, pkgs, ... }:` module that supplies a *platform* record and calls
  `shared.nix`'s `build`; there is **no dispatch on module type**. `shared.nix`
  holds the static `specs` list, the format option/DESIRED helpers + `build` (used
  by all three), and `systemPlatform` (shared by `nixos.nix`/`darwin.nix`, which
  differ only in `wireActivation`). The home-manager platform has a single
  consumer, so it's defined inline in `home-manager.nix`. Each entry's DESIRED
  comes from `settings` (a `pkgs.formats` generator, overridable per entry via
  `format`), or a pre-built `source` file; the two are mutually exclusive (asserted);
  `target` defaults to the attribute name (entries keyed by path); `package`
  defaults to the flake's own build (threaded in via `self`), so no overlay is
  needed. `cfprefsdDomain` (plist only) is a shared option on both home and system,
  but `build` emits a build-time assertion that it's only set on a Darwin host.
  Two recursion traps
  the module system punishes via `_module.freeformType`, both avoided by
  construction: (1) the platform is chosen by the *file*, never `pkgs.stdenv`, so
  config *keys* never depend on `pkgs`; (2) every platform config fragment uses a
  **static top-level key** whose value aggregates over `active` (e.g.
  `home.file = listToAttrs … entries`), so the `mkIf` body shape is fixed and
  `active` (hence `config`) isn't forced while keys are determined — never
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
- Refactors here are expected to be behavior-preserving — the test suite (unit in
  `src/`, integration in `tests/{json,plist,yaml,toml}.rs` + `tests/common`) is the gate.
