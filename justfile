_default:
    @just --list

# Quick check of the code including lints and formatting.
check:
  cargo fmt --check
  # Turn warnings into errors.
  cargo clippy --all-targets -- -D warnings
  cargo check --all-features

# Run a test suite: unit, msrv, or min-versions.
test suite="unit":
  just _test-{{suite}}

# Unit test suite.
_test-unit:
  cargo test --all-targets
  cargo test --doc

# Check code with MSRV compiler.
_test-msrv:
  # Handles creating sandboxed environments to ensure no newer binaries sneak in.
  cargo install cargo-msrv@0.18.4
  cargo msrv verify --all-features

# Test that minimum versions of dependency contraints are valid.
_test-min-versions:
  rm -f Cargo.lock
  cargo +nightly check --all-features -Z direct-minimal-versions

# Add a release tag and publish to the remote. Need write privileges on the repository.
tag version remote="upstream":
  echo "Adding release tag {{version}} and pushing to {{remote}}..."
  git tag -a {{version}} -m "Release {{version}}"
  git push {{remote}} {{version}}

# Run the crawler example with given seed node.
crawl ip port="8333" log="info":
    cargo run --example crawler -- --address {{ip}} --port {{port}} --log-level {{log}}
