# distill

`distill` produces compact, deterministic digests while wrapping Xcode, Swift, SwiftLint, Cargo, and Gradle. It runs on macOS and Windows x64.

Running the tool removes everything that was generated leaving left useful things like "build success" or "build fail" or "10 tests run, 8 pass, 2 failed, 0 skipped". Anything that isn't a success will tell you the errors and other issues. If you need to know what happened, Distill has a save log argument.

## Install

Download the matching release asset from [GitHub Releases](https://github.com/lycaennaa/distill/releases): Apple Silicon uses `aarch64-apple-darwin`, Intel uses `x86_64-apple-darwin`, and Windows x64 downloads use the `.exe` asset directly. macOS assets are `.tar.gz` archives; extract the binary and put it on your `PATH`. Downloads are unsigned; macOS binaries are not notarized and Gatekeeper may warn.

Install from the repository's default branch:

```sh
cargo install --git https://github.com/lycaennaa/distill --locked
```

Pin to a release tag (replace `v1.0.0` with the desired version):

```sh
cargo install --git https://github.com/lycaennaa/distill --tag v1.0.0 --locked
```

Or install this checkout from source:

```sh
cargo install --path . --locked
```

Building from source requires Rust 1.94+.

## Quick start

```text
distill xcode build|test|list
distill swift build|test|lint
distill cargo build|test|fmt|package|install|clippy|xtask
distill gradle <TASK> [ARG...]
```

Examples:
```sh
distill swift build
distill cargo test --all-targets
distill cargo install --path . --locked
distill xcode build --scheme Demo
distill gradle :app:assembleDebug
```

See [Usage](docs/usage.md) for command options, discovery, and action-specific behavior.

## Documentation

- [Usage](docs/usage.md)
- [Output contract](docs/output.md)
- [Privacy and saved logs](PRIVACY.md)
- [Release process](docs/releases.md)
- [Changelog](CHANGELOG.md)
- [Contributing](CONTRIBUTING.md)
- [License](LICENSE)
- [Security](SECURITY.md)
- [Support](SUPPORT.md)
