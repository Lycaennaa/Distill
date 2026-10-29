# Output contract

Normal output is line-oriented. Wrapper records use `KIND:`; diagnostics use the compact `DIAG:` prefix.

```text
PLAN: payload
DIAG: payload
INFO: kind=binary state=unresolved
```

Payload control characters and backslashes are escaped. Record kinds: `PLAN`, `DIAG`, `TEST`, `LIST`, `ACTION`, `INFO`, `RESULT`, `FAILURE`, `FALLBACK`, and `LOG`.

Distill parses compiler, lint, XCTest, Cargo, and Gradle output into deterministic records. Swift and Cargo test summaries report executed, passed, failed, and skipped counts; Gradle summaries use standard JUnit XML reports for test tasks observed during that invocation. Failed Gradle cases emit `TEST` records with the task, test name, and JUnit reason; up to 100 failure details are emitted. The Gradle summary is omitted rather than partial if collection exceeds 4,096 test tasks, 4,096 XML files, 64 MiB per file, or 256 MiB total. Failed Gradle tasks emit a `DIAG` record with the task and the captured `What went wrong` reason when available. Test summaries omit successful case details.

- `INFO` omits unavailable metadata and the discovered root. `planned/unresolved` marks a dry run; `unresolved` means product metadata is unknown. Resolved info omits `state`.
- Child output is never copied raw to stdout.
- `RESULT` reports exit code, owner, duration, and lower-precedence statuses. Success omits `class=success`; failures include their class.
- A failing command without recognized diagnostics emits `FALLBACK` with up to 20 sanitized lines.
- SwiftLint runs with more than 50 diagnostics emit counts by severity and rule, plus up to three example locations per group. `--save-log` retains captured child output subject to redaction.
- Other parsed diagnostics are not capped.
- `--stream` sends launched-app output to stderr; digest records remain on stdout.
- Paths inside the command root are relative; paths outside it remain absolute.

## Result classes and exit codes

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
