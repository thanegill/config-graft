# config-graft

> **AI Disclosure:** This project falls under [Level 5: Bots coded, human understands completely](https://www.visidata.org/blog/2026/ai/). Exact model used varies by commit and is in the commit message. This is disclosed as useful context for anyone depending on, auditing, or forking this code, not because I think it lowers the bar on the quality. Reach out or report bugs the way you would for any other project.

Three-way reconcile/merge for **JSON, plist, YAML, and TOML files** or a **directory tree**.

It deep-merges a *managed subset* (DESIRED) into a file the application also writes to (TARGET), while:

- **preserving** keys the app/user wrote that you don't manage,
- **pruning** keys you used to manage but dropped — but only if the user hasn't changed them, using a **BASE** snapshot (the previously-applied config) as the merge ancestor.

## Usage

```sh
config-graft <FORMAT> <TARGET> <DESIRED> [BASE]   # FORMAT: json | yaml | toml | plist | directory

config-graft json config.json desired.json .state/last-applied.json
config-graft json --check config.json desired.json base.json    # exit 3 if it would change
config-graft json --stdout --diff config.json desired.json       # preview without writing
config-graft json --array-strategy replace config.json desired.json  # own the list wholesale
config-graft json --merge-key name config.json desired.json          # match list-of-objects by `name`

config-graft plist app.plist desired.plist base.plist        # same merge, plist files
config-graft plist config desired                            # plist on any name (the subcommand names the format)
config-graft plist --plist-format binary app.plist desired.plist  # force a binary plist

config-graft yaml config.yaml desired.yaml                  # YAML, keeping comments
config-graft toml config.toml desired.toml                  # TOML, keeping comments

config-graft directory dest/ desired/                       # reconcile a directory tree
```

The format is a required **subcommand** (`json`, `yaml`, `toml`, `plist`, or `directory`); each subcommand exposes only the flags that apply to it, so an unsupported flag/format pairing is a usage error rather than a runtime one.

Arrays merge three-way by default; `--array-strategy` and `--merge-key` change that. See [`docs/arrays.md`](docs/arrays.md).

## Formats

JSON, plist, YAML and TOML each write differently: see [`docs/formats.md`](docs/formats.md).

## Nix modules

The flake ships modules for home-manager, NixOS and nix-darwin. Each entry reconciles its `settings` into a live file on every activation, with the previous generation as BASE:

```nix
{
  imports = [ config-graft.homeManagerModules.default ];

  home.managedJson.".config/app/config.json".settings = {
    theme = "dark";
    editor.fontSize = 14;
  };
}
```

[`examples/`](examples) has a complete `flake.nix` for each platform, and [`docs/nix-modules.md`](docs/nix-modules.md) covers every option.

## Directory mode

`config-graft directory dest/ desired/` reconciles a whole tree, file by file, including mode, owner and extended attributes. See [`docs/directory.md`](docs/directory.md).

## Develop

All tooling comes from the flake:

```sh
nix develop          # cargo, rustc, clippy, rustfmt, rust-analyzer, cargo-llvm-cov
cargo test           # unit + integration tests
cargo clippy --all-targets -- -D warnings
cargo llvm-cov       # source-based coverage (LLVM_COV/PROFDATA preset by the shell)
```

Or build/test hermetically through Nix (runs the test suite in `checkPhase`):

```sh
nix build            # ./result/bin/config-graft
nix run . -- --help
```

## Related

- Modeled on `kubectl apply`'s three-way merge (against its `last-applied-configuration`), scoped to a single local file rather than a cluster object.
- The three-way merge of **ordered arrays** draws on Schwägerl, Uhrig & Westfechtel, "A graph-based algorithm for three-way merging of ordered collections in EMF models," *Science of Computer Programming* 113 (2015), pp. 51–81, [doi:10.1016/j.scico.2015.02.010](https://doi.org/10.1016/j.scico.2015.02.010) ([open-access conference version](https://www.scitepress.org/papers/2014/47021/47021.pdf)).
- [`SPEC.md`](SPEC.md) — the full specification: semantics, exit codes, edge cases.
