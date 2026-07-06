# Examples

Each subdirectory is a self-contained flake wiring config-graft into one platform:

- [`home-manager/flake.nix`](home-manager/flake.nix): standalone home-manager
  (`home.managed{Json,Yaml,...}`, targets relative to `$HOME`); one entry uses
  `source` (a checked-in file) instead of inline `settings`.
- [`nixos/flake.nix`](nixos/flake.nix): a NixOS host (`environment.managed*`,
  absolute targets, reconciled during system activation); one entry overrides
  `format` with a validating `pkgs.formats` generator.
- [`nix-darwin/flake.nix`](nix-darwin/flake.nix): a nix-darwin host (same
  `environment.managed*`, applied in the `postActivation` phase).

Each example just pulls in the wrapper for its platform. The modules run
config-graft from this flake's own build (by store path), so there's no overlay
to apply and nothing to put on `PATH`.

## Trying one

The examples pull config-graft from its published source
(`config-graft.url = "github:thanegill/config-graft"`). To try one against a local
checkout instead, override the input:

```sh
nix eval ./examples/home-manager#homeConfigurations.alice.activationPackage.drvPath \
  --override-input config-graft path:/path/to/config-graft
```

Evaluate a build without applying it, e.g.:

```sh
nix eval ./examples/home-manager#homeConfigurations.alice.activationPackage.drvPath
nix eval ./examples/nixos#nixosConfigurations.example.config.system.build.toplevel.drvPath
nix eval ./examples/nix-darwin#darwinConfigurations.example.config.system.build.toplevel.drvPath
```

The NixOS and nix-darwin examples include a few minimal host stubs (filesystem,
bootloader, `stateVersion`) purely so they evaluate to a complete system; replace
those with your real host configuration.
