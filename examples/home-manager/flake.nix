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
        ];
      };
    };
}
