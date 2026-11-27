# config-graft

> **AI Disclosure:** This project falls under [Level 5: Bots coded, human understands completely](https://www.visidata.org/blog/2026/ai/). Exact model used varies by commit and is in the commit message. This is disclosed as useful context for anyone depending on, auditing, or forking this code, not because I think it lowers the bar on the quality. Reach out or report bugs the way you would for any other project.

Plenty of apps rewrite their own config file, saving window sizes and recent files next to the settings you care about. That makes the file hard to manage declaratively. Link it from Nix with `home.file` and the app either fails to save into the read-only store path or replaces the link with a file of its own. The next `home-manager switch` then refuses to run or, when forced, clobbers the app's changes. Write your settings over the file and the app's own keys are gone. A plain deep-merge keeps them, but it can never remove a key you stopped managing.

config-graft manages only your part of the file. It works on JSON, plist, YAML and TOML files and on directory trees, using a three-way merge like `kubectl apply`:

- **TARGET**: the live file, which the app also writes.
- **DESIRED**: the keys you manage.
- **BASE**: the DESIRED you applied last time (optional).

```
TARGET  {"a":1,"b":5,"appOnly":true}
DESIRED {"c":3}
BASE    {"a":1,"b":2}
RESULT  {"b":5,"appOnly":true,"c":3}
```

`c` is added and `appOnly`, which you never managed, is left alone. `a` and `b` were dropped from DESIRED: `a` still holds its BASE value, so it is pruned, while `b` was edited since, so the edit survives.

## What it looks like

### Lists of records

An app keeps a list of servers, adds its own `cache` entry and records each server's `status`. You manage the replica counts:

```yaml
# servers.yaml (TARGET)        # desired.yaml (DESIRED)
servers:                       servers:
  - name: web                    - name: web
    replicas: 2                    replicas: 3
    status: healthy              - name: db
  - name: db                       replicas: 1
    replicas: 1
  - name: cache
    replicas: 1
```

With `--merge-key name`, records are matched by name, so `web` is updated in place and the app's `status` and `cache` survive:

```console
$ config-graft yaml --merge-key name servers.yaml desired.yaml
$ cat servers.yaml
servers:
  - name: web
    replicas: 3
    status: healthy
  - name: db
    replicas: 1
  - name: cache
    replicas: 1
```

Without a key, the edited `web` would count as a new record and appear twice. See [`docs/arrays.md`](docs/arrays.md) for the array strategies.

### Drift checks

`--check` writes nothing and exits 3 when applying would change the file, so CI or an activation script can catch drift, and `--diff` shows what would change. Here someone switched the theme by hand, and `wordWrap` was dropped from DESIRED since the last apply:

```console
$ config-graft json --check --diff settings.json desired.json last-applied.json
~ theme: "light" => "dark"
- wordWrap = true
$ echo $?
3
```

The app's `recentFiles` key isn't in the diff, because it was never managed.

### Directory trees

`config-graft directory` manages the files you declare in a directory, with their mode and owner, and leaves the files the app keeps there alone. Here `desired/` holds an executable hook and a private settings file, and the app's own `app/cache.db` isn't declared:

```console
$ config-graft directory --check --diff app desired
~ hooks/pre-commit: file(19 bytes, 0644, 501:0) => file(24 bytes, 0755, 501:0)
+ settings.toml = file(15 bytes, 0600, 501:0)
```

Applying it rewrites the hook as `0755`, adds `settings.toml` as `0600` and doesn't touch `cache.db`. See [`docs/directory.md`](docs/directory.md) for metadata and what is refused.

## Usage

```sh
config-graft json config.json desired.json last-applied.json          # reconcile in place
config-graft json --check config.json desired.json last-applied.json  # exit 3 if it would change
config-graft json --stdout --diff config.json desired.json            # preview without writing
config-graft yaml config.yaml desired.yaml                            # YAML, keeping comments
config-graft directory dest/ desired/                                 # a directory tree
```

The subcommand (`json`, `yaml`, `toml`, `plist` or `directory`) names the format, and `config-graft <format> --help` lists its flags. Exit codes: 0 success, 1 error, 2 usage error, 3 change pending under `--check`.

## Nix modules

The flake ships modules for home-manager, NixOS and nix-darwin. Each entry reconciles its `settings` (or a `source` file) into a live file on every activation, with the previous generation's snapshot as BASE:

```nix
# with config-graft.homeManagerModules.default in your modules
home.managedJson.".config/app/config.json".settings = {
  theme = "dark";
  editor.fontSize = 14;
};
```

[`examples/`](examples) has a complete `flake.nix` for home-manager, NixOS and nix-darwin.

## Documentation

- [`docs/formats.md`](docs/formats.md): how each format reads and writes, including JSON number spelling, plist encodings and dates, and YAML/TOML in-place edits.
- [`docs/arrays.md`](docs/arrays.md): array strategies and `--merge-key` for lists of records.
- [`docs/directory.md`](docs/directory.md): reconciling a directory tree, with metadata.
- [`docs/nix-modules.md`](docs/nix-modules.md): every module option, including `cfprefsdDomain` for macOS preferences.
- `man config-graft` and `man config-graft-<format>`: the command-line reference, installed by the Nix package along with the formats, arrays and directory guides as `man 7 config-graft-formats`, `man 7 config-graft-arrays` and `man 7 config-graft-directory`.
- [`SPEC.md`](SPEC.md): the specification, and the source of truth for merge semantics.
- [`CHANGELOG.md`](CHANGELOG.md): what changed in each release.

## Develop

All tooling comes from the flake:

```sh
nix develop          # cargo, rustc, clippy, rustfmt, rust-analyzer, cargo-llvm-cov
cargo test           # unit + integration tests
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
cargo llvm-cov       # source-based coverage
nix build            # hermetic build, runs the tests; ./result/bin/config-graft
man ./result/share/man/man1/config-graft.1.gz
nix run . -- --help
```

## Related

- Modeled on `kubectl apply`'s three-way merge against `last-applied-configuration`, scoped to a local file instead of a cluster object.
- The ordered-array merge draws on Schwägerl, Uhrig & Westfechtel, "A graph-based algorithm for three-way merging of ordered collections in EMF models," *Science of Computer Programming* 113 (2015), pp. 51–81, [doi:10.1016/j.scico.2015.02.010](https://doi.org/10.1016/j.scico.2015.02.010) ([open-access version](https://www.scitepress.org/papers/2014/47021/47021.pdf)).
