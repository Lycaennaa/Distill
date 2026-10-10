# Usage

## Commands

```text
distill xcode build|test|list
distill swift build|test|lint
distill cargo build|test|fmt|package|install|clippy|xtask
```

Pass wrapped-tool arguments directly after the action. Use `--` when an argument conflicts with a Distill option and must be forwarded instead. Distill launches tools directly, without a shell.

Xcode, Swift, and Cargo root-based actions search the selected directory (`--cwd`) and its ancestors for a project or package manifest; missing or ambiguous roots fail. SwiftLint and Cargo install without `--path` use the selected working directory. Explicit `-workspace`, `-project`, `--package-path`, and `--manifest-path` values are validated and disable automatic discovery.

Cargo build/test/Clippy/install add `--quiet` unless verbosity is requested and `--message-format=json-diagnostic-rendered-ansi` unless another message format is supplied. Cargo package adds only `--quiet` unless verbosity is requested; Cargo fmt and xtask add no flags. Place Distill's `--nightly` before Cargo arguments and any `--` separator to select the host's nightly toolchain for any Cargo action.

Cargo install supports registry/Git installs from the selected working directory. With `--path`, Distill validates and selects the local Cargo package directory before launching Cargo.

Cargo xtask forwards its task arguments unchanged and adds no automatic flags. It requires a configured Cargo `xtask` alias or an installed `cargo-xtask` executable; see [cargo-xtask](https://github.com/matklad/cargo-xtask) for the alias pattern.

## Examples

```sh
distill --plan swift build --configuration release
distill --timeout 5m cargo clippy --all-targets --all-features
distill cargo fmt --check
distill cargo test --all-targets
distill cargo test --locked --offline --all-targets
distill cargo package --locked --allow-dirty
distill cargo install --path . --locked
distill cargo test --nightly --all-targets
distill cargo xtask ci
distill --save-log ./build.log xcode build --scheme Demo
distill --open --stream xcode build
distill swift lint --config .swiftlint.yml
```

## Xcode build post-actions

Only `xcode build` supports post-actions. `--open` launches the built app; detached launch uses macOS `open`. `--wait` runs the app executable and returns its status; `--stream` implies `--wait` and sends sanitized output to stderr. `--mv` moves the app to `/Applications`; `--omv` moves it before opening. Replacing an existing destination uses a rollback-capable backup. Swift and Cargo have no post-actions; Xcode runtime arguments are unsupported.

## Supported scope

Distill supports macOS only. It has no config, telemetry, JSON output, raw-log stdout mode, arbitrary wrapper actions, or warnings-as-errors mode. Recognized diagnostic output is unbounded; fallback output is capped. Cargo may access the network during installation; Distill itself makes no network requests.
