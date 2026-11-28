# Per-format descriptors, keyed by the format name. The key is also the
# `pkgs.formats.<name>` generator and the config-graft subcommand name, so
# every format is uniform: `settings` is serialized by a `pkgs.formats` generator
# (plist included, via `pkgs.formats.plist`). Consumers add the key back as `name`.
#
# `emptyDocument` is the empty DESIRED the prune of a removed entry reconciles
# against. It is a literal rather than the generator's output, so a consumer with
# no entry of a format does not build that generator (remarshal, for TOML).
{
  json = {
    fileExtension = "json";
    emptyDocument = "{}";
    optionName = "managedJson";
    settingsExample = {
      theme = "dark";
      editor.fontSize = 14;
    };
  };
  yaml = {
    fileExtension = "yaml";
    emptyDocument = "{}\n";
    optionName = "managedYaml";
    settingsExample = {
      theme = "dark";
      plugins = [ "git" ];
    };
  };
  toml = {
    fileExtension = "toml";
    emptyDocument = "";
    optionName = "managedToml";
    settingsExample = {
      theme = "dark";
      editor.font_size = 14;
    };
  };
  plist = {
    fileExtension = "plist";
    emptyDocument = ''
      <?xml version="1.0" encoding="UTF-8"?>
      <!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
      <plist version="1.0">
      <dict/>
      </plist>
    '';
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
