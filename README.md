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

## Commands and examples

```text
distill xcode build|test|list
distill swift build|test|lint
distill cargo build|test|fmt|package|clippy
```

Examples:

```sh
distill --plan swift build --configuration release
distill --timeout 5m cargo clippy --all-targets --all-features
distill cargo fmt --check
distill cargo test --all-targets
distill cargo test --locked --offline --all-targets
distill cargo package --locked --allow-dirty
distill cargo test --nightly --all-targets
distill --save-log ./build.log xcode build --scheme Demo
distill --open --stream xcode build
distill swift lint --config .swiftlint.yml
```

Pass wrapped-tool arguments directly after the action. Use `--` when an argument conflicts with a Distill option and must be forwarded instead. Distill launches tools directly, without a shell.

Discovery searches the current directory (`--cwd`) and its ancestors for an Xcode project/workspace, Swift package, or Cargo manifest. Missing or ambiguous roots fail. SwiftLint needs no package. Explicit `-workspace`, `-project`, `--package-path`, and `--manifest-path` values are validated and disable automatic discovery.

Cargo build/test/Clippy add `--quiet` unless verbosity is requested and `--message-format=json-diagnostic-rendered-ansi` unless another message format is supplied. Cargo package adds only `--quiet` unless verbosity is requested; Cargo fmt adds no flags. Pass Distill's `--nightly` flag to set `RUSTUP_TOOLCHAIN=nightly-aarch64-apple-darwin` for any Cargo action. Distill parses compiler, lint, XCTest, and Cargo output into deterministic records. Cargo test summaries include pass/fail/skipped counts; recognized test failures include failed cases. `xcode list` reports projects/workspaces, targets, configurations, and schemes. Test summaries omit successful case details.

## Output contract

Normal output is line-oriented. Wrapper records use `KIND:`; diagnostics use the compact `DIAG:` prefix.

```text
PLAN: payload
DIAG: payload
INFO: kind=binary state=unresolved
```

Payload control characters and backslashes are escaped. Record kinds: `PLAN`, `DIAG`, `TEST`, `LIST`, `ACTION`, `INFO`, `RESULT`, `FAILURE`, `FALLBACK`, and `LOG`.

- `INFO` omits unavailable metadata and the discovered root. `planned/unresolved` marks a dry run; `unresolved` means product metadata is unknown. Resolved info omits `state`.
- Child output is never copied raw to stdout.
- `RESULT` reports exit code, owner, duration, and lower-precedence statuses. Success omits `class=success`; failures include their class.
- A failing command without recognized diagnostics emits `FALLBACK` with up to 20 sanitized lines. Parsed diagnostics are not capped.
- `--stream` sends launched-app output to stderr; digest records remain on stdout.
- Paths inside the command root are relative; paths outside it remain absolute.

### Result classes and exit codes

| Class | Owner | Exit code |
| --- | --- | --- |
| `success` | child | `0` |
| `compile`, `test`, `lint`, `runtime` | child | Child exit code; `0` is normalized to `success` |
| `signaled` | child | `128 + signal` |
| `timeout` | wrapper | `124` |
| `interrupt` | wrapper | `130` |
| `tool_missing`, `launch` | wrapper | `127` |
| `usage` | wrapper | `2` |
| `discovery`, `artifact`, `post_action`, `log_write`, `generic_wrapper` | wrapper | `1` |

Status precedence is timeout, interrupt, wrapper failure, child failure, then success. Lower-precedence statuses remain in `RESULT` metadata.

## Planning, privacy, and saved logs

`--plan` resolves command, root, and artifact intent from the filesystem. It runs no child or metadata command; product state requiring tool metadata is marked planned or unresolved. `--plan` ignores `--save-log` and creates no log file.

`PLAN` and `INFO` never expose inherited environment values or unredacted child arguments. Child diagnostics and fallback text are untrusted; sanitization can miss unknown sensitive text. Non-Xcode saved logs and streamed output aren't guaranteed secret-free.

Xcode redaction is mandatory: signing identities become `[SIGNING_IDENTITY]`, team IDs `[TEAM_ID]`, provisioning profile IDs/names `[PROFILE_ID]`, and email-like values `[EMAIL]`. Redaction applies to Xcode diagnostics, fallback text, post-action errors, saved logs, and streamed app output. Other path text may remain visible.

`--save-log PATH` captures the primary child's complete merged output, sanitizes it, and atomically publishes a new file. Log contents never go to stdout. `LOG` reports the canonical path, sanitized byte count, SHA-256, and redaction policy before `RESULT`.

Existing paths, symlinks, and concurrent reservations fail rather than overwrite. Incomplete logs aren't published; lines over 4 MiB fail with `log_write`. The per-destination lock file remains for crash-safe coordination.

Each destination ancestor must be owned by the invoking user or root, have no extended ACL, and not be group/world-writable unless sticky. Parent symlinks are resolved. After publication, timeout or interrupt retains and reports the log.

## Build post-actions

Only `xcode build` supports post-actions. `--open` launches the built app; detached launch uses macOS `open`. `--wait` runs the app executable and returns its status; `--stream` implies `--wait` and sends sanitized output to stderr. `--mv` moves the app to `/Applications`; `--omv` moves it before opening. Replacing an existing destination uses a rollback-capable backup. Swift and Cargo have no post-actions; Xcode runtime arguments are unsupported.

## Platform and deliberate limits

Distill supports macOS only. It has no config, telemetry, network requests, JSON output, raw-log stdout mode, arbitrary wrapper actions, or warnings-as-errors mode. Recognized diagnostic output is unbounded; fallback output is capped. Cargo may access the network during installation; Distill itself makes no network requests.

CI runs formatting, tests, Clippy, source packaging, and macOS smoke tests. The release workflow publishes matching `vX.Y.Z` tag pushes after CI succeeds, and can also be run manually from the default branch by entering a new matching tag; the workflow creates that tag on the selected commit after confirming push CI passed. It verifies tag targets and default-branch ancestry, resumes interrupted drafts, and checks archive and checksum-file digests before publishing Intel and Apple Silicon archives plus `SHA256SUMS`. GitHub artifact attestations are generated only when the repository is public. Release notes combine the `Unreleased` section of `CHANGELOG.md` with GitHub-generated notes and verification instructions. If using a `v*` tag ruleset, allow the GitHub Actions app to create tags while preventing updates and deletions.

After each release, move `Unreleased` entries to a versioned heading and start a fresh `Unreleased` section. CI checks Xcode availability with `xcodebuild -version`; Xcode command behavior uses fake-tool tests because the repository has no Xcode project fixture.

## Project documentation

- [Changelog](CHANGELOG.md)
- [Contributing](CONTRIBUTING.md)
- [License](LICENSE)
- [Privacy](PRIVACY.md)
- [Security](SECURITY.md)
- [Support](SUPPORT.md)