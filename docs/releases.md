# Release process

## CI and publishing

CI runs the full Rust checks and a built-CLI smoke path natively on Linux, as well as macOS tests/smoke and a Windows GNU cross-build. The release workflow publishes Apple Silicon and Intel macOS archives, an x86_64 Linux GNU archive, and the x64 Windows GNU executable.

The `vX.Y.Z` release workflow verifies the tag against the default branch, builds assets, and creates `SHA256SUMS` covering every binary archive/executable. Release notes record each asset hash and the checksum-file hash; public release assets receive GitHub build provenance.

GitHub-generated release notes are combined with the `Unreleased` section from [CHANGELOG.md](../CHANGELOG.md). See [SUPPORT.md](../SUPPORT.md) for supported platforms and known limitations.

## After each release

Move `Unreleased` entries to a versioned heading and start a fresh `Unreleased` section.
