# macOS preference-domain option, shared by the home and system platforms (plist
# only). Reconciling through `cfprefsd` (`defaults`/`plutil`) is macOS-only; each
# platform supplies its own `description`, and `build` asserts a Darwin host when
# it's set.
lib: description:
lib.mkOption {
  type = lib.types.nullOr lib.types.str;
  default = null;
  example = "com.example.app";
  inherit description;
}
