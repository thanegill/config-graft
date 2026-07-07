# Per-format descriptors, keyed by the format name. The key is also the
# `pkgs.formats.<name>` generator and the config-graft subcommand name, so
# every format is uniform: `settings` is serialized by a `pkgs.formats` generator
# (plist included, via `pkgs.formats.plist`). Consumers add the key back as `name`.
{
  json = {
    fileExtension = "json";
    optionName = "managedJson";
    settingsExample = {
      theme = "dark";
      editor.fontSize = 14;
    };
  };
  yaml = {
    fileExtension = "yaml";
    optionName = "managedYaml";
    settingsExample = {
      theme = "dark";
      plugins = [ "git" ];
    };
  };
  toml = {
    fileExtension = "toml";
    optionName = "managedToml";
    settingsExample = {
      theme = "dark";
      editor.font_size = 14;
    };
  };
  plist = {
    fileExtension = "plist";
    optionName = "managedPlist";
    settingsExample = {
      NSGlobalDomain.AppleShowAllExtensions = true;
      recentItems = [
        "a"
        "b"
      ];
    };
  };
}
