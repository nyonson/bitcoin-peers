# Every commit on the master branch is expected to have working `check` and `test-*` recipes.
#
# The recipes make heavy use of `rustup`'s toolchain syntax (e.g. `cargo +nightly`). `rustup` is
# required on the system in order to intercept the `cargo` commands and to install and use the appropriate toolchain with components. 

# Explicit toolchain versions per hash for a chance at determinitic builds.
# Versions can be found at https://github.com/rust-lang/rust/releases
NIGHTLY_TOOLCHAIN := "nightly-2025-07-10"
STABLE_TOOLCHAIN := "1.88.0"

@_default:
    just --list

# Quick check including lints and formatting. Run "fix" mode for auto-fixes.
@check mode="verify":
  # Use nightly toolchain for modern format and lint rules.
  # Ensure the toolchain is installed and has the necessary components.
  rustup component add --toolchain {{NIGHTLY_TOOLCHAIN}} rustfmt clippy
  just _check-{{mode}}

# Verify check, fails if anything is off. Good for CI.
@_check-verify:
  # Cargo's wrapper for rustfmt predates workspaces, so uses the "--all" flag instead of "--workspaces".
  cargo +{{NIGHTLY_TOOLCHAIN}} fmt --check --all
  # Lint all workspace members. Enable all feature flags. Check all targets (tests, examples) along with library code. Turn warnings into errors.
  cargo +{{NIGHTLY_TOOLCHAIN}} clippy --workspace --all-features --all-targets -- -D warnings
  # Static analysis of types and lifetimes.
  # Nightly toolchain required by benches target.
  cargo +{{NIGHTLY_TOOLCHAIN}} check --workspace --all-features --all-targets
  # Build documentation to catch any broken doc links or invalid rustdoc.
  RUSTDOCFLAGS="-D warnings" cargo +{{STABLE_TOOLCHAIN}} doc --workspace --all-features --no-deps

# Attempt any auto-fixes for format and lints.
@_check-fix:
  # No --check flag to actually apply formatting.
  cargo +{{NIGHTLY_TOOLCHAIN}} fmt --all
  # Adding --fix flag to apply suggestions with --allow-dirty.
  cargo +{{NIGHTLY_TOOLCHAIN}} clippy --workspace --all-features --all-targets --fix --allow-dirty -- -D warnings

# Run a test suite: features, msrv, or constraints.
@test suite="features":
  just _test-{{suite}}

# Unit test suite.
@_test-features:
  # Virtual workspace (no code in root) doesn't require the "--workspace" flag, but just being explicit.
  # "--all-features" for highest coverage, assuming features are additive so no conflicts.
  # "--all-targets" runs `lib` (unit, integration), `bin`, `test`, `bench`, `example` tests, but not doc code. 
  cargo +{{STABLE_TOOLCHAIN}} test --workspace --no-default-features --all-targets
  cargo +{{STABLE_TOOLCHAIN}} test --workspace --all-features --all-targets
  # Docs needs a separate compilation.
  cargo +{{STABLE_TOOLCHAIN}} test --workspace --all-features --doc

# Check the MSRV.
@_test-msrv:
  # Ensure that the code works with the stated minimum supported rust version (MSRV).
  # This implicitly relys on using the V3 resolver which is MSRV-aware.
  # Handles creating sandboxed environments to ensure no newer binaries sneak in.
  cargo install cargo-msrv@0.18.4
  # MSRV tool doesn't have cargo's workspace flag behavior yet, need to verify each package separately.
  cargo msrv verify --manifest-path connection/Cargo.toml --all-features
  cargo msrv verify --manifest-path crawler/Cargo.toml --all-features

# Check minimum and maximum dependency contraints.
@_test-constraints:
  # Ensure that the workspace code works with dependency versions at both extremes. This checks
  # that we are not unintentionally using new feautures of a dependency or removed ones.
  # Skipping "--all-targets" for these checks since tests and examples are not relevant for a library consumer.
  # Enabling "--all-features" so all dependencies are checked.
  # Clear any previously resolved versions and re-resolve to the minimums.
  rm -f Cargo.lock
  cargo +{{NIGHTLY_TOOLCHAIN}} check --workspace --all-features -Z direct-minimal-versions
  # Clear again and check the maximums by ignoring any rust-version caps. 
  rm -f Cargo.lock
  cargo +{{NIGHTLY_TOOLCHAIN}} check --workspace --all-features --ignore-rust-version
  rm -f Cargo.lock

# Debug single test.
@debug test:
  cargo +{{STABLE_TOOLCHAIN}} test --workspace --all-features {{test}}

# Try an example: connection, crawler.
@try example ip port="8333" log="info":
  just _try-{{example}} {{ip}} {{port}} {{log}}

# Run the crawler example with given seed node.
@_try-crawler ip port="8333" log="info":
    cargo +{{STABLE_TOOLCHAIN}} run -p bitcoin-peers-crawler --example crawler -- --address {{ip}} --port {{port}} --log-level {{log}}
 
# Run the conncetion example to the given address.
@_try-connection ip port="8333" log="info":
    cargo +{{STABLE_TOOLCHAIN}} run -p bitcoin-peers-connection --example connection -- --address {{ip}} --port {{port}} --log-level {{log}}
 
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
  # Final confirmation, exit 1 is used to kill the script.
  printf "Publishing {{crate}} v{{version}}, do you want to continue? [y/N]: "; \
  read response; \
  case "$response" in \
    [yY]) ;; \
    *) echo "publish: Cancelled"; exit 1 ;; \
  esac
  # Publish the tag.
  echo "publish: Adding release tag {{crate}}-v{{version}} and pushing to {{remote}}..."
  # Using "-a" annotated tag over a lightweight tag for robust history.
  git tag -a {{crate}}-v{{version}} -m "Release {{crate}} v{{version}}"
  git push {{remote}} {{crate}}-v{{version}}
  # Publish the crate.
  cargo publish -p bitcoin-peers-{{crate}}
