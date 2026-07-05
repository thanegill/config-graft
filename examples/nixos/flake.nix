{
  description = "config-graft NixOS example";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    config-graft = {
      url = "github:thanegill/config-graft";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    { nixpkgs, config-graft, ... }:
    {
      # Build with: nixos-rebuild switch --flake .#example
      nixosConfigurations.example = nixpkgs.lib.nixosSystem {
        modules = [
          config-graft.nixosModules.default
          {
            nixpkgs.hostPlatform = "x86_64-linux";

            # System files an app owns and rewrites; reconciled during activation
            # with the previous generation as BASE. `target` is an absolute path;
            # the attribute name ("app") is just an identifier.
            environment.managedJson.app = {
              target = "/etc/app/config.json";
              settings = {
                theme = "dark";
                editor.fontSize = 14;
              };
            };

            environment.managedToml.app = {
              target = "/etc/app/config.toml";
              settings.theme = "dark"; # comments in the live file are preserved
            };

            # --- minimal host stubs so the example evaluates; replace with yours ---
            boot.loader.grub.enable = false;
            fileSystems."/" = {
              device = "/dev/disk/by-label/nixos";
              fsType = "ext4";
            };
            system.stateVersion = "24.05";
          }
        ];
      };
    };
}
