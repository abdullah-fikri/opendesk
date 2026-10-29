# Auto-update and rollback (Windows)

The OpenDesk service (`OpenDeskService`, LocalSystem) updates the installed edition from signed release
manifests and goes back to the previous version when the new one does not come up. Code:
`client-desktop/src/service/update/`. Linux packages are updated by the package manager.

## Release side

Every release publishes, next to the installers, one manifest per edition and its detached signature:

| Edition | Manifest | Installer |
| --- | --- | --- |
| Standard | `manifest.json` | `OpenDesk-Setup-<v>.exe` |
| Admin | `manifest-admin.json` | `OpenDesk-Admin-Setup-<v>.exe` |
| Host | `manifest-host.json` | `OpenDesk-Host-Setup-<v>.exe` |

```json
{"version":"1.1.0","channel":"stable","min_version":"0.0.0","rollout_percent":100,
 "installer_url":"https://github.com/.../releases/download/v1.1.0/OpenDesk-Setup-1.1.0.exe",
 "sha256":"<hex>","size":12345678}
```

`<manifest>.sig` is the hex ed25519 signature over the exact bytes of the manifest. `scripts/sign-manifest.sh`
writes both; the release workflow runs it when the `UPDATE_SIGNING_KEY` secret exists (otherwise the step is
skipped and devices see no update). Repository variables `UPDATE_ROLLOUT_PERCENT` (default 100) and
`UPDATE_MIN_VERSION` (default `0.0.0`) fill the matching fields.

- `rollout_percent`: a device takes the release when `SHA-256(desk ID) mod 100 < rollout_percent`; raise it in
  later releases (or re-sign the manifest of the same release) to widen the rollout.
- `min_version`: the oldest version this release may be rolled back to (raise it when a release writes data
  that older builds cannot read). A manual rollback below it needs `--force`.

### Signing key

1. `openssl genpkey -algorithm ed25519 -out release-update.pem` on an offline machine.
2. Store the whole PEM file as the `UPDATE_SIGNING_KEY` repository secret. Never commit it.
3. `openssl pkey -in release-update.pem -pubout -outform DER | tail -c 32 | od -An -v -tx1 | tr -d ' \n'`
   and put the 64 hex digits in `RELEASE_PUBLIC_KEY` (`client-desktop/src/service/update/manifest.rs`).

The key is set (since 1.2.0); a build with the all-zero placeholder would reject every manifest. Releases
signed by CI need the `UPDATE_SIGNING_KEY` secret; without it the step is skipped and devices see no update.
Keep an offline backup of the PEM: losing it means installed devices can never be updated automatically again.
Rotating the key means shipping a build with the new public key, signed with the old one, first.

## Device side

All state lives in `%ProgramData%\OpenDesk\updates`, readable and writable by SYSTEM and Administrators only
(a planted folder with another owner is emptied). The layout is described in `store.rs`.

1. Check: 10 minutes after the service starts, then every 6 hours, the service fetches the manifest of its
   edition and its `.sig` over HTTPS. Only a manifest that verifies is read. No downgrade, versions marked bad
   and a pin are respected (see `manifest::decide`).
2. Download: before anything else the installer of the running version is kept for a rollback (from the
   release's own manifest, `.../download/v<current>/manifest.json`); without it the update does not happen.
   Then the new installer is downloaded and checked against `size` and `sha256`. Installers are checked again
   right before they run.
3. Install: only while the agent reports no hosted or viewer session (or no user is logged on). The service
   copies `config.json`, `history.json` and device identity files of the user to `backup-<current>\`, writes
   `pending_health.json`, stops the agent and runs
   `setup.exe /VERYSILENT /SUPPRESSMSGBOXES /NORESTART /LOG=logs\install-<v>.log`. The installer stops and
   replaces the service.
4. Health: the new service waits for the agent to write `healthy` (its version, once registered with the
   relay). The 5-minute window starts with the first agent start, so a machine nobody is logged on to is not
   rolled back. If the window passes, or the agent exits right after start 3 times, the service restores the
   user files, marks the version bad (never offered again automatically) and runs the previous installer.

The installers of the two newest installed versions and their backups are kept.

### Configuration

| Where | Effect |
| --- | --- |
| `config.json` `"auto_update": false` | No automatic checks for this user's machine |
| `OPENDESK_AUTO_UPDATE=0` (machine or user) | Same |
| `config.json` `"update_url"` / `OPENDESK_UPDATE_URL` | Manifest URL instead of the GitHub release. A mirror should keep the `.../latest/download/<manifest>` and `.../download/v<v>/<manifest>` layout so the running version's installer can be found |

A non-administrator can point `update_url` anywhere, but only manifests signed with the release key are used.

### Commands (elevated terminal)

| Command | Effect |
| --- | --- |
| `opendesk update` / `opendesk update latest` | Unpin and check now, ignoring the rollout and bad marks; installs when idle |
| `opendesk update 1.2.3 [--force]` | Fetch that release's signed manifest (`.../download/v1.2.3/`), install it when idle (newer or older) and pin it; below `min_version` needs `--force` |
| `opendesk update status` | Installed, pinned, kept and bad versions, and the update in progress |
| `opendesk rollback [version] [--force]` | Reinstall the previous kept version (or `version`) now and mark the current one bad |
| `opendesk pin 1.2.3` / `opendesk unpin` | Install only that version / follow releases again |

The older spellings `--update-now`, `--rollback`, `--pin-version` and `--unpin` still work. The commands are
queued in `requests\` and carried out by the service within seconds; results are in the service's log
(`opendesk.trace` of the SYSTEM profile).

## Compatibility rules

- The installer runs over a newer install (no version check, `ignoreversion`); keep it that way.
- Settings and update files only gain optional fields (`#[serde(default)]`); fields are never removed or
  renamed, and unknown fields are ignored, so an older version reads what a newer one wrote.
