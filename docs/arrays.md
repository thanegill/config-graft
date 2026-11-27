# Arrays

Objects merge key by key. A scalar in DESIRED replaces TARGET's, unless the two are already equal, in which case TARGET's spelling is kept. `null` is an ordinary value, not a delete marker: only the BASE-to-DESIRED diff removes keys. An object left empty by a prune is removed, but one DESIRED declares, or one the app had already emptied, is kept.

Arrays need a rule for combining two lists, which you pick with `--array-strategy` on the `json`, `yaml`, `toml` and `plist` subcommands:

- `merge` (the default): a three-way merge against BASE that keeps what either side added, prunes what DESIRED dropped, and preserves reordering. See below.
- `replace`: DESIRED's list wins wholesale. Use it when you own the whole list, or for a list of objects with no field to match them by.
- `concat`: DESIRED's elements are appended to TARGET's, keeping order and duplicates.
- `set`: a two-way union that ignores order and BASE and drops duplicates. An element you remove from DESIRED is not removed from TARGET.

A strategy applies only when both sides hold an array at that key; otherwise DESIRED's value replaces TARGET's. A managed array that you drop from DESIRED entirely is pruned as one value, whatever the strategy, and only if TARGET still holds what BASE had.

The exact rules are in [SPEC §5](../SPEC.md#5-merge--type-semantics).

## How `merge` works

For each element, by value:

- an element is kept if TARGET or DESIRED has it and neither side deleted it since BASE;
- an element in BASE that DESIRED dropped is pruned, and one the user deleted from TARGET stays deleted;
- a reordering on either side is preserved.

With no BASE, every element counts as new, so `merge` behaves like `set`: TARGET's order, then DESIRED's additions.

Membership is a set, so a value held twice in one input keeps only its first copy, and config-graft warns on stderr when that drops something.

If TARGET and DESIRED reorder the same elements in contradictory ways, that is a conflict. config-graft still produces a deterministic result, preferring TARGET's order, and prints a warning naming the array and the elements involved. The exit code is unchanged.

## Keyed lists: `--merge-key`

By default `merge` matches elements by their whole value, which breaks down for a list of records. Say the app owns a `servers` list and has added a `status` field to `web`, and you manage each server's `replicas` and want to bump `web` from 2 to 3:

```jsonc
// TARGET
{ "servers": [ { "name": "web", "replicas": 2, "status": "healthy" } ] }
// DESIRED
{ "servers": [ { "name": "web", "replicas": 3 } ] }
```

Without a key, the edited `web` reads as a deletion plus an insertion, and you get two `web` entries:

```json
{ "servers": [
  { "name": "web", "replicas": 2, "status": "healthy" },
  { "name": "web", "replicas": 3 }
] }
```

With `--merge-key name`, `web` is matched by its `name` and its fields merge three-way like any object: the managed `replicas` updates, the app's `status` survives, and there is one entry:

```json
{ "servers": [ { "name": "web", "replicas": 3, "status": "healthy" } ] }
```

Forms of `--merge-key`, which apply only under `merge` and can be repeated:

- `--merge-key name` applies to every array of objects.
- `--merge-key name,id` gives candidate fields; the first one present wins.
- `--merge-key servers=name` applies only to the array at that path. The path runs from the document root (or, inside a matched record, from that record), with segments joined by `.` (or `:` for plist), so `--merge-key spec.containers=name` and `--merge-key spec.template.containers=name` can differ. Write each key in the path unquoted, even one a warning prints quoted (`x.[b]`, not `x."[b]"`); a scoped key whose path names no array in any input is reported on stderr.

Keying engages only when a key resolves and every element on both sides is an object carrying it; otherwise `merge` falls back to matching by value. Two records with the same key in one input collapse to the first, with a warning.

A warning about an array inside a matched record names it with a selector, such as `servers[name="web"].tags`.
