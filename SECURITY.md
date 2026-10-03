# Security Policy

## Reporting a vulnerability

Please report suspected vulnerabilities privately through GitHub's Security Advisories feature for this repository (Security → Advisories → Report a vulnerability), if enabled. Do not post exploit details in a public issue. If private reporting is unavailable, open an issue asking the maintainers for a private contact without including sensitive details.

Include the affected Distill version, macOS version, relevant tool versions, impact, and a minimal reproduction. Redact credentials, signing information, private paths, and customer data.

## Security considerations

Distill launches the selected tool directly rather than through a shell, but it is not a sandbox. Wrapped build and test tools run with the caller's permissions and may execute project scripts or access files and networks themselves.

Child diagnostics, fallback output, and saved logs are untrusted. Xcode redaction is mandatory for documented signing/profile/team/email patterns, but it is not a general secret scanner. Non-Xcode sanitization is best effort and may miss sensitive text. Review logs before sharing them; local paths can remain visible.

Distill itself makes no network requests. Cargo installation and wrapped tools may use the network.