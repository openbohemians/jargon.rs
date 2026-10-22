# Jargonaut - Jargon for Rust: define your CLI jargon with JSON Schema

# Default task
default: test

# Build the library
build:
  cargo build

# Run all tests, the conformance cases included
test:
  cargo test

# Run the linter
lint:
  cargo clippy --all-targets

# Format code
fmt:
  cargo fmt

# Copy in the conformance cases (record them there with `just conformance`)
sync path="../jargon":
  scripts/sync-conformance.sh {{path}}

# Generate and open the API documentation
doc:
  cargo doc --no-deps --open
