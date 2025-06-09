_default:
    @just --list

# Quick check of the code including lints and formatting.
check toolchain="nightly":
  cargo +{{toolchain}} fmt --check
  # Turn warnings into errors.
  cargo +{{toolchain}} clippy --all-targets -- -D warnings
  cargo +{{toolchain}} check --all-features

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

# Try an example: connection, crawler.
try example ip port="8333" log="info":
  just _try-{{example}} {{ip}} {{port}} {{log}}

# Run the crawler example with given seed node.
_try-crawler ip port="8333" log="info":
    cargo run --example crawler -- --address {{ip}} --port {{port}} --log-level {{log}}
 
# Run the conncetion example to the given address.
_try-connection ip port="8333" log="info":
    cargo run --example connection -- --address {{ip}} --port {{port}} --log-level {{log}}
 
# Publish a new version. Requires write privileges on upsream repository and crates.io.
publish version remote="upstream":
  @if ! git diff --quiet || ! git diff --cached --quiet; then \
    echo "publish: Uncommitted changes"; exit 1; fi
  @if [ "$$(git rev-parse --abbrev-ref HEAD)" != "master" ]; then \
    echo "publish: Not on master branch"; exit 1; fi
  @if ! grep -q "## v$(VERSION)" CHANGELOG.md; then \
    echo "publish: CHANGELOG.md entry missing $(VERSION)"; exit 1; fi
  @if ! grep -q '^version = "$(VERSION)"' Cargo.toml; then \
    echo "publish: Cargo.toml version mismatch"; exit 1; fi
  @echo "publish: Adding release tag {{version}} and pushing to {{remote}}..."
  @git tag -a {{version}} -m "Release {{version}}"
  @git push {{remote}} {{version}}
