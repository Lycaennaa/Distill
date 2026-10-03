# Support

Distill supports macOS only. Use the repository's GitHub Issues for reproducible bugs and feature requests. Use Discussions for questions if enabled; otherwise open a concise issue without sensitive details.

For bug reports, include:

- Distill version (`distill --version`).
- macOS version and relevant tool versions (`xcodebuild -version`, `swift --version`, or `cargo --version`).
- A minimal reproduction, expected result, and actual result.
- Relevant sanitized output and whether the command used `--plan`, `--save-log`, or `--stream`.

Do not attach raw build output or saved logs without reviewing them. They can contain private paths, project details, or secrets that sanitization did not recognize. Report vulnerabilities privately as described in `SECURITY.md`.