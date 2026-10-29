# Maintainers

| Name | GitHub | Areas |
| --- | --- | --- |
| Abdullah Fikri | [@abdullah-fikri](https://github.com/abdullah-fikri) | Everything (lead maintainer) |

OpenDesk has one maintainer today. This file records what someone else would need to keep the project going, so
the project does not depend on one person's memory.

## Becoming a maintainer

Contributors with several merged pull requests in an area, who review others' changes and follow
[CONTRIBUTING.md](CONTRIBUTING.md), can be invited as maintainers of that area. Maintainers get write access,
review pull requests in their area and are listed above.

## Credentials and where they live

Each of these must have **an offline backup and at least one other trusted holder** (a sealed copy, a password
manager shared vault, or a second maintainer). Losing one is expensive; the table says how much.

| Credential | Used for | Stored in | If lost |
| --- | --- | --- | --- |
| Update signing key (ed25519 PEM) | Signs auto-update manifests ([docs/auto-update.md](docs/auto-update.md)) | `UPDATE_SIGNING_KEY` repository secret + offline backup | Installed Windows devices can never update themselves again; every user must reinstall manually |
| Android keystore | Signs the APK/AAB ([client-android/README.md](client-android/README.md)) | `ANDROID_KEYSTORE_*` repository secrets + offline backup | Users must uninstall and reinstall; a Play listing needs Play App Signing recovery |
| GitHub organization/repository owner | Releases, secrets, security advisories | GitHub account with 2FA and recovery codes | Repository and releases cannot be managed |
| Container registry (ghcr.io) | Relay image | GitHub account | New relay images cannot be published |
| Domain / relay used for testing | Development | Hosting provider account | Development relay unavailable |

A leaked signing key must be rotated: ship a build that embeds the new public key, signed with the old key,
then revoke the old one (see the rotation note in [docs/auto-update.md](docs/auto-update.md)).

## Release runbook

Releases are cut by pushing a `vX.Y.Z` tag; the whole procedure, including the version check and the CHANGELOG
section, is in [docs/ci-cd.md](docs/ci-cd.md). Anyone with write access and the secrets above can release.

## If the maintainer is unavailable

If the lead maintainer has not responded to issues, pull requests or security reports for 90 days, the listed
maintainers (or, while there are none, the most active contributors) may open an issue titled
"Maintainer succession" and, after 30 more days without an answer, ask GitHub to transfer the repository under
its [deceased or inactive user policy](https://docs.github.com/en/site-policy/other-site-policies/github-deceased-user-policy).
The lead maintainer can name a successor in advance by adding them to this file.
