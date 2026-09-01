test:
  cargo test

build:
  cargo build

fmt:
  cargo fmt

install:
  cargo install --path . --force

run *args:
  cargo run -- {{ args }}

# The four checks CI gates on (.github/workflows/ci.yml) - run before pushing
check:
  cargo fmt --check
  cargo clippy --all-targets -- -D warnings
  cargo test
  cargo build --release
