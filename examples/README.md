# Examples

Each subdirectory is a self-contained flake wiring config-graft into one platform:

- [`home-manager/flake.nix`](home-manager/flake.nix): standalone home-manager
  (`home.managed{Json,Yaml,...}`, targets relative to `$HOME`); one entry uses
  `source` (a checked-in file) instead of inline `settings`, and one uses
  `managedDirectory` (the `directory` subcommand) to reconcile a whole source tree into
  a target directory. A second, aarch64-darwin, configuration (`alice-darwin`)
  demonstrates per-user `cfprefsdDomain` (reconciling a macOS preference domain
  through cfprefsd).
- [`nixos/flake.nix`](nixos/flake.nix): a NixOS host (`environment.managed*`,
  absolute targets, reconciled during system activation); one entry overrides
  `format` with a validating `pkgs.formats` generator, and one uses
  `environment.managedDirectory` to reconcile a source tree.
- [`nix-darwin/flake.nix`](nix-darwin/flake.nix): a nix-darwin host (same
  `environment.managed*`, applied in the `postActivation` phase).

Each example just pulls in the wrapper for its platform. The modules run
config-graft from this flake's own build (by store path), so there's no overlay
to apply and nothing to put on `PATH`.

## Trying one

The examples pull config-graft from its published source
(`config-graft.url = "github:thanegill/config-graft"`). That repository is
**private**, so an unauthenticated `nix` cannot fetch it (a bare
`nix eval ./examples/...` 404s on the GitHub API). To evaluate against the
published flake you need repo access plus a nix token
(`--option access-tokens github.com=...`, or `access-tokens` in `nix.conf`), or a
`git+ssh://` input URL. Overriding the input with a local checkout (below) sidesteps
this entirely.

To try one against a local checkout, override the input:

```sh
nix eval ./examples/home-manager#homeConfigurations.alice.activationPackage.drvPath \
  --override-input config-graft path:/path/to/config-graft
```

Evaluate a build without applying it, e.g.:

```sh
nix eval ./examples/home-manager#homeConfigurations.alice.activationPackage.drvPath
nix eval ./examples/home-manager#homeConfigurations.alice-darwin.activationPackage.drvPath
nix eval ./examples/nixos#nixosConfigurations.example.config.system.build.toplevel.drvPath
nix eval ./examples/nix-darwin#darwinConfigurations.example.config.system.build.toplevel.drvPath
```

The NixOS and nix-darwin examples include a few minimal host stubs (filesystem,
bootloader, `stateVersion`) purely so they evaluate to a complete system; replace
those with your real host configuration.
