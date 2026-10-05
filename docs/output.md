# Output contract

Normal output is line-oriented. Wrapper records use `KIND:`; diagnostics use the compact `DIAG:` prefix.

```text
PLAN: payload
DIAG: payload
INFO: kind=binary state=unresolved
```

Payload control characters and backslashes are escaped. Record kinds: `PLAN`, `DIAG`, `TEST`, `LIST`, `ACTION`, `INFO`, `RESULT`, `FAILURE`, `FALLBACK`, and `LOG`.

Distill parses compiler, lint, XCTest, and Cargo output into deterministic records. Cargo test summaries include pass/fail/skipped counts; recognized test failures include failed cases. `xcode list` reports projects/workspaces, targets, configurations, and schemes. Test summaries omit successful case details.

- `INFO` omits unavailable metadata and the discovered root. `planned/unresolved` marks a dry run; `unresolved` means product metadata is unknown. Resolved info omits `state`.
- Child output is never copied raw to stdout.
- `RESULT` reports exit code, owner, duration, and lower-precedence statuses. Success omits `class=success`; failures include their class.
- A failing command without recognized diagnostics emits `FALLBACK` with up to 20 sanitized lines. Parsed diagnostics are not capped.
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
