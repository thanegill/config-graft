{
  description = "config-graft NixOS example";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    # Outside this repo, use: config-graft.url = "github:thanegill/config-graft";
    config-graft.url = "path:../..";
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
            # Apply the overlay so the module finds the config-graft binary.
            nixpkgs.overlays = [ config-graft.overlays.default ];

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
