# Examples

Each subdirectory is a self-contained flake wiring config-graft into one platform:

- [`home-manager/`](home-manager) — standalone home-manager
  (`home.managed{Json,Yaml,...}`, targets relative to `$HOME`).
- [`nixos/`](nixos) — a NixOS host (`environment.managed*`, absolute targets,
  reconciled during system activation).
- [`nix-darwin/`](nix-darwin) — a nix-darwin host (same `environment.managed*`,
  applied in the `postActivation` phase).

Every example applies `config-graft.overlays.default` so the module finds the
`config-graft` binary, then pulls in the wrapper for its platform.

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
