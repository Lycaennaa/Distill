# Contributing

Bug reports, focused fixes, tests, and documentation improvements are welcome. For substantial behavior changes, open an issue first to agree on the scope and user-visible contract.

## Development setup

Distill targets macOS and the Rust version declared in `Cargo.toml`. Xcode, Swift, SwiftLint, and Cargo are needed only for the corresponding integration or smoke tests. Just is optional.

Run the standard checks from the repository root:

```sh
just check
```

The equivalent Cargo commands are:

```sh
cargo fmt --all -- --check
cargo test --locked --all-targets
cargo clippy --locked --all-targets -- -D warnings
```

`just build` and `just clippy` use an installed `distill` command when available; otherwise they call Cargo directly. Formatting and tests always use Cargo because Distill does not wrap those actions. `just package` validates a local package and allows a dirty worktree; release packaging should be checked from a clean tree.

## Pull requests

- Keep changes focused and explain the user-visible effect.
- Add or update tests for behavior changes, including relevant failure cases.
- Update README or policy documentation when command behavior, privacy, security, or support guidance changes.
- Run the checks above and state which checks were run.
- Do not include credentials, private logs, generated build output, or unrelated formatting changes.

The GitHub Actions workflow runs the macOS CI checks, including real Swift/Cargo smoke tests.