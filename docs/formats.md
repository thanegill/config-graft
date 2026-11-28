# Formats

config-graft reads and writes JSON, plist, YAML and TOML. The subcommand picks the format (`config-graft json|plist|yaml|toml ...`), and that one format governs TARGET, DESIRED, BASE and the output. There is no extension inference and no conversion between formats. For a directory tree, see [directory.md](directory.md).

What differs between the formats is how they write:

- **JSON** and **plist** rewrite the whole file in a canonical layout. Comments and formatting are not kept.
- **YAML** and **TOML** edit the existing text in place, so comments, blank lines and formatting survive on everything config-graft doesn't change. They refuse an edit they can't make safely.

In every format, keys keep TARGET's order and keys new to it are appended in DESIRED's order; in TOML, `[table]` sections always follow the plain keys, so a new plain key goes before them. `--indent` is JSON-only, and passing it to another subcommand is a usage error (exit 2). The sign of a zero counts in every format, so `-0.0` and `0.0` are different values.

All four refuse (exit 1, file untouched) a TARGET that exists but does not parse or isn't a mapping, rather than treating it as empty and writing DESIRED over the app's keys. An empty or missing TARGET is a first apply. The exact rules are in [SPEC §5a](../SPEC.md#5a-formats) and [§7](../SPEC.md#7-file-handling--robustness).

A TARGET that is a symlink is read through the link, but writing to it is refused (exit 1), since replacing the link would leave the file it points at unchanged. Pass `--follow-symlinks` to write the file the link resolves to instead. The refusal comes only when there is something to write: an unchanged result exits 0, `--check` refuses (exit 1, not 3) when a change is pending, and `--stdout` never refuses. With the flag, a dangling link whose directory doesn't exist exits 1 rather than creating it. See [SPEC §8](../SPEC.md#8-atomicity--formatting).

## JSON

- Numbers keep their spelling: `1e1`, `1E2`, `2.50` and `1e400` reach disk exactly as the file wrote them. A high-precision decimal, an integer larger than `u64`, or an exponent past `f64`'s range round-trips unchanged instead of being shortened, turned into `1.2345e29`, or making the file unreadable.
- Numbers compare by value: `10`, `10.0` and `1e1` are the same number. A value the app rewrote in another spelling still prunes, and a DESIRED spelled differently from TARGET leaves the file alone.
- The cost of comparing by value: a DESIRED of `8080` cannot rewrite a TARGET's `8080.0`. That matters when the consumer is strict about integers, such as a Go `json.Unmarshal` into an `int` or a `"type": "integer"` schema.
- Output is pretty-printed with `--indent` (a number of spaces, or `tab`; default 2 spaces).
- JSONC and JSON5 are not supported.
- A document nested deeper than 512 containers is refused.

## plist

### Encoding

Reads accept XML and binary plists. By default (`--plist-format follow`) the output keeps the encoding the target already has: binary stays binary, XML stays XML, and a target that is absent, empty or neither is written as XML. `--plist-format xml` or `--plist-format binary` forces one. An existing binary Preferences file therefore stays binary, and none of the XML limits below apply to it.

`Date`, `Data` and `Uid` values round-trip unchanged, except for the XML date floor below. plist has no `null`.

### Dates in an XML run

CFPropertyList's XML parser rejects a fractional-second `<date>`, and a date read out of a binary plist (which is what `defaults export` produces) almost always has one. So an XML run:

- floors every `Date` to a whole second, on all three inputs, so TARGET still compares equal to BASE and a managed date key can still be pruned;
- reports each floored date on stderr (TARGET and DESIRED only);
- warns, and writes anyway, when flooring merges two dates less than a second apart and the merge keeps only one. An array config-graft doesn't manage is copied through untouched and never warns. The check looks at whole elements, so two dicts in an array that differed only in a sub-second date are reported, and so are two `--merge-key` records whose keys the floor made equal.

### XML-illegal characters

XML 1.0 cannot carry a C0 control character other than tab, newline and carriage return, such as the ESC (`0x1B`) separators in `NSUserKeyEquivalents`. macOS's own parser tolerates them, so a file holding one loads on macOS but nowhere else. In an XML run:

- a value this run would *introduce* with such a character is refused (exit 1, target untouched);
- a value the XML target already holds is passed through with a warning, so a run that changes nothing never fails on a file macOS wrote;
- rewriting a binary target as XML (only possible with `--plist-format xml`) writes every value afresh, so nothing carries over and such a value is refused. The error says whether the target was binary or not recognized as XML;
- a carriage return is written as `&#13;` and survives.

A target counts as XML however its prologue opens: comments and processing instructions ahead of `<?xml` are read the way every plist parser reads them.

### Other limits

A date outside the years 0 to 9999 is refused in an XML run. A binary write has none of the XML limits on this page.

A document nested deeper than 512 containers is refused, in either encoding.

## YAML

YAML 1.2, parsed with [saphyr](https://github.com/saphyr-rs/saphyr).

- Edits in place: comments, blank lines, quoting and indentation are kept on every part of the file config-graft doesn't change. Only an empty or first-apply target is written from scratch. Lines it adds or re-renders end in CRLF when most of the file's lines do, LF otherwise.
- Custom tags, multi-document streams, non-string keys and a non-mapping root are refused (exit 1, file untouched), and so is an edit that would change what an alias resolves to, or editing inside an alias to a non-empty mapping or sequence. An alias itself can be replaced or pruned whatever it refers to.
- Anchors and aliases are otherwise kept as written, except in an entry config-graft re-renders (a changed sequence, a scalar turned into a collection), which is written expanded.
- Every write is checked to read back as the intended result before it lands.
- A target holding only comments, or a bare `---`, counts as an empty mapping. Its text is kept and the managed keys are appended. A prune that removes the last key writes `{}` on a line of its own, after any comments or bare `---` the file keeps (but before a document-end `...`).
- A float config-graft writes takes its shortest form (`1.0`, `1.0e+300`, `.inf`). DESIRED's spelling is not kept, and a sequence that changes is rewritten whole.
- Comments are not moved when their key moves. A mapping the result empties or fills is re-rendered whole, which drops its comments, including the one on its key line.
- A document nested deeper than 512 containers is refused, with an alias counted as deep as the node it names.

## TOML

Parsed and edited with [toml_edit](https://github.com/toml-rs/toml).

- Edits in place, like YAML: comments, blank lines and formatting are kept on everything config-graft doesn't change. A file keeps its CRLF or LF line endings; one that mixes them is written with the ending most of its lines have. A first apply is written with ordinary `[section]` tables.
- Every write is checked to read back as the intended result, and an edit that can't be made safely is refused (exit 1, file untouched).
- Date-times round-trip unchanged. TOML has no `null`.
- Comments are not moved when their key moves. A dotted or implicit table that the result empties is rewritten as an explicit `[table]` header.

## Warnings

Whenever a run changes something beyond the managed edits, or a flag applies to nothing, config-graft prints a `config-graft: warning:` line on stderr, usually naming the path (`.`-separated, or `:` for plist), which can step into an array element or a keyed record, as in `servers[name="web"].tags`. Warnings never change the exit code, and `--check` and `--stdout` print them too, except the two that name the file instead of a key path (a dropped setuid, setgid or sticky bit, and a TARGET that changed before the write), which only a run that writes reports. See [SPEC §5 Diagnostics](../SPEC.md#diagnostics) for the full list.
