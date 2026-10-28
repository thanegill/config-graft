# config-graft

Three-way reconcile (kubectl-apply-style) for app-owned **JSON / plist / YAML / TOML** config files, plus a **directory-tree** mode (the `directory` subcommand). `SPEC.md` is the source of truth for merge semantics; this file is orientation for working on the code.

## Commands (via the Nix dev shell)

```bash
nix develop --command cargo test     # unit (src) + per-format integration (tests/)
nix develop --command cargo clippy
nix develop --command cargo fmt -- --check
nix develop --command cargo mutants --in-diff <(git diff origin/master)   # mutation-test only the code the diff touches
nix build                            # hermetic; runs the test suite in checkPhase
nix flake check                      # the module layer (modules/tests.nix) -- cargo covers none of it
```

Refactors are expected to be behavior-preserving; `cargo test` plus `nix flake check` is the gate. Neither alone is enough: `cargo test` cannot see the Nix module's decisions, and the module's flag-mapping tests cannot see what the binary does with them -- that gap is how #43 stayed green on both sides.

## Design rules

The rationale for most of these lives in the doc comment at the code it governs; read that before changing the code.

- **Each format owns its leaf type** (`JsonLeaf`/`YamlLeaf`/`PlistLeaf`/`TomlLeaf`/`FsLeaf`), so encoders are total. Don't reintroduce a single enum mixing formats.
- **`Node::Map` carries no side payload.** Per-directory metadata (mode/owner/xattrs) is an ordinary leaf under the reserved empty-string key, so it reconciles through the same machinery as any entry. No `Leaf::LeafMeta`, no directory-specific `Node` impls.
- Every leaf keeps `Hash` consistent with `Eq` (array set-union dedups through `HashSet`). `Node`'s `Hash` is hand-written so a `Map` hashes order-independently, matching `IndexMap`'s `PartialEq`.
- **All output rendering lives in `src/render.rs`**: the primitives plus `Node`'s `Display` and `Node::diff`. `--diff` output is asserted byte-for-byte, and one file deciding all of it is what makes a change to it visible. Each format's leaf `Display` stays with its codec; `canonical_float_bits` stays in `value.rs` (it serves the `Eq`/`Hash` contract, not output).
- **`read_file` returns `Result<Option<Node>, Error>`** -- `Ok(None)` is absent/empty (a first apply), `Err` is present-but-unreadable -- and `Backend::run` refuses a TARGET that parses to a non-mapping. Don't collapse these into one `Option`: an unreadable TARGET treated as `{}` makes the result DESIRED alone, so the write replaces every app-owned key with exit 0. That is what turned the `1e400` JSON parse failure into whole-file loss.
- **Dispatch is static.** `main` matches the clap subcommand and calls `ByteBackend::<F>::run` or `Directory::run`; each subcommand exposes only its own flags, so an unsupported flag/format pairing is a clap usage error (exit 2). No runtime flag checks, no extension inference.
- **`serde_json` is a dev-dependency only** (the `json!` fixtures in the reconcile tests). Don't bring it back into the shipped binary -- not for the JSON codec (see JSON below) and not for rendering.
- **Never compare JSON numbers through json-syntax's number type**: it stores and compares lexically (by its `Ord`, `1` > `0.1e+80`; by its `Eq`, `1` != `1.0`). Value comparison is `src/number.rs`, whose `Hash` must stay consistent with `Eq` or array set-union keeps both spellings. Its differential tests against `bigdecimal` assert how many pairs they compared -- keep that, or a regression that merely declines more literals passes vacuously.
- **Don't hand-roll a float renderer.** `render_f64` goes through `ryu`; Rust's `{:?}` disagreed with serde_json on 3.6% of `[1e13, 1e16)`. The two tests in `render.rs` cross-check it against serde_json, which formats through `zmij` -- that is why the binary doesn't depend on `zmij` itself: the cross-check would then compare zmij with zmij and pass vacuously. `quote` goes through json-syntax and was checked by hand to escape byte-for-byte like serde_json (control bytes, NUL, DEL, astral-plane characters); no test locks that, so re-check it if you touch `quote`.
- **YAML/TOML writes edit the original document in place** to keep comments, and refuse rather than corrupt on anything they can't safely edit.
- **Directory mode's trade-offs are deliberate**: non-transactional across files, hardlinks not preserved, ancestor path components trusted. They are locked by characterization tests (`hardlink_is_broken_on_rewrite`, `eacces_mid_walk_refuses_whole_run`) and documented in SPEC §10 -- don't "fix" them silently.
- `saphyr` (0.0.x, YAML 1.2) and `json-syntax` are pre-1.0; `Cargo.lock` is pinned for `nix build`.

## Format codecs

Each codec's mechanics are documented at the code. These are the decisions that are easy to undo by accident.

### JSON (`json.rs`)

- Parses and prints with **`json-syntax`, not serde_json**: it keeps every number's source literal, so `JsonLeaf::Number(String)` round-trips `1e1`, `1E2` and `1e400` as written. serde_json normalized exponents while scanning, and its `arbitrary_precision` made `$serde_json::private::Number` a magic object key at any depth. Don't reintroduce it.
- There is deliberately **no `Float(f64)`**: it shortened high-precision decimals, turned integers past `u64` into floats, and made an out-of-range exponent unparseable -- which silently replaced the file, because an unparseable TARGET was then treated as `{}` (see `read_file` above).
- Arrays and objects print with `Limit::Item(0)`, not `Limit::Always`: `Always` expands an empty container, so `{}` would become `{\n}` and every managed file would be reformatted.
- The nesting check runs as a byte scan **before** `Value::parse_str`. Don't move it behind the parser: json-syntax drops a `Value` recursively, so a check on the finished tree aborts on the drop it exists to prevent.

### plist (`plist.rs`)

- `--plist-format follow` (the default) writes the encoding the target already has (issue #43): re-encoding a binary plist as XML floors the app's dates and puts its bytes out of reach. `resolve_write_opts` runs **once, before the first input is read**, because the date floor is encoding-dependent; the one `WriteOpts` is threaded through, so no codec ever sees `Follow`.
- The XML date floor runs in `normalize_for_run` on **all three inputs** (BASE without warnings), not in `serialize`. Flooring only the written bytes leaves a floored TARGET unequal to its fractional BASE, and a managed date key can then never be pruned.
- `refuse_on_write` lets through, with a warning, XML-illegal characters the target already held, **only when the on-disk bytes are XML**. Keep that gate: `follow` makes binary-target-to-XML unreachable by default, but `--plist-format xml` still reaches it, and dropping the gate reopens #33.
- **One classification (`CurrentBytes::classify`) answers both** "which encoding does follow write" and "may anything carry over" -- two predicates disagreed and caused #44. Its two call sites have different preconditions (`refuse_on_write` sees only bytes that already parsed; `resolve_write_opts` sees raw bytes before any parse), so check any loosening against both.
- A carriage return is **not** refused: plist >= 1.10 writes `&#13;`, which round-trips. `modules/lib/common.nix`'s build-time assertion leaves `\r` off its list for the same reason.
- A normalization collapse in an array is a **warning, not a refusal**. An earlier revision refused, and because an unresolvable path counted as zero survivors it failed valid runs -- don't reintroduce that. The warning cannot see a date inside an array of dicts (issue #37).
- Depth is checked on the built `plist::Value` and a rejected tree is `dismantle`d, never dropped. The event API that would avoid building it sits behind an unstable crate feature -- not worth pinning plist to a tilde requirement.

### TOML

The root is always a table, so `NotTomlTable` is unreachable; it is kept only for `FormatKind` symmetry.

### Directory mode (`directory.rs`)

- Not a `Format` (a tree has no byte stream); it plugs into the shared run via `impl Backend for Directory`.
- `FsLeaf::File` equality is `(len, digest, attrs)` and ignores the source path -- that is what keeps re-apply a no-op.
- `parse` is the single place the reserved empty-string key is interpreted; the write path works on `FsTree`, which cannot express attrs-as-an-entry.
- Because a directory's attrs are a reserved-key leaf, an **empty declared directory is prunable**, unlike a JSON `{}` value.

## Nix modules (`modules/`)

Three module files, one per platform: `home-manager.nix` (`home.managed*`), `nixos.nix` and `darwin.nix` (`environment.managed*`, thin wrappers over `lib/system.nix` that pass only their activation wiring). There is **no generic engine and no per-platform dispatch record** -- each module writes its `options`/`config` linearly and pulls format-agnostic pieces from `lib/`. Keep it that way.

### Recursion trap

`_module.freeformType` punishes config keys that depend on `config` (`pkgs` included). Avoided by construction -- keep both:

1. The module is chosen by the **file**, never by `pkgs.stdenv`, so config keys never depend on `pkgs`.
2. Every config fragment uses a **static top-level key** whose value aggregates over `active` (e.g. `home.file = mapAttrs' ... entries`). Never `mkMerge (mapAttrsToList ... active)` at the top.

### plist entries and issue #43

- On the file path, `mkEntryReconcileScript` adds `--plist-format binary` when `binary` is true and **omits the flag entirely** otherwise, so the write follows the target's encoding. That omission is the whole fix for #43; a `--plist-format xml` there puts the bug straight back.
- The **cfprefsd path always passes `--plist-format binary`**, whatever `binary` says: its scratch file goes back through `defaults import`, and routing it through XML only risks loss (XML-illegal bytes, sub-second dates). `follow` is not enough, because an empty domain's scratch file comes from `plutil -create xml1`, which `follow` would read as XML.
- `binary = true` runs the entry's own `pkgs.formats` generator, then converts with `plistutil`. Don't reintroduce the `builtins.toJSON` hop this replaced -- it bypassed the entry's `format` override. The conversion is type-faithful for everything `settings` can hold (a bool stays `<true/>`, not `<integer>1</integer>`; empty dicts and lists survive); no test locks that, so re-check it before swapping the converter.
- `mkAssertions` fails the build for an XML-illegal byte in `settings` while `binary` is false; its predicate leaves `\r` off for the same reason the Rust side does (plist writes `&#13;`).

### Tests and examples

- `tests.nix` is mostly pure evaluation, but `binaryTargetRun` builds the real binary and runs the generated script against a binary plist holding a raw ESC -- the only test that sees the module and binary together. The ESC is built with `printf '\033'` in the builder rather than spelled in the Nix source.
- `examples/{home-manager,nixos,nix-darwin}/` are standalone consumer flakes on `github:thanegill/config-graft`; test a local checkout with `--override-input config-graft path:.`.
