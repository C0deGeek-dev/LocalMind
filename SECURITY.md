# Security Policy

## Supported Versions

Security fixes land on the latest release train. Earlier trains are not
maintained — upgrade to the current minor.

| Version | Supported |
| --- | --- |
| 1.2.x   | ✅        |
| < 1.2   | ❌        |

## Reporting security issues

Until a public security contact exists, report issues privately to the
repository owner rather than opening a public issue. Do not disclose a
vulnerability publicly before a fix is available.

## Security scope

Security-sensitive areas of LocalMind: secret redaction on import, the on-disk memory store, the human review gate before durable memory, and portable-knowledge bundle import/export.
