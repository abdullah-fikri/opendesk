# Security policy

OpenDesk controls computers remotely, so a security bug can hand a stranger someone's desktop. Thank you for
reporting one privately first.

## Reporting a vulnerability

Report it through GitHub's private vulnerability reporting:
**[Security → Report a vulnerability](https://github.com/abdullah-fikri/opendesk/security/advisories/new)**.

Please do not open a public issue, pull request or discussion for it.

Include what you can of:

- the component (relay, Windows or Linux client, Android app, installer, auto-update) and its version
  (`opendesk version`, or the relay's `/` endpoint);
- what an attacker needs (network position, a device ID, a registered device, an administrator account...);
- steps to reproduce, or a proof of concept;
- the impact as you see it.

## What happens next

| Step | Target |
| --- | --- |
| Acknowledgement of your report | 3 working days |
| First assessment (confirmed or not, severity) | 10 working days |
| Fix released for critical and high severity | 30 days after confirmation |

We keep you informed, agree on a disclosure date with you, and credit you in the advisory and the changelog
unless you prefer otherwise. A CVE is requested for confirmed vulnerabilities of medium severity or above.

## Supported versions

Only the latest release receives security fixes. Windows installs update themselves through the signed
auto-update channel ([docs/auto-update.md](docs/auto-update.md)); Linux packages and the relay image must be
updated by their operator.

| Version | Supported |
| --- | --- |
| Latest release | Yes |
| Anything older | No: update first |

## Scope

In scope: everything in this repository, including the relay protocol, the end-to-end encryption, file
transfer, the Windows service and installer, and the release and auto-update pipeline.

Known, documented behavior is not a vulnerability by itself (see
[docs/architecture.md](docs/architecture.md#security-model)):

- an **open relay** (no `OPENDESK_DEVICE_CREDENTIALS` and no enforced allowlist) admits every device;
- **desktop hosts are unattended** and accept every session the relay grants;
- the relay sees file names, clipboard text and session metadata.

Reports that bypass these boundaries are in scope: for example gaining administrator rights, controlling a host,
or reading end-to-end encrypted content on a relay that is configured to require credentials.

Out of scope: denial of service by sheer traffic volume, findings that need an already compromised device or
administrator account, and missing hardening headers on the relay's informational HTTP endpoints.

## Safe harbor

Good-faith research that respects this policy, only uses relays and devices you own or are allowed to test,
and does not access, change or keep other people's data is welcome; we will not pursue legal action against it.
