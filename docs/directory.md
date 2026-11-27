# Directory mode

`config-graft directory` reconciles a whole directory tree instead of one file. TARGET, DESIRED and BASE are directories. A directory is treated like an object and each file or symlink like a single value, so the same three-way merge decides which files exist and what they hold: files the app created are kept, and files you stop declaring are pruned unless the user changed them.

```sh
config-graft directory dest/ desired/ base/                    # reconcile a tree
config-graft directory --diff --check dest/ desired/           # preview drift (exit 3 if pending)
config-graft directory --manage-root dest/ desired/            # also the root's own attributes
config-graft directory --no-owner --xattrs safe dest/ desired/ # narrow the metadata in scope
```

The exact rules are in [SPEC §5b](../SPEC.md#5b-directory-mode-the-directory-subcommand).

## What gets written

- **Only what changed.** Files are created, updated or deleted individually, so app-owned files keep their inode and mtime.
- **Each file atomically.** A file is written to a temp file in the same directory and renamed into place. Its bytes stream from source to destination without being buffered, and its identity is a SHA-256 digest, so large trees stay cheap.
- A file is one value: config-graft never merges inside it, and DESIRED's copy replaces TARGET's whole.
- **Symlinks** are managed by the path they point to and never followed. A dangling symlink is fine.
- **Read-only directories** can still be updated. A managed directory whose mode lacks owner-write, such as a Nix store `source`'s `0555`, gets owner-write only while config-graft changes it, and its mode is then put back (or set to DESIRED's).

## Metadata

A file's and a directory's mode, owner (uid/gid) and extended attributes are part of its identity. A metadata-only change counts as a change, shows up in `--diff`, and is applied on write.

- `--no-owner` leaves uid/gid alone.
- `--xattrs safe` skips privileged and system namespaces; `--xattrs none` ignores extended attributes. Attributes out of scope are neither read nor removed.
- An attribute that can't be set (no privilege to `chown`, say, or an xattr the filesystem rejects) refuses the run with nothing written, rather than leaving a half-applied file.
- A DESIRED tree owned by another user, such as a root-owned Nix store path, needs privilege to apply. Run as that owner, or pass `--no-owner`.
- The root directory you point at is left alone by default. `--manage-root` reconciles its own attributes too.

## What gets refused

These stop the run with exit 1 and write nothing:

- a FIFO, socket or device file;
- a filename that isn't valid UTF-8;
- a TARGET that exists but isn't a directory;
- replacing a directory with a file when the directory holds entries config-graft never managed (remove it by hand to proceed);
- a directory that can't be read partway through the walk, since a partial view would look like deleted files.

**Names TARGET would store as one file**, such as `Foo` and `foo` on a case-insensitive filesystem (the macOS default) or two Unicode spellings of one name, are refused too. If a DESIRED name matches an entry already on disk, the run refuses before anything is written, and `--check` refuses as well. If both names are new, the run stops only once the first is written, and `--check` and `--diff` don't report it. To recover, rename the entry on disk to DESIRED's spelling, or drop one name from DESIRED. A pair config-graft can't see is left alone: if BASE has `Foo` and TARGET holds it as `foo`, dropping `Foo` leaves `foo` behind, to remove by hand.

## Flags

`directory` takes `--manage-root`, `--no-owner` and `--xattrs`, plus the shared `--base`, `--no-prune`, `--diff` and `--check`. The byte-format flags (`--stdout`, `--follow-symlinks`, `--indent`, `--plist-format`, `--array-strategy`, `--merge-key`) don't exist on it, so passing one is a usage error (exit 2).

## Trade-offs

These are deliberate. [SPEC §10](../SPEC.md#10-known-trade-offs) has the full list.

- **Not one transaction.** A crash mid-apply leaves a partial tree in which every file is consistent; re-running completes it.
- **Hardlinks are not preserved.** A rewritten file gets a new inode, and other names linked to the old one keep the old content.
- **Not safe against concurrent writers.** The tree is read, reconciled, then applied, so a change made between the read and the write can be lost. Point config-graft at a tree no other process is changing.
- **Which name pairs are refused depends on TARGET's filesystem.** A tree holding `Foo` and `foo` is fine on a case-sensitive Linux filesystem and refused on a default macOS one.
- **Parent directories are trusted.** Symlinks inside the tree are never followed, but the path you give and its parents are used as given.
- **An empty declared directory can be pruned**, unlike an empty JSON object: a directory carries its own attributes, so even an empty one is a managed entry. For the same reason a directory the app emptied is removed once DESIRED drops it and its attributes still match BASE, where an object the app emptied inside a file is kept.
