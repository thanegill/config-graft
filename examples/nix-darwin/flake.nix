{
  description = "config-graft nix-darwin example";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    nix-darwin = {
      url = "github:nix-darwin/nix-darwin";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    config-graft = {
      url = "github:thanegill/config-graft";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    { nix-darwin, config-graft, ... }:
    {
      # Build with: darwin-rebuild switch --flake .#example
      darwinConfigurations.example = nix-darwin.lib.darwinSystem {
        modules = [
          config-graft.darwinModules.default

          # --- minimal host stub so the example evaluates; replace with yours ---
          {
            nixpkgs.hostPlatform = "aarch64-darwin";
            system.stateVersion = 5;
          }

          # config-graft: system files an app owns and rewrites; reconciled during
          # the postActivation phase with the previous generation as BASE. The
          # attribute name is the target's absolute path (override with `target`).

          # config-graft (1): edited in place -- config-graft rewrites the .plist
          # file directly.
          {
            environment.managedPlist."/Library/Preferences/com.acme.editor.plist".settings = {
              ShowLineNumbers = true;
            };
          }

          # config-graft (2): reconciled through cfprefsd -- set `cfprefsdDomain` so
          # the merge goes `defaults export` -> config-graft -> `defaults import`,
          # instead of editing the file (whose on-disk copy cfprefsd may override
          # from cache). Runs as root, so this targets the system/global domain.
          {
            environment.managedPlist."/Library/Preferences/com.acme.daemon.plist" = {
              cfprefsdDomain = "com.acme.daemon";
              settings.LogLevel = "info";
            };
          }
        ];
      };
    };
}
