# Release a new version: just release-patch|release-minor|release-major
# (dry-run first with `cargo release <bump>` - no --execute - to preview)
_release bump:
  cargo release --execute {{bump}}
