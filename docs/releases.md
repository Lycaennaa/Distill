# Release process

## CI and publishing

CI runs formatting, tests, Clippy, source packaging, and macOS smoke tests. It checks Xcode availability with `xcodebuild -version`; Xcode command behavior uses fake-tool tests because the repository has no Xcode project fixture.

The release workflow publishes matching `vX.Y.Z` tag pushes after CI succeeds. It can also be run manually from the default branch by entering a new matching tag; the workflow creates that tag on the selected commit after confirming push CI passed. It verifies tag targets and default-branch ancestry, resumes interrupted drafts, and checks archive and checksum-file digests before publishing Intel and Apple Silicon archives plus `SHA256SUMS`.

GitHub artifact attestations are generated only when the repository is public. Release notes combine the `Unreleased` section of [CHANGELOG.md](../CHANGELOG.md) with GitHub-generated notes and verification instructions. If using a `v*` tag ruleset, allow the GitHub Actions app to create tags while preventing updates and deletions.

## After each release

Move `Unreleased` entries to a versioned heading and start a fresh `Unreleased` section.
