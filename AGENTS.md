# Agent guidance

## Project constraints

- Distill is a macOS-only Rust CLI. Respect the declared Rust minimum and existing command/output contracts.
- Preserve deterministic records, stdout/stderr separation, exit-status ownership, cancellation, and privacy behavior documented in `README.md`, `PRIVACY.md`, and `SECURITY.md`.
- Launch wrapped tools directly; do not introduce shell execution. Keep tests for behavior changes, including failure paths; use fake tools for command-adapter coverage.
- Keep changes focused. Update user-facing docs when behavior, supported tools, privacy, or security guarantees change.

## Validation

- `cargo fmt --all -- --check`
- `cargo test --locked --all-targets`
- `cargo clippy --locked --all-targets -- -D warnings`
- `cargo package --locked` for a clean-tree package check; use `--allow-dirty` only for local validation.
- `just check` runs formatting, tests, and Clippy when Just is installed. `just build` and `just clippy` use `distill` when available and fall back to Cargo.

CI's real Swift/Cargo smoke tests run on macOS. See `.github/workflows/ci.yml` for the full pipeline.