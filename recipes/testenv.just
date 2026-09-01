# Scratch dir for testenv-* recipes - an isolated XDG_CONFIG_HOME and
# XDG_DATA_HOME so testing never touches the real ~/.config/dispatchd or
# ~/.local/share/dispatchd (config, members roster, and the SQLite DB).
testenv_dir := justfile_directory() / ".testenv"
testenv_env := "XDG_CONFIG_HOME=" + testenv_dir / "config" + " XDG_DATA_HOME=" + testenv_dir / "data"

# Scaffold config.toml + members.toml templates under .testenv
testenv-init:
  {{ testenv_env }} cargo run -- init

# Run dispatchd against the .testenv config/db instead of your real one
testenv-run *args:
  {{ testenv_env }} cargo run -- {{ args }}

# Wipe the .testenv scratch dir
testenv-clean:
  rm -rf {{ testenv_dir }}
