# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

[Unreleased]: https://github.com/thanegill/json-apply/compare/v0.0.2...HEAD
[0.0.2]: https://github.com/thanegill/json-apply/compare/v0.0.1...v0.0.2
[0.0.1]: https://github.com/thanegill/json-apply/releases/tag/v0.0.1
