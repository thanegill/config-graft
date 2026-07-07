{
  description = "config-graft standalone home-manager example";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    home-manager = {
      url = "github:nix-community/home-manager";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    config-graft = {
      url = "github:thanegill/config-graft";
      inputs.nixpkgs.follows = "nixpkgs";
    };
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
        pkgs = import nixpkgs { system = "x86_64-linux"; };

        modules = [
          config-graft.homeManagerModules.default

          # --- minimal home stub so the example evaluates; replace with yours ---
          {
            home.username = "alice";
            home.homeDirectory = "/home/alice";
            home.stateVersion = "24.05";
          }

          # config-graft (1): graft a few keys into a JSON file the app rewrites;
          # keys the app owns are preserved, keys you drop here are pruned on the
          # next switch (BASE is the previous generation's snapshot). The attribute
          # name is the target path, relative to $HOME (override with `target`).
          {
            home.managedJson.".config/app/config.json".settings = {
              theme = "dark";
              editor.fontSize = 14;
            };
          }

          # config-graft (2): YAML; comments in the live file are preserved.
          {
            home.managedYaml.".config/tool/config.yaml".settings.plugins = [ "git" ];
          }

          # config-graft (3): `source`: reconcile a file built some other way
          # (checked in here, but it could be any generator, rendered template, or
          # derivation) instead of inline `settings`.
          {
            home.managedJson.".config/widget/config.json".source = ./widget.json;
          }

          # config-graft (4): the `directory` subcommand: reconcile a whole *tree* into a
          # target directory. Files the app adds under it are kept; files you drop
          # from `source` are pruned on the next switch. A store-built `source` is
          # root-owned, so `noOwner` on this (non-root) home-manager activation.
          {
            home.managedDirectory.".config/app/plugins" = {
              source = ./plugins;
              noOwner = true;
            };
          }
        ];
      };

      # Activate with: home-manager switch --flake .#alice-darwin
      #
      # macOS-only: `cfprefsdDomain` reconciles a per-user preference domain through
      # cfprefsd (defaults export -> config-graft -> defaults import) instead of
      # editing the plist file in place, so a running app's cached prefs adopt the
      # merged result. It asserts a Darwin host, so it lives in its own aarch64-darwin
      # configuration rather than the linux `alice` above.
      homeConfigurations."alice-darwin" = home-manager.lib.homeManagerConfiguration {
        pkgs = import nixpkgs { system = "aarch64-darwin"; };

        modules = [
          config-graft.homeManagerModules.default

          # --- minimal home stub so the example evaluates; replace with yours ---
          {
            home.username = "alice";
            home.homeDirectory = "/Users/alice";
            home.stateVersion = "24.05";
          }

          {
            home.managedPlist."Library/Preferences/com.example.app.plist" = {
              cfprefsdDomain = "com.example.app";
              settings = {
                ShowStatusBar = true;
                FontSize = 13;
              };
            };
          }
        ];
      };
    };
}
