# distill

`distill` wraps Xcode, Swift, SwiftLint, and Cargo on macOS, turning tool output into compact, deterministic digests.

## Install

Download the matching archive from [GitHub Releases](https://github.com/lycaennaa/distill/releases): Apple Silicon uses `aarch64-apple-darwin`, Intel uses `x86_64-apple-darwin`. Extract `distill` and put it on your `PATH`. Binaries are unsigned and not notarized; Gatekeeper may warn.

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
distill cargo build|test|fmt|package|clippy
```

```sh
distill swift build
distill cargo test --all-targets
distill xcode build --scheme Demo
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
