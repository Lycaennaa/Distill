# Privacy

This document covers the Distill CLI. GitHub may separately collect information when you browse the repository or use GitHub features.

## Data and network behavior

Distill runs locally and has no telemetry or network requests of its own. It launches the selected local tool (such as Xcode, Swift, SwiftLint, or Cargo) with the invoking user's environment. Those tools may access the network, read local files, execute build or test scripts, and create their own caches. Cargo may access the network while installing or resolving dependencies.

`--plan` resolves command, root, and artifact intent from the filesystem. It does not launch a child or metadata command, and it ignores `--save-log` without creating a log. Product state that requires tool metadata is marked planned or unresolved. Wrapper-generated `PLAN` and `INFO` records do not disclose inherited environment values or unredacted child arguments; `INFO` does not print the discovered root.

## Output and saved logs

Child output is untrusted. Paths inside the selected command root are rendered relative to that root; paths outside it may remain absolute. Other local path text may also appear in diagnostics and logs. Sanitization may miss unknown sensitive text, so output and logs are not guaranteed secret-free; review them before sharing.

`--save-log` captures the primary child's complete merged output, sanitizes it, and atomically publishes a new file. Log contents never go to stdout. The `LOG` record reports the canonical destination, sanitized byte count, SHA-256, and redaction policy before `RESULT`.

Xcode redaction is mandatory: signing identities become `[SIGNING_IDENTITY]`, team IDs `[TEAM_ID]`, provisioning profile IDs/names `[PROFILE_ID]`, and email-like values `[EMAIL]`. Redaction applies to Xcode diagnostics, fallback text, post-action errors, saved logs, and streamed app output. Other path text may remain visible. Non-Xcode saved logs are only best-effort sanitized.

Existing paths, symlinks, and concurrent reservations fail rather than overwrite. Incomplete logs are not published; lines over 4 MiB fail with `log_write`. The per-destination lock file remains for crash-safe coordination.

Each destination ancestor must be owned by the invoking user or root, have no extended ACL, and not be group/world-writable unless sticky. Parent symlinks are resolved. After publication, timeout or interrupt retains and reports the log. Saved logs and lock files remain on the user's filesystem until removed.

`--stream` is limited to launched Xcode build products; streamed Xcode output is subject to Xcode redaction. Distill does not provide a raw-output escape hatch.

## User choices

Use `--plan` to inspect a planned invocation without running the wrapped tool. Avoid saving or sharing output that contains sensitive project details, and inspect any saved log before sending it to maintainers.
