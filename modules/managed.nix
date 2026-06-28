# home-manager wrapper: `home.managed{Json,Plist,Yaml,Toml}`. All the logic lives
# in the shared engine; this just selects the home-manager flavour. See ./lib.nix.
(import ./lib.nix).mkModule "home"
