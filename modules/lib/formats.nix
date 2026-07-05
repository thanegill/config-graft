# Static, pkgs-free per-format descriptors. `kind` picks freeform (a
# `pkgs.formats` generator) vs. plist (`lib.generators.toPlist`); `isFreeform` is
# the predicate `build` and the platforms branch on.
{
  formats = [
    {
      format = "json";
      fileExtension = "json";
      optionName = "managedJson";
      kind = "freeform";
      settingsExample = {
        theme = "dark";
        editor.fontSize = 14;
      };
    }
    {
      format = "yaml";
      fileExtension = "yaml";
      optionName = "managedYaml";
      kind = "freeform";
      settingsExample = {
        theme = "dark";
        plugins = [ "git" ];
      };
    }
    {
      format = "toml";
      fileExtension = "toml";
      optionName = "managedToml";
      kind = "freeform";
      settingsExample = {
        theme = "dark";
        editor.font_size = 14;
      };
    }
    {
      format = "plist";
      fileExtension = "plist";
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
