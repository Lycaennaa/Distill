# Usage

## Commands

```text
distill xcode build|test|list
distill swift build|test|lint
distill cargo build|test|fmt|package|install|clippy|xtask
distill gradle <TASK> [ARG...]
```

Gradle tasks run through the nearest project's wrapper, discovered from the current directory or `--cwd`. Distill forwards the task and remaining arguments to the wrapper.
On macOS Distill executes `gradlew` directly. On Windows it invokes `gradle/wrapper/gradle-wrapper.jar` through `java`, using `JAVA_HOME` or a `java` executable on `PATH`.

Gradle test actions emit JUnit totals and up to 100 failure details. Distill uses the standard report location or recognizes literal `file('relative/path')` and `new File(rootDir, 'relative/path')` `projectDir` assignments in `settings.gradle`/`settings.gradle.kts`. It rejects report paths outside the selected root and omits summaries for dynamic, unrecognized, or ambiguous mappings rather than risking stale or sibling reports. Collection is omitted rather than partial if interrupted or if reports/settings are malformed or unreadable, or if limits are exceeded: 4,096 XML files, 65,536 report-directory entries, 64 MiB per report, 256 MiB total, or 1 MiB per settings file.

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
distill --cwd app gradle :app:assembleDebug
```

## Xcode build post-actions

Only `xcode build` supports post-actions. `--open` launches the built app; detached launch uses macOS `open`. `--wait` runs the app executable and returns its status; `--stream` implies `--wait` and sends sanitized output to stderr. `--mv` moves the app to `/Applications`; `--omv` moves it before opening. Replacing an existing destination uses a rollback-capable backup. Swift and Cargo have no post-actions; Xcode runtime arguments are unsupported.

## Supported scope

Distill runs on macOS and Windows x64 (GNU); Xcode actions require macOS. It has no config, telemetry, JSON output, raw-log stdout mode, arbitrary custom Distill actions, or warnings-as-errors mode. Recognized diagnostic output is unbounded; fallback output is capped. Cargo installation and dependency resolution, and Gradle wrapper downloads and dependency resolution, may access the network; Distill itself makes no requests.
