# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **TOML** support alongside JSON, plist, and YAML. The format is inferred from
  TARGET's extension (`.toml`) or forced with `--format toml`. Like YAML, an
  existing TOML target is edited in place (via `toml_edit`) so **comments, blank
  lines, and formatting are preserved** on the parts that don't change; only an
  empty/first-apply target is written canonically. Every write is verified to
  round-trip back to the intended result, refusing (and leaving the file
  untouched) rather than risk corruption. Date-times round-trip as atomic leaves.
- **Move-aware three-way array merge** via `--array-strategy merge`: reconciles
  array element membership against BASE (prune a BASE element dropped from
  DESIRED, keep insertions on either side, respect a user's deletion) and orders
  the survivors so a reordering on either side is preserved, breaking a
  contradictory cross-over move deterministically (generalized topological sort).
  Well-defined for arrays of uniquely-valued elements; use `replace` for arrays of
  structurally anonymous objects.
- **Conflict warnings** for `merge`: when TARGET and DESIRED reorder the same
  elements contradictorily, the resolved-but-arbitrary reorder is reported on
  stderr, naming the array's path and the elements involved. Diagnostic only — the
  result stays deterministic (TARGET order preferred) and the exit code is
  unchanged.
- **Keyed matching** for `merge` via `--merge-key`: identify object-array elements
  by a field so keyed records are merged in place (their fields reconciled
  three-way) instead of matched by whole value — a managed field can change while
  an app-added field survives, with no duplicate entry. `FIELD` (or `f1,f2`
  candidates) applies to any object-array; `KEY=FIELD` scopes it to arrays under
  object key `KEY`. Falls back to value matching when a key doesn't resolve or not
  every element carries it; arrays of anonymous objects stay atomic (`replace`).

### Changed

- **Default `--array-strategy` is now `merge`** (was `replace`): arrays are
  reconciled element-wise against BASE by default. Use `replace` to own a list
  wholesale, or for arrays of structurally anonymous objects that `merge` can't
  match by value.
- Diagnostic key-paths (`--diff` output and `merge` conflict warnings) use the
  format's separator: `.` for JSON/YAML/TOML, `:` for plist (PlistBuddy).

## [0.0.3] - 2026-06-16

### Added

- **YAML** support alongside JSON and plist. The format is inferred from TARGET's
  extension (`.yaml`/`.yml`) or forced with `--format yaml`. Unlike JSON/plist,
  an existing YAML target is edited in place so **comments, blank lines, and
  formatting are preserved** on the parts that don't change. Constructs that can't
  be edited safely — anchors/aliases, tags, multi-document streams, non-string
  keys — are refused (the file is left untouched) rather than risk corruption.
- `--plist-binary` to write binary plist output instead of XML (plist reads
  already accept either).

### Changed

- **Renamed the project to `config-graft`** (was `json-apply`) to reflect that
  the merge engine is multi-format (JSON, plist, YAML), not JSON-specific. The
  crate, binary, and command are all now `config-graft`.
- DESIRED parse/shape errors now name the specific format (e.g. "DESIRED must be
  a YAML mapping") instead of a generic message.
- An invalid `--indent` value is now a usage error (exit 2) rather than a runtime
  error (exit 1).
- Format-specific flags (`--indent`, `--plist-binary`) now error and exit when
  passed with a format they don't apply to, instead of being silently ignored.

## [0.0.2] - 2026-06-15

### Added

- Apple **plist** support alongside JSON. The merge engine is format-agnostic;
  the format is inferred from TARGET's extension (`.plist` → plist, else JSON)
  and can be forced with `--format json|plist`. Reads accept both XML and binary
  plist; output is normalized XML. `Date`/`Data`/`Uid` scalars round-trip as
  atomic leaves.

### Changed

- Reworked the engine onto an internal Node value model so the reconcile logic
  is shared across formats.

## [0.0.1] - 2026-06-12

### Added

- Initial release: three-way reconcile for app-owned JSON files
  (`json-apply <TARGET> <DESIRED> [BASE]`), deep-merging a managed DESIRED subset
  into TARGET while preserving unmanaged keys and pruning dropped keys via a BASE
  ancestor.
- Array strategies: atomic by default, with `--array-strategy concat|set`.
- `--check`, `--stdout`, and `--diff` modes; `--indent` control.
- An empty BASE argument is treated as no base.

[Unreleased]: https://github.com/thanegill/config-graft/compare/v0.0.3...HEAD
[0.0.3]: https://github.com/thanegill/config-graft/compare/v0.0.2...v0.0.3
[0.0.2]: https://github.com/thanegill/config-graft/compare/v0.0.1...v0.0.2
[0.0.1]: https://github.com/thanegill/config-graft/releases/tag/v0.0.1
