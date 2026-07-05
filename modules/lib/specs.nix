# Static, pkgs-free per-format descriptors. `kind` picks freeform (a
# `pkgs.formats` generator) vs. plist (`lib.generators.toPlist`); `isFreeform` is
# the predicate `build` and the platforms branch on.
{
  specs = [
    {
      fmt = "json";
      ext = "json";
      optionName = "managedJson";
      kind = "freeform";
      settingsExample = {
        theme = "dark";
        editor.fontSize = 14;
      };
    }
    {
      fmt = "yaml";
      ext = "yaml";
      optionName = "managedYaml";
      kind = "freeform";
      settingsExample = {
        theme = "dark";
        plugins = [ "git" ];
      };
    }
    {
      fmt = "toml";
      ext = "toml";
      optionName = "managedToml";
      kind = "freeform";
      settingsExample = {
        theme = "dark";
        editor.font_size = 14;
      };
    }
    {
      fmt = "plist";
      ext = "plist";
      optionName = "managedPlist";
      kind = "plist";
      settingsExample = {
        NSGlobalDomain.AppleShowAllExtensions = true;
        recentItems = [
          "a"
          "b"
        ];
      };
    }
  ];

  isFreeform = spec: spec.kind == "freeform";
}
