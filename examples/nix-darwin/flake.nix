{
  description = "config-graft nix-darwin example";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    nix-darwin = {
      url = "github:nix-darwin/nix-darwin";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    config-graft.url = "github:thanegill/config-graft";
  };

  outputs =
    { nix-darwin, config-graft, ... }:
    {
      # Build with: darwin-rebuild switch --flake .#example
      darwinConfigurations.example = nix-darwin.lib.darwinSystem {
        modules = [
          config-graft.darwinModules.default
          {
            nixpkgs.hostPlatform = "aarch64-darwin";

            # System files an app owns and rewrites; reconciled during the
            # postActivation phase with the previous generation as BASE. `target`
            # is an absolute path; the attribute name ("app") is just an identifier.
            environment.managedPlist.app = {
              target = "/Library/Preferences/com.example.app.plist";
              settings = {
                NSGlobalDomain.AppleShowAllExtensions = true;
              };
            };

            # --- minimal host stub so the example evaluates; replace with yours ---
            system.stateVersion = 5;
          }
        ];
      };
    };
}
