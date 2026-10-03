# Agent guidance

This Rust workspace builds a CLI and GUI for Brother P-Touch printers. Keep rendering and protocol changes in their owning crates. For printer-model or layout behavior, consult the README and affected crate before changing shared behavior.

The CI checks are `cargo fmt --all -- --check`, `cargo check --workspace`, `cargo clippy --workspace -- -D warnings`, and `cargo test --workspace`. Unit tests use local protocol, rendering, and state fixtures; they do not open a USB printer. Run the relevant local checks for code changes. Do not run CLI or GUI actions that enumerate a connected printer, feed or cut tape, or print labels as validation; those actions require an explicit request. Release packaging is triggered by version tags, so do not create or push a release tag as part of routine changes.
