# Changelog

Notable user-visible changes are recorded here.

## Unreleased

- Bound Gradle JUnit aggregation by observed tasks, directory entries, and invocation-wide file/byte limits; incomplete summaries are omitted.
- Use configured Gradle subproject directories for JUnit reports and reject unresolved mappings and paths outside the project root.
- Preserve timeout and interrupt results when Gradle report collection stops.
- Run arbitrary Gradle tasks through the project's wrapper. The new public `Tool::Gradle` and `RootKind::GradleWrapper` variants are source-breaking.
- Document Cargo installation from Git, including tag pinning.
- Add Cargo xtask support. The public `Action::Xtask` variant is source-breaking, requiring major version 2.1.0.
- Add Cargo install support with validated local `--path` discovery.
- Add a Windows x64 GNU release package; Xcode actions remain macOS-only.
- Allow untrusted `FILE_ADD_SUBDIRECTORY` access on non-final Windows log-path ancestors while keeping the final destination parent and private log ACLs strict.
