# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
