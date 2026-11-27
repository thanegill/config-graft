# Examples

Each subdirectory is a self-contained flake that wires config-graft into one platform. The options they use are described in [`docs/nix-modules.md`](../docs/nix-modules.md).

- [`home-manager/flake.nix`](home-manager/flake.nix): standalone home-manager, with targets relative to `$HOME`. It shows inline `settings`, a checked-in `source` file, and a `managedDirectory` tree. A second, aarch64-darwin configuration (`alice-darwin`) reconciles macOS preference domains with `cfprefsdDomain`.
- [`nixos/flake.nix`](nixos/flake.nix): a NixOS host, with absolute targets reconciled during system activation. One entry overrides `format` with a validating `pkgs.formats` generator, and one uses `environment.managedDirectory`.
- [`nix-darwin/flake.nix`](nix-darwin/flake.nix): a nix-darwin host, applied in the `postActivation` phase.

Each example only imports its platform's module. The modules run config-graft from this flake's own build by store path, so there is no overlay to apply and nothing to put on `PATH`.

The NixOS and nix-darwin examples include minimal host stubs (filesystem, bootloader, `stateVersion`) so they evaluate to a complete system. Replace those with your real host configuration.

## Evaluating one

Evaluate a build without applying it:

```sh
nix eval ./examples/home-manager#homeConfigurations.alice.activationPackage.drvPath
nix eval ./examples/home-manager#homeConfigurations.alice-darwin.activationPackage.drvPath
nix eval ./examples/nixos#nixosConfigurations.example.config.system.build.toplevel.drvPath
nix eval ./examples/nix-darwin#darwinConfigurations.example.config.system.build.toplevel.drvPath
```

The examples pull config-graft from `github:thanegill/config-graft`. To try them against a local checkout instead, override the input:

```sh
nix eval ./examples/home-manager#homeConfigurations.alice.activationPackage.drvPath \
  --override-input config-graft path:/path/to/config-graft
```
