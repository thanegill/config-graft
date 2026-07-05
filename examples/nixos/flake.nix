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

          # --- minimal host stub so the example evaluates; replace with yours ---
          {
            nixpkgs.hostPlatform = "x86_64-linux";
            boot.loader.grub.enable = false;
            fileSystems."/" = {
              device = "/dev/disk/by-label/nixos";
              fsType = "ext4";
            };
            system.stateVersion = "24.05";
          }

          # config-graft: system files an app owns and rewrites; reconciled during
          # activation with the previous generation as BASE. The attribute name is
          # the target's absolute path (override with `target`).
          (
            { lib, pkgs, ... }:
            {
              # `format`: the `pkgs.formats` generator that serializes `settings`
              # and supplies the type that validates it. Here we wrap the JSON
              # generator's type with an extra check, so `theme` must be "dark" or
              # "light" -- any other value fails at build time.
              environment.managedJson."/etc/app/config.json" = {
                format =
                  let
                    jsonFormat = pkgs.formats.json { };
                  in
                  jsonFormat
                  // {
                    type = lib.types.addCheck jsonFormat.type (
                      v:
                      !(v ? theme)
                      || builtins.elem v.theme [
                        "dark"
                        "light"
                      ]
                    );
                  };
                settings = {
                  theme = "dark";
                  editor.fontSize = 14;
                };
              };

              # comments in the live file are preserved
              environment.managedToml."/etc/app/config.toml".settings.theme = "dark";
            }
          )
        ];
      };
    };
}
