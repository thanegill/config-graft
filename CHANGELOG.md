# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Man pages:** the Nix package installs config-graft(1) and one page per subcommand, generated from the `--help` text by `cargo xtask man`.
- **Warnings on stderr whenever a run changes a value beyond the managed edits**, naming the key path: an input array holding the same value (or `--merge-key` key) twice where a copy is dropped ([#49](https://github.com/thanegill/config-graft/issues/49)), and a plist `Date` floored to a whole second for XML output. They never change the exit code.
- **plist:** an XML write refuses (exit 1) to introduce a string or key holding a character XML can't carry, such as the ESC `0x1B` in `NSUserKeyEquivalents`. A value an XML target already holds is written through with a warning.
- **JSON is parsed and printed by `json-syntax`** instead of `serde_json`, which removes the `$serde_json::private::Number` magic key that silently turned an app's object into a number. `--diff` output is unchanged.
- **`managedPlist`:** a build-time assertion rejects `settings` holding an XML-illegal character while `binary` is false.

### Changed

- **`json --help` shows `--indent`'s default as `[default: 2]`**, as `--array-strategy` and `--plist-format` already do.
- **`--help` reads as a manual:** the root help explains TARGET, DESIRED and BASE and lists the exit codes, and each subcommand describes how its format is written.
- **Building from source requires Rust 1.93 or later**, now declared as `rust-version` in Cargo.toml.
- **directory:** `--diff` shows each xattr as `name=` plus the first 8 hex digits of its value's SHA-256, which is stable across Rust releases. Every xattr digest in existing output changes once.
- **Breaking:** `--base PATH` together with a positional BASE is a usage error (exit 2), even if one is empty; the flag used to win silently.
- **Breaking:** plist output keeps the target's encoding. `--plist-binary` is replaced by `--plist-format follow|xml|binary` (default `follow`); `--plist-format binary` is the old flag, `--plist-format xml` the old default, and `--plist-binary` is gone (exit 2).
- **`managedPlist`:** a file entry at its defaults no longer rewrites a binary target as XML ([#43](https://github.com/thanegill/config-graft/issues/43)), so an ESC the app wrote no longer fails activation and sub-second dates aren't floored.
- **`managedPlist`:** the cfprefsd path always writes binary, and `binary` defaults to `true` for a `cfprefsdDomain` entry.
- **`managedPlist`:** a binary DESIRED is built with the entry's own `format` generator and `plistutil`, so a `format` override now applies on the binary path too.
- **A managed value equal to TARGET's is left as TARGET spells it**, so a JSON `2.50` no longer rewrites `2.5` while `--diff` shows nothing.

### Fixed

- **TOML:** a CRLF file keeps its line endings instead of being rewritten entirely to LF, even by a run that changed nothing (where `--check` also exited 3); a file that mixes endings takes the one most of its lines have.
- **YAML:** the refusal for an unsafe edit no longer says anchors and aliases are unsupported; it names an edit that would change what an alias resolves to, and so does `yaml --help`.
- **YAML:** a key added after an alias to a mapping or sequence (`b: *x`, `<<: *base`) is written after it instead of above earlier keys or refused, and such an alias can now be pruned or replaced.
- **YAML:** a key added to a mapping whose first entry is an explicit `? key` or an anchored or tagged key is written instead of refused; a comment indented under such a key is kept above a key added after it, and removed with it when it is pruned.
- **YAML:** adding keys to a nested mapping that is the last entry of its parent, and to the parent in the same run, is written instead of refused.
- **YAML:** a file whose lines mostly end in CRLF keeps that ending on the lines config-graft adds or re-renders, instead of mixing in bare LF lines.
- **A refused file says why** instead of always "is not valid <format>": the parser's message with its line and column, the 512-container depth cap, or what valid YAML holds that can't be reconciled; control and invisible characters from the file are escaped.
- **An object DESIRED declares empty survives the prune that emptied it**, instead of being deleted and re-added on the next run. YAML writes it as `key: {}`, and TOML turns an emptied dotted or implicit table into an explicit `[a]` header.
- **An object the app had already emptied survives a prune that removed nothing from it**, instead of being deleted because it was empty. Directory mode is unchanged: a directory the app emptied is still removed once DESIRED drops it, while its attributes match BASE.
- **YAML:** a DESIRED key under an existing `key: {}` is written instead of refused; a comment on its key line is dropped.
- **A declared `-0.0` replaces an existing `0.0`** ([#40](https://github.com/thanegill/config-graft/issues/40)): the sign of a zero is part of its value in every format, so an array keeps both and a `-0.0` the app rewrote as `0.0` is no longer pruned.
- **YAML:** a file nested past 512 containers is refused (exit 1) instead of crashing with a stack overflow; a deep BASE disables pruning.
- **A key holding `[` or opening with `"` is quoted in warnings and refusals** (`x."[b]"`), so it can't be mistaken for a `--merge-key` selector; directory `--diff` keeps the `/` before a name starting with `[`.
- **YAML:** a file of only comments, or a bare `---`, is an empty mapping instead of an error, and its text is kept.
- **YAML:** pruning or replacing a nested collection or block scalar that has a key after it, or adding a key after one, works and keeps the surrounding comments; it used to refuse, delete comments, or panic.
- **A privileged rewrite keeps TARGET's owner and group** instead of handing the file to root; a failed chown refuses the write.
- **YAML:** a file holding non-ASCII text can be edited in place; it used to be refused or panic.
- **A non-UTF-8 BASE path is accepted**, like TARGET and DESIRED.
- **YAML:** replacing a scalar with a collection, or back, under an explicit `? key` or a tagged or anchored key is written instead of refused, as a plain `key:` at the line's indent.
- **YAML writes floats as floats** (`1.0`, `-0.0`, `.inf`, `.nan`); `1.0` used to become the integer `1` and infinity a string.
- **An XML plist with comments before `<?xml` is recognized as XML** ([#44](https://github.com/thanegill/config-graft/issues/44)), so values the app wrote pass through.
- **The plist refusal says whether the target was binary or not recognized as XML**, since only one is fixed by dropping `--plist-format xml`.
- **A deeply nested JSON or plist file is refused instead of crashing**; the depth limit rises from 128 to 512 containers.
- **A TARGET that exists but can't be read is refused (exit 1)** instead of being treated as empty and overwritten with DESIRED.
- **plist:** an XML run floors dates to whole seconds, so `plutil` and `defaults import` accept the file; a binary write keeps full precision.
- **JSON numbers keep their source spelling** and compare by value, so high-precision decimals, big integers and exponents like `1e400` round-trip; `1e400` used to replace the whole file.

## [0.1.1] - 2026-07-07

### Added

- `managedPlist` entries gain a plist-only **`binary`** option (default `false`). With `binary = true` the Nix module generates a **binary** plist DESIRED — via `libplist`'s `plistutil` at build time — and reconciles with `--plist-binary`, so preference values XML cannot represent (a string containing a byte illegal in XML 1.0, e.g. the ESC `0x1B` separators in `NSUserKeyEquivalents`) round-trip instead of corrupting the DESIRED. Reconciling such a domain through `cfprefsdDomain` merges the dict in place rather than clobbering it like `defaults write -dict`. `plistutil` is a build-time dependency of the module only, pulled in only for entries that opt in; the config-graft engine already read and wrote binary plist.

## [0.1.0] - 2026-07-07

### Added

- **Directory mode** (the `directory` subcommand): reconcile a whole directory *tree* instead of a single file — TARGET, DESIRED, and BASE are directories. A directory is a map and a file/symlink is an atomic leaf, so the same three-way merge preserves app-created files and prunes files you stop declaring (keeping user edits). Writes are **minimal and in place** — unchanged files keep their inode and mtime, each file write is atomic, and file contents stream source→destination (never buffered in memory) with a SHA-256 content digest for identity. A file's — and a directory's — **mode, owner (uid/gid), and extended attributes** are tracked and reconciled; an attribute that can't be applied (e.g. no privilege to `chown`) refuses the run rather than leaving a half-applied entry. Symlinks are managed by target and never followed; FIFOs/sockets/devices and non-UTF-8 filenames are refused. The root directory's own attributes are left untouched unless `--manage-root` is given. Opt-in: `directory` is never inferred from a path. Metadata drift (mode/owner/xattr, on files and directories) shows up in `--diff`, not just content changes. Replacing an app-populated directory with a file is refused rather than deleting content never under management; sibling names that collide when case-folded, and trees nested past an internal depth limit, are refused; leftover `.config-graft-tmp.` entries from an interrupted apply are ignored on read. Each directory is fsync'd once after its entry changes settle, so the renames are crash-durable.
- `--manage-root` (directory mode) to also reconcile the TARGET directory's own mode/owner/xattrs, not just its contents.
- `--no-owner` and `--xattrs <all|safe|none>` (directory mode) to narrow the metadata reconcile scope. The default manages everything (owner + all xattrs); `--no-owner` leaves uid/gid alone, `--xattrs safe` skips privileged/system namespaces, `--xattrs none` ignores extended attributes. In-scope xattrs on the target but absent from DESIRED are removed (they converge); a `chown` to the caller's own uid/gid is skipped so the common case needs no privilege.
- **TOML** support alongside JSON, plist, and YAML. Selected with the `toml` subcommand. Like YAML, an existing TOML target is edited in place (via `toml_edit`) so **comments, blank lines, and formatting are preserved** on the parts that don't change; only an empty/first-apply target is written canonically. Every write is verified to round-trip back to the intended result, refusing (and leaving the file untouched) rather than risk corruption. Date-times round-trip as atomic leaves.
- **Move-aware three-way array merge** via `--array-strategy merge`: reconciles array element membership against BASE (prune a BASE element dropped from DESIRED, keep insertions on either side, respect a user's deletion) and orders the survivors so a reordering on either side is preserved, breaking a contradictory cross-over move deterministically (generalized topological sort). Well-defined for arrays of uniquely-valued elements; use `replace` for arrays of structurally anonymous objects.
- **Conflict warnings** for `merge`: when TARGET and DESIRED reorder the same elements contradictorily, the resolved-but-arbitrary reorder is reported on stderr, naming the array's path and the elements involved. Diagnostic only — the result stays deterministic (TARGET order preferred) and the exit code is unchanged.
- **Keyed matching** for `merge` via `--merge-key`: identify object-array elements by a field so keyed records are merged in place (their fields reconciled three-way) instead of matched by whole value — a managed field can change while an app-added field survives, with no duplicate entry. `FIELD` (or `f1,f2` candidates) applies to any object-array; `PATH=FIELD` scopes it to the array at `PATH` — its full path from the document root, segments joined by the format separator (`.`, or `:` for plist) — so same-named arrays at different depths (`spec.containers` vs `spec.template.containers`) can take different rules. Falls back to value matching when a key doesn't resolve or not every element carries it; arrays of anonymous objects stay atomic (`replace`). A contradictory reorder *inside* a matched record (one of its nested arrays) is surfaced too, its warning locating it with a `[field=value]` element selector on the array key — `servers[name="web"].tags` (`servers[name="web"]:tags` for plist).
- **Nix flake modules** for declarative config management on NixOS, nix-darwin, and home-manager: `homeManagerModules.default` (`home.managed*`), `nixosModules.default` / `darwinModules.default` (`environment.managed*`), and an optional `overlays.default`. Each entry reconciles `settings` (or a pre-built `source` file) into the live file on activation, pruning keys dropped from Nix against the previous generation. Freeform entries take a `format` override; plist entries take `cfprefsdDomain` (macOS). Runs config-graft by store path, defaulting to this flake's own build; override it per entry or module-wide via `managed.package`. See `examples/` for a full flake per platform.
- `managedDirectory` (all three platforms) for the `directory` subcommand: reconcile a whole `source` *tree* into a target directory — files the app adds are kept, files dropped from `source` are pruned, and per-file mode/owner/xattrs are reconciled. Directory-specific options `manageRoot`, `noOwner` (set it on a non-root home-manager activation, since a store-built source is root-owned), and `xattrs` (`all`/`safe`/`none`) map to the CLI flags.

### Changed

- **BREAKING: the format is now a required per-format subcommand**, replacing the `--format` flag and extension inference. Invoke `config-graft json|yaml|toml|plist|directory TARGET DESIRED [BASE] [flags]`; the format is no longer guessed from TARGET's extension, and there is no `--format` flag. Each subcommand exposes only the flags that apply to it (JSON `--indent`, plist `--plist-binary`, the byte formats `--stdout`/`--sort-keys`/`--array-strategy`/`--merge-key`, directory `--manage-root`/`--no-owner`/`--xattrs`), so an unsupported flag/format pairing is now a CLI usage error (exit 2) enforced by the parser rather than a runtime check.
- **Default `--array-strategy` is now `merge`** (was `replace`): arrays are reconciled element-wise against BASE by default. Use `replace` to own a list wholesale, or for arrays of structurally anonymous objects that `merge` can't match by value.
- Diagnostic key-paths (`--diff` output and `merge` conflict warnings) use the format's separator: `.` for JSON/YAML/TOML, `:` for plist (PlistBuddy).

## [0.0.3] - 2026-06-16

### Added

- **YAML** support alongside JSON and plist. The format is inferred from TARGET's extension (`.yaml`/`.yml`) or forced with `--format yaml`. Unlike JSON/plist, an existing YAML target is edited in place so **comments, blank lines, and formatting are preserved** on the parts that don't change. Constructs that can't be edited safely — anchors/aliases, tags, multi-document streams, non-string keys — are refused (the file is left untouched) rather than risk corruption.
- `--plist-binary` to write binary plist output instead of XML (plist reads already accept either).

### Changed

- **Renamed the project to `config-graft`** (was `json-apply`) to reflect that the merge engine is multi-format (JSON, plist, YAML), not JSON-specific. The crate, binary, and command are all now `config-graft`.
- DESIRED parse/shape errors now name the specific format (e.g. "DESIRED must be a YAML mapping") instead of a generic message.
- An invalid `--indent` value is now a usage error (exit 2) rather than a runtime error (exit 1).
- Format-specific flags (`--indent`, `--plist-binary`) now error and exit when passed with a format they don't apply to, instead of being silently ignored.

## [0.0.2] - 2026-06-15

### Added

- Apple **plist** support alongside JSON. The merge engine is format-agnostic; the format is inferred from TARGET's extension (`.plist` → plist, else JSON) and can be forced with `--format json|plist`. Reads accept both XML and binary plist; output is normalized XML. `Date`/`Data`/`Uid` scalars round-trip as atomic leaves.

### Changed

- Reworked the engine onto an internal Node value model so the reconcile logic is shared across formats.

## [0.0.1] - 2026-06-12

### Added

- Initial release: three-way reconcile for app-owned JSON files (`json-apply <TARGET> <DESIRED> [BASE]`), deep-merging a managed DESIRED subset into TARGET while preserving unmanaged keys and pruning dropped keys via a BASE ancestor.
- Array strategies: atomic by default, with `--array-strategy concat|set`.
- `--check`, `--stdout`, and `--diff` modes; `--indent` control.
- An empty BASE argument is treated as no base.

[Unreleased]: https://github.com/thanegill/config-graft/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/thanegill/config-graft/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/thanegill/config-graft/compare/v0.0.3...v0.1.0
[0.0.3]: https://github.com/thanegill/config-graft/compare/v0.0.2...v0.0.3
[0.0.2]: https://github.com/thanegill/config-graft/compare/v0.0.1...v0.0.2
[0.0.1]: https://github.com/thanegill/config-graft/releases/tag/v0.0.1
