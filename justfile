# Every commit on the master branch is expected to have working `check` and `test-*` recipes.
#
# The recipes make heavy use of `rustup`'s toolchain syntax (e.g. `cargo +nightly`). `rustup` is
# require to be installed on the system in order to intercept the `cargo` commands and
# to install and use the appropriate toolchain with components. 

NIGHTLY_TOOLCHAIN := "nightly-2025-06-10"

@_default:
    just --list

# Quick check of the code including format and lint rules. 
@check toolchain=NIGHTLY_TOOLCHAIN:
  # Default to the nightly toolchain for modern format and lint rules.

  # Ensure the toolchain is installed and has the necessary components.
  rustup component add --toolchain {{toolchain}} rustfmt clippy
  # Cargo's wrapper for rustfmt predates workspaces, so uses the "--all" flag instead of "--workspaces".
  cargo +{{toolchain}} fmt --check --all
  # Lint all workspace members. Enable all feature flags. Check all targets (tests, examples) along with library code. Turn warnings into errors.
  cargo +{{toolchain}} clippy --workspace --all-features --all-targets -- -D warnings
  # Checking the extremes: all features enabled as well as none. If features are additive, this should expose conflicts.
  # If non-additive features (mutually exclusive) are defined, more specific commands are required.
  cargo +{{toolchain}} check --workspace --no-default-features --all-targets
  cargo +{{toolchain}} check --workspace --all-features --all-targets

# Run a test suite: unit, msrv, or constraints.
@test suite="unit":
  just _test-{{suite}}

# Unit test suite.
@_test-unit:
  # Virtual workspace (no code in root) doesn't require the "--workspace" flag, but just being explicit.
  # "--all-features" for highest coverage, assuming features are additive so no conflicts.
  # "--all-targets" runs `lib` (unit, integration), `bin`, `test`, `bench`, `example` tests, but not doc code. 
  cargo test --workspace --all-features --all-targets
  cargo test --workspace --all-features --doc

# Check code with MSRV compiler.
@_test-msrv:
  # Handles creating sandboxed environments to ensure no newer binaries sneak in.
  cargo install cargo-msrv@0.18.4
  # MSRV tool doesn't have cargo's workspace flag behavior yet, need to verify each package separately.
  cargo msrv verify --manifest-path connection/Cargo.toml --all-features
  cargo msrv verify --manifest-path crawler/Cargo.toml --all-features

# Test that minimum versions of dependency contraints are valid.
@_test-constraints:
  rm -f Cargo.lock
  # Skipping "--all-targets" since tests and examples are not relevant for a library consumer.
  cargo +{{NIGHTLY_TOOLCHAIN}} check --workspace --all-features -Z direct-minimal-versions

# Try an example: connection, crawler.
@try example ip port="8333" log="info":
  just _try-{{example}} {{ip}} {{port}} {{log}}

# Run the crawler example with given seed node.
@_try-crawler ip port="8333" log="info":
    cargo run -p bitcoin-peers-crawler --example crawler -- --address {{ip}} --port {{port}} --log-level {{log}}
 
# Run the conncetion example to the given address.
@_try-connection ip port="8333" log="info":
    cargo run -p bitcoin-peers-connection --example connection -- --address {{ip}} --port {{port}} --log-level {{log}}
 
# Publish a new version. Requires write privileges on upsream repository and crates.io.
@publish crate version remote="upstream":
  # Publish guardrails: be on a clean master, updated changelog, updated manifest.
  if ! git diff --quiet || ! git diff --cached --quiet; then \
    echo "publish: Uncommitted changes"; exit 1; fi
  if [ "`git rev-parse --abbrev-ref HEAD`" != "master" ]; then \
    echo "publish: Not on master branch"; exit 1; fi
  if ! grep -q "## v{{version}}" {{crate}}/CHANGELOG.md; then \
    echo "publish: CHANGELOG.md entry missing for v{{version}}"; exit 1; fi
  if ! grep -q '^version = "{{version}}"' {{crate}}/Cargo.toml; then \
    echo "publish: Cargo.toml version mismatch"; exit 1; fi
  # Final confirmation.
  printf "Publishing {{crate}} v{{version}}, do you want to continue? [y/N]: "; \
  read -r response; \
  # Exit 1 to kill the script.
  [ "$response" = "y" ] || [ "$response" = "Y" ] || { echo "publish: Cancelled"; exit 1; }
  # Publish the tag.
  echo "publish: Adding release tag {{crate}}-v{{version}} and pushing to {{remote}}..."
  # Using "-a" annotated tag over a lightweight tag for robust history.
  git tag -a {{crate}}-v{{version}} -m "Release {{crate}} v{{version}}"
  git push {{remote}} {{crate}}-v{{version}}
  # Publish the crate.
  cargo publish -p {{crate}}
