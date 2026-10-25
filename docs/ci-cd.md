# CI/CD

Two GitHub Actions workflows live in `.github/workflows`. Dependabot (`.github/dependabot.yml`) opens weekly
update pull requests for every ecosystem.

## CI (`ci.yml`)

Runs on every pull request, on pushes to `main`, and on demand.

| Job | Runner | Steps |
| --- | --- | --- |
| Version consistency | Ubuntu | `scripts/check-version.sh`: every manifest matches `VERSION` |
| Relay (Go) | Ubuntu | `gofmt`, `go vet`, `go test -race`, build, Docker image build |
| Desktop + core engine (Rust) | Windows | `cargo fmt --check`, `cargo clippy -D warnings`, tests, build of all three editions |
| Desktop + core engine (Rust, Linux) | Ubuntu 22.04 | `cargo fmt --check`, `cargo clippy -D warnings` for every edition, tests under Xvfb including the live X11 capture, XTEST input, clipboard and OpenH264 tests, the three `.deb` packages |
| Android (Kotlin) | Ubuntu, JDK 17 | unit tests, Android lint (report uploaded as an artifact), debug APK |

Run the same checks locally with `make lint` and `make test`.

## Releases (`release.yml`)

1. Bump `VERSION` and the versions it is checked against (`client-desktop/Cargo.toml`,
   `core-engine/Cargo.toml`, `server-relay/internal/version/version.go`, `installer/opendesk.iss`). Android reads
   `VERSION` directly. `scripts/check-version.sh` lists anything you missed.
2. Merge to `main`, then tag and push: `git tag v1.0.0 && git push origin v1.0.0`.

The workflow then:

| Job | Output |
| --- | --- |
| Verify tag | Fails unless the tag equals `VERSION` and every manifest |
| Relay image | `ghcr.io/<owner>/opendesk-relay:<version>`, `:<major>.<minor>` and `:latest` for `linux/amd64` and `linux/arm64`, pushed with the workflow's `GITHUB_TOKEN` |
| Windows | `OpenDesk`, `OpenDesk-Admin`, `OpenDesk-Host` executables and their installers |
| Linux | `opendesk`, `opendesk-admin`, `opendesk-host` `.deb` packages and `opendesk-<version>-linux-x86_64.tar.gz`, built on Ubuntu 22.04 (glibc 2.35+) |
| Android | `OpenDesk-<version>.apk` and `OpenDesk-<version>.aab` for Google Play (signed when the signing secrets exist, otherwise `-unsigned`) |
| GitHub Release | All of the above plus `SHA256SUMS.txt`, with generated release notes |

### Secrets

| Secret | Needed for |
| --- | --- |
| `GITHUB_TOKEN` | Provided automatically; publishes the image and the release. |
| `ANDROID_KEYSTORE_BASE64` | Base64 of the release keystore (`base64 -w0 release.jks`). Optional. |
| `ANDROID_KEYSTORE_PASSWORD`, `ANDROID_KEY_ALIAS`, `ANDROID_KEY_PASSWORD` | Signing the APK and App Bundle. Optional. Locally the same names also work as Gradle properties; see [client-android/README.md](../client-android/README.md). |

Windows binaries are not code-signed yet; SmartScreen will warn on first run until they are. The Linux packages
are unsigned `.deb` files (no APT repository yet); verify them against `SHA256SUMS.txt`.

### GHCR visibility

A newly published package is private. To let servers pull without logging in, open the package on GitHub
(Profile > Packages > `opendesk-relay` > Package settings) and set its visibility to public, or run
`docker login ghcr.io` on the server with a token that has `read:packages`.
