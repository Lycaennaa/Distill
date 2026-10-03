# Privacy

This document covers the Distill CLI. GitHub may separately collect information when you browse the repository or use GitHub features.

## Data and network behavior

Distill runs locally and has no telemetry or network requests of its own. It launches the selected local tool (such as Xcode, Swift, SwiftLint, or Cargo) with the invoking user's environment. Those tools may access the network, read local files, execute build or test scripts, and create their own caches. Cargo may access the network while installing or resolving dependencies.

`--plan` performs filesystem discovery but does not launch a child or metadata command, and it does not create a saved log. Wrapper-generated `PLAN` and `INFO` records do not disclose inherited environment values or unredacted child arguments; `INFO` does not print the discovered root.

## Output and saved logs

Child output is untrusted. Paths inside the selected command root are rendered relative to that root; paths outside it may remain absolute. Other local path text may also appear in diagnostics and logs.

`--save-log` captures the primary child’s merged output and publishes a sanitized log. Sanitization is best effort for non-Xcode commands and may miss unknown sensitive text. Xcode redaction is mandatory and covers signing identities, team IDs, provisioning profile identifiers/names, and email-like values. This is not a guarantee that logs contain no secrets. Review logs before sharing them.

The `LOG` record includes the canonical destination, sanitized byte count, checksum, and redaction policy. The lock file used for destination coordination remains after completion. Saved logs and lock files remain on the user's filesystem until removed.

`--stream` is limited to launched Xcode build products; streamed Xcode output is subject to Xcode redaction. Distill does not provide a raw-output escape hatch.

## User choices

Use `--plan` to inspect a planned invocation without running the wrapped tool. Avoid saving or sharing output that contains sensitive project details, and inspect any saved log before sending it to maintainers.