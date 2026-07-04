{
  description = "config-graft standalone home-manager example";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    home-manager = {
      url = "github:nix-community/home-manager";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    # Outside this repo, use: config-graft.url = "github:thanegill/config-graft";
    config-graft.url = "path:../..";
  };

  outputs =
    {
      nixpkgs,
      home-manager,
      config-graft,
      ...
    }:
    {
      # Activate with: home-manager switch --flake .#alice
      homeConfigurations."alice" = home-manager.lib.homeManagerConfiguration {
        # Apply the overlay so the module finds the config-graft binary.
        pkgs = import nixpkgs {
          system = "x86_64-linux";
          overlays = [ config-graft.overlays.default ];
        };

        modules = [
          config-graft.homeManagerModules.default
          {
            home.username = "alice";
            home.homeDirectory = "/home/alice";
            home.stateVersion = "24.05";

            # Graft a few keys into files the apps keep rewriting: keys the app
            # owns are preserved, keys you drop here are pruned on the next switch
            # (BASE is the previous generation's snapshot).
            home.managedJson.claude-code = {
              target = ".claude/settings.json";
              settings.permissions.ask = [ "Bash(git push)" ];
            };

            home.managedYaml.app = {
              target = ".config/app/config.yaml"; # comments in the live file are preserved
              settings.theme = "dark";
            };
          }
        ];
      };
    };
}
