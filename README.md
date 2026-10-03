# distill

`distill` is a local macOS CLI that wraps Xcode, Swift, SwiftLint, and Cargo commands and renders compact, deterministic digests instead of forwarding raw build output.

## Install

Install this checkout with Cargo:

```sh
cargo install --path . --locked
```

The package declares its version, description, license file, Rust minimum version, README, keywords, and crates.io categories in `Cargo.toml`.

## Commands and examples

```text
distill xcode build|test|list
distill swift build|test|lint
distill cargo build|clippy
```

Examples:

```sh
distill --plan swift build -- --configuration release
distill --timeout 5m cargo clippy -- --all-targets --all-features
distill --save-log ./build.log xcode build -- --scheme Demo
distill --open --stream xcode build
distill swift lint -- --config .swiftlint.yml
```

Wrapper options such as `--cwd`, `--timeout`, `--plan`, `--save-log`, and post-action flags belong before the child argument separator. Arguments after `--` go only to the selected tool; Distill never invokes a shell. Discovery starts at the current directory or `--cwd` and walks ancestors to select one Xcode project/workspace, Swift package, or Cargo manifest. Ambiguous or missing roots fail as wrapper errors. SwiftLint can run without a package. Explicit `-workspace`, `-project`, `--package-path`, and `--manifest-path` values are validated and suppress automatic selection.

Build and test adapters add quiet diagnostic flags unless a user verbosity or diagnostic-format option takes precedence. Xcode, compiler, lint, XCTest, and Cargo diagnostics are parsed into deterministic records. `xcode list` reports available project/workspace, target, configuration, and scheme sections. Test output includes recognized pass/fail/skipped counts and failed cases, not a list of successful cases.

## Output contract

Normal output is line-oriented. Wrapper records use `KIND:`; diagnostics use the compact `DIAG:` prefix.

```text
PLAN: payload
DIAG: payload
INFO: kind=binary state=unresolved
```

Payload control characters and backslashes are escaped. Reserved record kinds are `PLAN`, `DIAG`, `TEST`, `LIST`, `ACTION`, `INFO`, `RESULT`, `FAILURE`, `FALLBACK`, and `LOG`. `INFO` omits unavailable product/scheme/path/action values, shows state only while unresolved, and never prints the discovered root. `planned/unresolved` marks a dry run; `unresolved` means product metadata was not resolved; resolved info omits state. Child output is never copied raw to stdout. The final `RESULT` reports exit code, owner, duration, and lower-precedence statuses; it omits redundant `class=success`, while non-success results retain their class. A failing command with no recognized diagnostic emits a `FALLBACK` header and at most 20 sanitized lines; parsed diagnostics are not capped. `--stream` sends launched-product output to stderr; digest records remain on stdout.
Diagnostic paths under the selected command root are rendered relative to that root; paths outside it remain absolute.

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

When multiple statuses occur, precedence is timeout, interrupt, other wrapper failures, child failures, then success. Lower-precedence statuses remain available as `RESULT` metadata.

## Planning, privacy, and saved logs

`--plan` resolves filesystem-only command/root internally and artifact intent, but does not print the discovered root. It runs no child command or metadata command; product state that requires tool metadata is marked planned or unresolved. `--plan` ignores `--save-log` and creates no log file.

Wrapper-generated `PLAN` and `INFO` records do not disclose inherited environment values or unredacted child arguments. Child-produced diagnostic and fallback text is untrusted. Sanitization is best effort and can miss unknown sensitive text; non-Xcode saved logs and streamed child output do not promise exhaustive secrecy. Xcode redaction is mandatory and has no opt-out: signing identities become `[SIGNING_IDENTITY]`, team IDs `[TEAM_ID]`, provisioning profile identifiers/names `[PROFILE_ID]`, and email-like values `[EMAIL]`. This also applies to Xcode diagnostics, fallback text, post-action errors, saved logs, and streamed Xcode app output. Other local path text can remain visible.

`--save-log PATH` captures the complete merged output from the primary wrapped child, sanitizes it, then atomically publishes a new file. It does not turn the log into digest/stdout content. The `LOG` record reports the canonical destination, sanitized byte count, SHA-256 checksum, and redaction policy; it appears before the final `RESULT`. Existing paths, symlinks, and concurrent reservations fail rather than overwrite. Incomplete files are not published; a line larger than 4 MiB fails closed as `log_write`. The per-destination advisory lock file remains after completion for crash-safe coordination.

Every directory in the resolved destination ancestry must be owned by the invoking user or root, have no extended ACL, and not be group/world-writable unless the sticky bit is set. Parent symlinks are resolved before publication. After the publish/rename commit point, timeout or interrupt retains and reports the log.

## Build post-actions

`--open`, `--wait`, and `--stream` are valid only for `xcode build`. `--open` launches the built `.app`; detached launch uses macOS `open`. `--wait` runs the app executable and returns its exit status; `--stream` implies `--wait` and sends sanitized output to stderr. Swift and Cargo do not support launch post-actions.

Only `xcode build` accepts `--mv` and `--omv`. `--mv` moves the built app to `/Applications`; `--omv` moves it before opening. Existing destination apps are replaced through a rollback-capable backup transaction. Xcode runtime arguments are not supported.

## Platform and deliberate limits

Distill supports macOS only. It has no config file, telemetry, network behavior, JSON output, raw-log stdout escape hatch, arbitrary wrapper action, or warnings-as-errors mode. Digest output can grow with recognized diagnostics; only unknown-failure fallback output is capped. Cargo may access the network during installation to resolve dependencies, but Distill itself makes no network requests.

CI runs the unit and process-level fake-tool suites, formatting, Clippy, a release build, package/install checks, and real Swift/Cargo/Clippy smoke commands on macOS. `xcodebuild -version` verifies the available Xcode toolchain; Xcode command behavior is covered by deterministic fake-tool tests because the repository does not contain an Xcode project fixture.

## Project documentation

- [Changelog](CHANGELOG.md)
- [Contributing](CONTRIBUTING.md)
- [License](LICENSE)
- [Privacy](PRIVACY.md)
- [Security](SECURITY.md)
- [Support](SUPPORT.md)